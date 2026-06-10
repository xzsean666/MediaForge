use bytes::Bytes;
use mediaforge_config::AppConfig;
use mediaforge_core::{
    cover_object_key, hls_master_playlist_key, image_result_object_key, media_prefix,
    result_id_from_parameters, screenshot_object_key, video_result_object_key, CoreError,
};
use mediaforge_image::{ImageProcessingError, ImageProcessor};
use mediaforge_ingest::{file_sha256_and_size, IngestError};
use mediaforge_manifest::{ManifestError, ManifestRepository};
use mediaforge_storage::{create_object_storage, DynObjectStorage, PutObjectRequest, StorageError};
use mediaforge_tasks::{TaskError, TaskRepository};
use mediaforge_types::{
    DerivedMediaKind, DerivedMediaObject, ImageProcessingRequest, MediaObject, ResourceId,
    TaskDescriptor, TaskId, TaskOperation, TaskState, VideoProcessingRequest,
};
use mediaforge_video::{FfmpegProcessor, VideoProcessingError};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use tokio::time::sleep;
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum WorkerError {
    #[error(transparent)]
    Core(#[from] CoreError),
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error(transparent)]
    Manifest(#[from] ManifestError),
    #[error(transparent)]
    Task(#[from] TaskError),
    #[error(transparent)]
    Image(#[from] ImageProcessingError),
    #[error(transparent)]
    Ingest(#[from] IngestError),
    #[error(transparent)]
    Video(#[from] VideoProcessingError),
    #[error("worker IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("task descriptor is missing: {0}")]
    MissingTask(TaskId),
}

pub type WorkerResult<T> = Result<T, WorkerError>;

pub async fn run_worker_loop(config: AppConfig) -> WorkerResult<()> {
    let storage = create_object_storage(&config).await?;
    let runtime = WorkerRuntime::new(config, storage);
    let worker_id = format!("worker-{}", Uuid::new_v4());

    loop {
        let processed_count = runtime.run_once(&worker_id).await?;
        if processed_count == 0 {
            sleep(runtime.config.worker_poll_interval).await;
        }
    }
}

#[derive(Clone)]
pub struct WorkerRuntime {
    config: AppConfig,
    storage: DynObjectStorage,
    manifests: ManifestRepository,
    tasks: TaskRepository,
    video_processor: FfmpegProcessor,
}

impl WorkerRuntime {
    pub fn new(config: AppConfig, storage: DynObjectStorage) -> Self {
        Self {
            video_processor: FfmpegProcessor::new(
                config.ffmpeg_path.clone(),
                config.ffprobe_path.clone(),
            ),
            manifests: ManifestRepository::new(storage.clone()),
            tasks: TaskRepository::new(storage.clone()),
            storage,
            config,
        }
    }

    pub async fn run_once(&self, worker_id: &str) -> WorkerResult<usize> {
        let pending_tasks = self.tasks.list_pending().await?;
        let mut processed_count = 0;

        for task in pending_tasks {
            if processed_count >= self.config.worker_concurrency {
                break;
            }

            let status = self.tasks.read_status(&task.task_id).await?;
            if !matches!(status.state, TaskState::Pending) {
                continue;
            }

            if !self.tasks.claim_task(&task.task_id, worker_id).await? {
                continue;
            }

            processed_count += 1;
            if let Err(error) = self.process_task(&task).await {
                tracing::error!(task_id = %task.task_id, error = %error, "task failed");
                let _ = self
                    .tasks
                    .mark_failed(&task.task_id, error.to_string())
                    .await;
            }
        }

        Ok(processed_count)
    }

    async fn process_task(&self, task: &TaskDescriptor) -> WorkerResult<()> {
        self.tasks.mark_running(&task.task_id).await?;

        match &task.operation {
            TaskOperation::ImageProcessing { request } => {
                self.process_image_task(&task.resource_id, request).await?;
            }
            TaskOperation::VideoProcessing { request } => {
                self.process_video_task(&task.resource_id, request).await?;
            }
        }

        self.tasks
            .mark_completed(&task.task_id, Some("completed".to_string()))
            .await?;
        Ok(())
    }

    async fn process_image_task(
        &self,
        resource_id: &ResourceId,
        request: &ImageProcessingRequest,
    ) -> WorkerResult<()> {
        let manifest = self.manifests.read(resource_id).await?;
        let source_path = self
            .download_to_temp(&manifest.original.object_key, "source")
            .await?;
        let result_id = result_id_from_parameters(resource_id, "image_processing", request)?;
        let output_path = self.config.temp_directory.join(format!(
            "image-{}.{}",
            Uuid::new_v4(),
            request.output_format.extension()
        ));

        let processed = ImageProcessor::process_file(&source_path, &output_path, request)?;
        let object_key =
            image_result_object_key(resource_id, &result_id, request.output_format.extension())?;
        let media_object = self
            .upload_output_file(
                &object_key,
                &output_path,
                request.output_format.mime_type(),
                processed.metadata,
            )
            .await?;

        self.manifests
            .merge_derived(
                resource_id,
                DerivedMediaObject {
                    result_id,
                    derived_kind: DerivedMediaKind::ImageVariant,
                    object: media_object,
                    parameters: serde_json::to_value(request).unwrap_or(serde_json::Value::Null),
                    created_at: chrono::Utc::now(),
                },
            )
            .await?;

        remove_temp_file(source_path).await;
        remove_temp_file(output_path).await;
        Ok(())
    }

    async fn process_video_task(
        &self,
        resource_id: &ResourceId,
        request: &VideoProcessingRequest,
    ) -> WorkerResult<()> {
        let manifest = self.manifests.read(resource_id).await?;
        let source_path = self
            .download_to_temp(&manifest.original.object_key, "source")
            .await?;
        let result_id = result_id_from_parameters(resource_id, "video_processing", request)?;
        let working_directory = self
            .config
            .temp_directory
            .join(format!("video-task-{}", Uuid::new_v4()));
        tokio::fs::create_dir_all(&working_directory).await?;

        let output_path =
            working_directory.join(format!("output.{}", request.profile.container.extension()));
        self.video_processor
            .transcode(&source_path, &output_path, &request.profile)
            .await?;
        let metadata = self
            .video_processor
            .probe_metadata(&output_path)
            .await
            .unwrap_or_default();
        let object_key = video_result_object_key(
            resource_id,
            &result_id,
            request.profile.container.extension(),
        )?;
        let media_object = self
            .upload_output_file(
                &object_key,
                &output_path,
                request.profile.container.mime_type(),
                metadata,
            )
            .await?;
        self.merge_video_derived(
            resource_id,
            result_id.clone(),
            DerivedMediaKind::VideoVariant,
            media_object,
            request,
        )
        .await?;

        if request.generate_cover {
            let cover_path = working_directory.join("cover.jpg");
            self.video_processor
                .screenshot(
                    &source_path,
                    &cover_path,
                    1.0,
                    mediaforge_types::ImageOutputFormat::Jpeg,
                )
                .await?;
            let object_key = cover_object_key(resource_id, &result_id, "jpg")?;
            let media_object = self
                .upload_output_file(
                    &object_key,
                    &cover_path,
                    "image/jpeg",
                    mediaforge_types::MediaMetadata::default(),
                )
                .await?;
            self.merge_video_derived(
                resource_id,
                result_id.clone(),
                DerivedMediaKind::Cover,
                media_object,
                request,
            )
            .await?;
        }

        for (index, screenshot) in request.screenshots.iter().enumerate() {
            let screenshot_path = working_directory.join(format!(
                "screenshot-{index:06}.{}",
                screenshot.output_format.extension()
            ));
            self.video_processor
                .screenshot(
                    &source_path,
                    &screenshot_path,
                    screenshot.timestamp_seconds,
                    screenshot.output_format,
                )
                .await?;
            let object_key = screenshot_object_key(
                resource_id,
                &result_id,
                index,
                screenshot.output_format.extension(),
            )?;
            let media_object = self
                .upload_output_file(
                    &object_key,
                    &screenshot_path,
                    screenshot.output_format.mime_type(),
                    mediaforge_types::MediaMetadata::default(),
                )
                .await?;
            self.merge_video_derived(
                resource_id,
                result_id.clone(),
                DerivedMediaKind::Screenshot,
                media_object,
                request,
            )
            .await?;
        }

        if request.generate_hls {
            let hls_directory = working_directory.join("hls");
            let playlist = self
                .video_processor
                .generate_hls(&source_path, &hls_directory)
                .await?;
            self.upload_hls_directory(resource_id, &result_id, &hls_directory, &playlist, request)
                .await?;
        }

        remove_temp_file(source_path).await;
        let _ = tokio::fs::remove_dir_all(working_directory).await;
        Ok(())
    }

    async fn merge_video_derived(
        &self,
        resource_id: &ResourceId,
        result_id: mediaforge_types::ResultId,
        derived_kind: DerivedMediaKind,
        media_object: MediaObject,
        request: &VideoProcessingRequest,
    ) -> WorkerResult<()> {
        self.manifests
            .merge_derived(
                resource_id,
                DerivedMediaObject {
                    result_id,
                    derived_kind,
                    object: media_object,
                    parameters: serde_json::to_value(request).unwrap_or(serde_json::Value::Null),
                    created_at: chrono::Utc::now(),
                },
            )
            .await?;
        Ok(())
    }

    async fn upload_hls_directory(
        &self,
        resource_id: &ResourceId,
        result_id: &mediaforge_types::ResultId,
        hls_directory: &Path,
        playlist_path: &Path,
        request: &VideoProcessingRequest,
    ) -> WorkerResult<()> {
        let mut entries = tokio::fs::read_dir(hls_directory).await?;
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if !entry.metadata().await?.is_file() {
                continue;
            }
            let file_name = path
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("segment.ts");
            let object_key = if path == playlist_path {
                hls_master_playlist_key(resource_id, result_id)?
            } else {
                format!(
                    "{}/hls/{}/{}",
                    media_prefix(resource_id)?,
                    result_id.as_ref(),
                    file_name
                )
            };
            let content_type = if file_name.ends_with(".m3u8") {
                "application/vnd.apple.mpegurl"
            } else {
                "video/mp2t"
            };
            let media_object = self
                .upload_output_file(
                    &object_key,
                    &path,
                    content_type,
                    mediaforge_types::MediaMetadata::default(),
                )
                .await?;
            self.merge_video_derived(
                resource_id,
                result_id.clone(),
                if file_name.ends_with(".m3u8") {
                    DerivedMediaKind::HlsPlaylist
                } else {
                    DerivedMediaKind::HlsSegment
                },
                media_object,
                request,
            )
            .await?;
        }
        Ok(())
    }

    async fn download_to_temp(&self, object_key: &str, label: &str) -> WorkerResult<PathBuf> {
        tokio::fs::create_dir_all(&self.config.temp_directory).await?;
        let extension = Path::new(object_key)
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or("bin");
        let path =
            self.config
                .temp_directory
                .join(format!("{label}-{}.{}", Uuid::new_v4(), extension));
        let bytes = self.storage.get_object(object_key).await?;
        tokio::fs::write(&path, bytes).await?;
        Ok(path)
    }

    async fn upload_output_file(
        &self,
        object_key: &str,
        path: &Path,
        mime_type: &str,
        metadata: mediaforge_types::MediaMetadata,
    ) -> WorkerResult<MediaObject> {
        let bytes = tokio::fs::read(path).await?;
        self.storage
            .put_object_if_absent(PutObjectRequest {
                key: object_key.to_string(),
                bytes: Bytes::from(bytes),
                content_type: Some(mime_type.to_string()),
                metadata: BTreeMap::new(),
            })
            .await?;
        let (checksum_sha256, size_bytes) = file_sha256_and_size(path).await?;

        Ok(MediaObject {
            object_key: object_key.to_string(),
            file_name: Path::new(object_key)
                .file_name()
                .and_then(|value| value.to_str())
                .map(ToString::to_string),
            mime_type: mime_type.to_string(),
            size_bytes,
            checksum_sha256,
            metadata,
        })
    }
}

async fn remove_temp_file(path: PathBuf) {
    let _ = tokio::fs::remove_file(path).await;
}
