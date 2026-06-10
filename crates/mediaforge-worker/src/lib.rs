use bytes::Bytes;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use mediaforge_config::AppConfig;
use mediaforge_core::{
    cover_object_key, hls_master_playlist_key, image_result_object_key, media_prefix,
    original_object_key, result_id_from_parameters, screenshot_object_key, source_expiry_key,
    source_expiry_prefix, source_expiry_timestamp_from_key, video_result_object_key, CoreError,
};
use mediaforge_image::{ImageProcessingError, ImageProcessor};
use mediaforge_ingest::{inspect_file, IngestError};
use mediaforge_manifest::{ManifestError, ManifestRepository};
use mediaforge_storage::{create_object_storage, DynObjectStorage, PutObjectRequest, StorageError};
use mediaforge_tasks::{TaskError, TaskRepository};
use mediaforge_types::{
    DerivedMediaKind, DerivedMediaObject, ImageProcessingRequest, MediaKind, MediaMetadata,
    MediaObject, ResourceId, TaskDescriptor, TaskId, TaskOperation, TaskState,
    VideoProcessingRequest,
};
use mediaforge_uploads::{UploadSession, UploadSessionRepository, UploadState};
use mediaforge_video::{FfmpegProcessor, VideoProcessingError};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use tokio::task::JoinSet;
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
    Upload(#[from] mediaforge_uploads::UploadError),
    #[error(transparent)]
    Image(#[from] ImageProcessingError),
    #[error(transparent)]
    Ingest(#[from] IngestError),
    #[error(transparent)]
    Video(#[from] VideoProcessingError),
    #[error("source content does not match declared media kind: {0}")]
    MediaKindMismatch(String),
    #[error("failed to serialize worker data: {0}")]
    Serialize(#[from] serde_json::Error),
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

pub async fn process_single_task(
    config: AppConfig,
    task_id: TaskId,
    worker_id: String,
) -> WorkerResult<()> {
    let storage = create_object_storage(&config).await?;
    let runtime = WorkerRuntime::new(config, storage);
    runtime.process_task_by_id(&task_id, &worker_id).await
}

/// Source-file expiry marker written when an upload requests a TTL. The reaper
/// lists these by time-ordered key and deletes the original once due.
#[derive(Debug, Serialize, Deserialize)]
struct SourceExpiry {
    resource_id: ResourceId,
    object_key: String,
    expires_at: DateTime<Utc>,
}

#[derive(Clone)]
pub struct WorkerRuntime {
    config: AppConfig,
    storage: DynObjectStorage,
    manifests: ManifestRepository,
    tasks: TaskRepository,
    uploads: UploadSessionRepository,
    video_processor: FfmpegProcessor,
}

impl WorkerRuntime {
    pub fn new(config: AppConfig, storage: DynObjectStorage) -> Self {
        Self {
            video_processor: FfmpegProcessor::new(
                config.ffmpeg_path.clone(),
                config.ffprobe_path.clone(),
                config.ffmpeg_threads,
            ),
            manifests: ManifestRepository::new(storage.clone()),
            tasks: TaskRepository::new(storage.clone()),
            uploads: UploadSessionRepository::new(storage.clone()),
            storage,
            config,
        }
    }

    /// One scheduler tick: finalize uploaded sources, process media tasks, and
    /// reap expired source files. Returns how many units of work were started.
    pub async fn run_once(&self, worker_id: &str) -> WorkerResult<usize> {
        let mut processed = self.run_finalize_round().await?;
        processed += self.run_task_round(worker_id).await?;

        if self.config.source_reaper_enabled {
            if let Err(error) = self.reap_expired_sources().await {
                tracing::warn!(error = %error, "source reaper failed");
            }
        }

        Ok(processed)
    }

    // ---- Upload finalization -------------------------------------------------

    /// Finalizes up to `worker_concurrency` queued uploads concurrently.
    async fn run_finalize_round(&self) -> WorkerResult<usize> {
        let pending = self.uploads.list_pending().await?;
        let mut join_set = JoinSet::new();
        let mut started = 0;

        for upload_id in pending {
            if started >= self.config.worker_concurrency {
                break;
            }
            started += 1;
            let runtime = self.clone();
            join_set.spawn(async move {
                if let Err(error) = runtime.finalize_upload(&upload_id).await {
                    tracing::error!(upload_id = %upload_id, error = %error, "upload finalization failed");
                    let terminal = true; // finalization failures are not auto-retried for now
                    let _ = runtime
                        .uploads
                        .mark_failed(&upload_id, error.to_string(), terminal)
                        .await;
                }
            });
        }

        while join_set.join_next().await.is_some() {}
        Ok(started)
    }

    /// Downloads the staged object, content-addresses it, writes the manifest,
    /// schedules processing, and applies any source TTL. Server-side copy moves
    /// the bytes from the staging key to the content-addressed key without
    /// re-uploading.
    async fn finalize_upload(&self, upload_id: &str) -> WorkerResult<()> {
        let session = self.uploads.read(upload_id).await?;
        if session.state != UploadState::Pending {
            return Ok(());
        }

        tokio::fs::create_dir_all(&self.config.temp_directory).await?;
        let temp_path = self
            .config
            .temp_directory
            .join(format!("finalize-{}.bin", Uuid::new_v4()));
        self.storage
            .get_object_to_file(&session.staging_key, &temp_path)
            .await?;

        let result = self.finalize_downloaded(&session, &temp_path).await;
        remove_temp_file(temp_path).await;
        result
    }

    async fn finalize_downloaded(
        &self,
        session: &UploadSession,
        temp_path: &Path,
    ) -> WorkerResult<()> {
        // Hash once (streaming) to derive the content-addressed resource id.
        let inspected = inspect_file(
            temp_path,
            session.file_name.as_deref(),
            session.content_type.clone(),
        )
        .await?;
        ensure_content_matches_kind(temp_path, session.media_kind)?;

        let resource_id = inspected.resource_id.clone();
        let original_key = original_object_key(&resource_id, &inspected.extension)?;

        // Move the bytes to their permanent, content-addressed location.
        if !self.storage.object_exists(&original_key).await? {
            self.storage
                .copy_object(&session.staging_key, &original_key)
                .await?;
        }

        let metadata = match session.media_kind {
            MediaKind::Image => {
                ImageProcessor::inspect_image(temp_path).unwrap_or_else(|_| MediaMetadata::default())
            }
            MediaKind::Video => MediaMetadata::default(),
        };
        let original = MediaObject {
            object_key: original_key.clone(),
            file_name: session.file_name.clone(),
            mime_type: inspected.mime_type,
            size_bytes: inspected.size_bytes,
            checksum_sha256: inspected.checksum_sha256,
            metadata,
        };
        self.manifests
            .create_if_missing(resource_id.clone(), session.media_kind, original)
            .await?;

        // The staging object is no longer needed.
        let _ = self.storage.delete_object(&session.staging_key).await;

        if let Some(ttl_seconds) = session.source_expires_in_seconds {
            self.write_source_expiry(&resource_id, &original_key, ttl_seconds)
                .await?;
        }

        let processing_task_id = self.schedule_processing(&resource_id, session).await?;
        if let Some(task_id) = &processing_task_id {
            self.manifests.record_task(&resource_id, task_id.clone()).await?;
        }

        self.uploads
            .mark_finalized(&session.upload_id, resource_id, processing_task_id)
            .await?;
        Ok(())
    }

    /// Creates the post-upload processing task: always for video (client request
    /// or a sensible default), and for image only when a request was supplied.
    async fn schedule_processing(
        &self,
        resource_id: &ResourceId,
        session: &UploadSession,
    ) -> WorkerResult<Option<TaskId>> {
        let operation = match session.media_kind {
            MediaKind::Video => {
                let request = match &session.processing {
                    Some(value) => serde_json::from_value::<VideoProcessingRequest>(value.clone())?,
                    None => default_video_processing_request(),
                };
                Some(TaskOperation::VideoProcessing { request })
            }
            MediaKind::Image => match &session.processing {
                Some(value) => {
                    let request = serde_json::from_value::<ImageProcessingRequest>(value.clone())?;
                    Some(TaskOperation::ImageProcessing { request })
                }
                None => None,
            },
        };

        match operation {
            Some(operation) => {
                let (task, _) = self.tasks.create_task(resource_id.clone(), operation).await?;
                Ok(Some(task.task_id))
            }
            None => Ok(None),
        }
    }

    async fn write_source_expiry(
        &self,
        resource_id: &ResourceId,
        object_key: &str,
        ttl_seconds: u64,
    ) -> WorkerResult<()> {
        let expires_at = Utc::now() + ChronoDuration::seconds(ttl_seconds as i64);
        let marker = SourceExpiry {
            resource_id: resource_id.clone(),
            object_key: object_key.to_string(),
            expires_at,
        };
        self.put_json(
            source_expiry_key(expires_at.timestamp(), resource_id),
            &marker,
        )
        .await
    }

    // ---- Source reaper -------------------------------------------------------

    /// Deletes original source objects whose TTL has elapsed. Markers are keyed
    /// by a zero-padded timestamp so the listing is time-ordered and the scan
    /// can stop at the first not-yet-due marker.
    async fn reap_expired_sources(&self) -> WorkerResult<usize> {
        let now = Utc::now().timestamp();
        let keys = self.storage.list_keys(&source_expiry_prefix()).await?;
        let mut removed = 0;

        for key in keys {
            let Some(timestamp) = source_expiry_timestamp_from_key(&key) else {
                continue;
            };
            if timestamp > now {
                break; // keys are time-ordered; nothing after this is due yet
            }
            let bytes = self.storage.get_object(&key).await?;
            let marker: SourceExpiry = serde_json::from_slice(&bytes)?;
            let _ = self.storage.delete_object(&marker.object_key).await;
            let _ = self.storage.delete_object(&key).await;
            tracing::info!(resource_id = %marker.resource_id, "expired source deleted");
            removed += 1;
        }

        Ok(removed)
    }

    // ---- Task processing -----------------------------------------------------

    /// Claims and processes up to `worker_concurrency` tasks concurrently,
    /// reclaiming tasks abandoned by crashed workers once their lease expires.
    async fn run_task_round(&self, worker_id: &str) -> WorkerResult<usize> {
        let pending = self.tasks.list_pending().await?;
        let mut join_set = JoinSet::new();
        let mut claimed = 0;

        for descriptor in pending {
            if claimed >= self.config.worker_concurrency {
                break;
            }

            let status = self.tasks.read_status(&descriptor.task_id).await?;
            // Pending tasks are claimable; Leased/Running ones only if their
            // lease has expired (claim_or_reclaim_task enforces the timeout).
            if matches!(status.state, TaskState::Completed | TaskState::Failed) {
                continue;
            }
            if !self
                .tasks
                .claim_or_reclaim_task(&descriptor.task_id, worker_id, self.config.task_lease_timeout)
                .await?
            {
                continue;
            }

            claimed += 1;
            let runtime = self.clone();
            join_set.spawn(async move {
                if let Err(error) = runtime.process_task(&descriptor).await {
                    tracing::error!(task_id = %descriptor.task_id, error = %error, "task failed");
                    let _ = runtime
                        .tasks
                        .mark_failed(
                            &descriptor.task_id,
                            error.to_string(),
                            runtime.config.task_max_attempts,
                        )
                        .await;
                }
            });
        }

        while join_set.join_next().await.is_some() {}
        Ok(claimed)
    }

    pub async fn process_task_by_id(&self, task_id: &TaskId, worker_id: &str) -> WorkerResult<()> {
        let status = self.tasks.read_status(task_id).await?;
        if matches!(status.state, TaskState::Completed) {
            tracing::info!(%task_id, "task already completed");
            return Ok(());
        }

        if matches!(status.state, TaskState::Pending) {
            let claimed = self.tasks.claim_task(task_id, worker_id).await?;
            if !claimed {
                tracing::info!(%task_id, "task already has a lease; processing idempotently");
            }
        }

        let task = self.tasks.read_descriptor(task_id).await?;
        if let Err(error) = self.process_task(&task).await {
            let _ = self
                .tasks
                .mark_failed(task_id, error.to_string(), self.config.task_max_attempts)
                .await;
            return Err(error);
        }
        Ok(())
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

    /// Downloads an object to a temp file by streaming (bounded memory).
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
        self.storage.get_object_to_file(object_key, &path).await?;
        Ok(path)
    }

    /// Uploads a local output file by streaming, then records its checksum/size.
    async fn upload_output_file(
        &self,
        object_key: &str,
        path: &Path,
        mime_type: &str,
        metadata: mediaforge_types::MediaMetadata,
    ) -> WorkerResult<MediaObject> {
        self.storage
            .put_object_streaming(object_key, path, Some(mime_type.to_string()))
            .await?;
        let (checksum_sha256, size_bytes) =
            mediaforge_ingest::file_sha256_and_size(path).await?;

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

    async fn put_json<T>(&self, key: String, value: &T) -> WorkerResult<()>
    where
        T: Serialize,
    {
        self.storage
            .put_object(PutObjectRequest {
                key,
                bytes: Bytes::from(serde_json::to_vec_pretty(value)?),
                content_type: Some("application/json".to_string()),
                metadata: BTreeMap::new(),
            })
            .await?;
        Ok(())
    }
}

fn default_video_processing_request() -> VideoProcessingRequest {
    use mediaforge_types::{
        ImageOutputFormat, ScreenshotRequest, VideoCodec, VideoContainer, VideoProfile,
        VideoResolution,
    };
    VideoProcessingRequest {
        profile: VideoProfile {
            codec: VideoCodec::H264,
            container: VideoContainer::Mp4,
            resolution: Some(VideoResolution::P720),
            crf: Some(23),
            bitrate_kbps: None,
        },
        screenshots: vec![ScreenshotRequest {
            timestamp_seconds: 1.0,
            output_format: ImageOutputFormat::Jpeg,
        }],
        generate_cover: true,
        generate_hls: false,
    }
}

/// Best-effort content sniffing: if the file's magic bytes identify a concrete
/// type that contradicts the declared media kind, reject it. Unknown content is
/// allowed through (validation is "where practical", per the spec).
fn ensure_content_matches_kind(path: &Path, media_kind: MediaKind) -> WorkerResult<()> {
    let detected = match infer::get_from_path(path) {
        Ok(Some(kind)) => kind,
        _ => return Ok(()),
    };

    let matches = match media_kind {
        MediaKind::Image => detected.matcher_type() == infer::MatcherType::Image,
        MediaKind::Video => detected.matcher_type() == infer::MatcherType::Video,
    };
    if matches {
        Ok(())
    } else {
        Err(WorkerError::MediaKindMismatch(format!(
            "declared {media_kind:?} but content looks like {}",
            detected.mime_type()
        )))
    }
}

async fn remove_temp_file(path: PathBuf) {
    let _ = tokio::fs::remove_file(path).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use mediaforge_config::{
        AuthConfig, RuntimeMode, S3Config, StorageBackend, StorageConfig, UploadLimitConfig,
    };
    use mediaforge_storage::FilesystemObjectStorage;
    use mediaforge_uploads::NewUpload;
    use std::sync::Arc;
    use std::time::Duration;

    fn test_config(root: &std::path::Path) -> AppConfig {
        AppConfig {
            runtime_mode: RuntimeMode::Worker,
            http_bind: "127.0.0.1:0".parse().unwrap(),
            storage: StorageConfig {
                backend: StorageBackend::Filesystem,
                s3: S3Config {
                    endpoint: None,
                    region: "auto".to_string(),
                    bucket: "mediaforge".to_string(),
                    access_key_id: None,
                    secret_access_key: None,
                    force_path_style: false,
                },
                filesystem_root: root.to_path_buf(),
            },
            cdn_base_url: None,
            link_signing_secret: "secret".to_string(),
            auth: AuthConfig {
                enabled: false,
                jwt_secret: None,
                leeway_seconds: 30,
            },
            upload_limit: UploadLimitConfig {
                enabled: true,
                max_bytes: 1024 * 1024,
            },
            temp_directory: root.join("temp"),
            ffmpeg_path: "ffmpeg".to_string(),
            ffprobe_path: "ffprobe".to_string(),
            ffmpeg_threads: Some(1),
            worker_concurrency: 2,
            worker_poll_interval: Duration::from_secs(1),
            task_lease_timeout: Duration::from_secs(600),
            task_max_attempts: 3,
            presigned_upload_expiry: Duration::from_secs(3600),
            source_reaper_enabled: true,
            sqlite_cache_path: None,
            log_level: "info".to_string(),
        }
    }

    fn runtime(root: &std::path::Path) -> WorkerRuntime {
        let storage: DynObjectStorage =
            Arc::new(FilesystemObjectStorage::new(root.to_path_buf()));
        WorkerRuntime::new(test_config(root), storage)
    }

    #[tokio::test]
    async fn finalize_content_addresses_uploaded_source() {
        let directory = tempfile::tempdir().unwrap();
        let runtime = runtime(directory.path());

        // Simulate a client's direct-to-storage upload by placing bytes at a
        // staging key, then opening + queueing the matching session.
        let session = runtime
            .uploads
            .create(NewUpload {
                media_kind: MediaKind::Image,
                file_name: Some("photo.bin".to_string()),
                content_type: Some("image/png".to_string()),
                declared_size_bytes: Some(5),
                source_expires_in_seconds: None,
                processing: None,
            })
            .await
            .unwrap();
        runtime
            .storage
            .put_object(PutObjectRequest {
                key: session.staging_key.clone(),
                bytes: Bytes::from_static(b"hello"),
                content_type: None,
                metadata: BTreeMap::new(),
            })
            .await
            .unwrap();
        runtime
            .uploads
            .enqueue_finalization(&session.upload_id)
            .await
            .unwrap();

        let started = runtime.run_finalize_round().await.unwrap();
        assert_eq!(started, 1);

        // The session is finalized with the content-addressed resource id.
        let finalized = runtime.uploads.read(&session.upload_id).await.unwrap();
        assert_eq!(finalized.state, UploadState::Completed);
        let resource_id = finalized.resource_id.expect("resource id assigned");
        assert_eq!(
            resource_id.as_ref(),
            "sha256:2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );

        // The manifest exists and the staging object was cleaned up.
        let manifest = runtime.manifests.read(&resource_id).await.unwrap();
        assert_eq!(manifest.original.size_bytes, 5);
        assert!(!runtime
            .storage
            .object_exists(&session.staging_key)
            .await
            .unwrap());
        // Image upload without a processing request creates no task.
        assert!(finalized.processing_task_id.is_none());
    }

    #[tokio::test]
    async fn reaper_deletes_due_source_only() {
        let directory = tempfile::tempdir().unwrap();
        let runtime = runtime(directory.path());

        let live = mediaforge_core::resource_id_from_bytes(b"live");
        let dead = mediaforge_core::resource_id_from_bytes(b"dead");
        runtime
            .storage
            .put_object(PutObjectRequest {
                key: "media/live/source.bin".to_string(),
                bytes: Bytes::from_static(b"live"),
                content_type: None,
                metadata: BTreeMap::new(),
            })
            .await
            .unwrap();
        runtime
            .storage
            .put_object(PutObjectRequest {
                key: "media/dead/source.bin".to_string(),
                bytes: Bytes::from_static(b"dead"),
                content_type: None,
                metadata: BTreeMap::new(),
            })
            .await
            .unwrap();

        // One marker already due (past), one far in the future.
        let past = SourceExpiry {
            resource_id: dead.clone(),
            object_key: "media/dead/source.bin".to_string(),
            expires_at: Utc::now() - ChronoDuration::seconds(10),
        };
        let future = SourceExpiry {
            resource_id: live.clone(),
            object_key: "media/live/source.bin".to_string(),
            expires_at: Utc::now() + ChronoDuration::seconds(10_000),
        };
        runtime
            .put_json(source_expiry_key(past.expires_at.timestamp(), &dead), &past)
            .await
            .unwrap();
        runtime
            .put_json(
                source_expiry_key(future.expires_at.timestamp(), &live),
                &future,
            )
            .await
            .unwrap();

        let removed = runtime.reap_expired_sources().await.unwrap();
        assert_eq!(removed, 1);
        assert!(!runtime
            .storage
            .object_exists("media/dead/source.bin")
            .await
            .unwrap());
        assert!(runtime
            .storage
            .object_exists("media/live/source.bin")
            .await
            .unwrap());
    }
}
