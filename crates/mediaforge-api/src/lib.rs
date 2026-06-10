use axum::extract::multipart::MultipartError;
use axum::extract::{Multipart, Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use bytes::Bytes;
use mediaforge_config::AppConfig;
use mediaforge_core::{manifest_key, original_object_key, validate_resource_id};
use mediaforge_image::ImageProcessor;
use mediaforge_ingest::{IngestError, IngestSession, IngestedMedia};
use mediaforge_links::{LinkError, LinkGenerator};
use mediaforge_manifest::{ManifestError, ManifestRepository};
use mediaforge_storage::{create_object_storage, DynObjectStorage, PutObjectRequest, StorageError};
use mediaforge_tasks::{TaskError, TaskRepository};
use mediaforge_types::{
    GeneratedLink, ImageOutputFormat, ImageProcessingRequest, LinkPolicy, MediaKind, MediaMetadata,
    MediaObject, ResourceId, ResourceManifest, ScreenshotRequest, TaskId, TaskOperation,
    TaskStatus, UploadResponse, VideoCodec, VideoContainer, VideoProcessingRequest, VideoProfile,
    VideoResolution,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Arc;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;

#[derive(Debug, thiserror::Error)]
pub enum ApiRunError {
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error("failed to bind API server: {0}")]
    Bind(#[from] std::io::Error),
}

#[derive(Clone)]
pub struct AppState {
    config: Arc<AppConfig>,
    storage: DynObjectStorage,
    manifests: ManifestRepository,
    tasks: TaskRepository,
    links: LinkGenerator,
}

impl AppState {
    pub fn new(config: AppConfig, storage: DynObjectStorage) -> Self {
        let manifests = ManifestRepository::new(storage.clone());
        let tasks = TaskRepository::new(storage.clone());
        let links = LinkGenerator::new(&config, storage.clone());
        Self {
            config: Arc::new(config),
            storage,
            manifests,
            tasks,
            links,
        }
    }
}

pub async fn run_api(config: AppConfig) -> Result<(), ApiRunError> {
    let storage = create_object_storage(&config).await?;
    let bind_address = config.http_bind;
    let router = build_router(config, storage);
    let listener = tokio::net::TcpListener::bind(bind_address).await?;
    tracing::info!(%bind_address, "mediaforge api listening");
    axum::serve(listener, router).await?;
    Ok(())
}

pub fn build_router(config: AppConfig, storage: DynObjectStorage) -> Router {
    let state = AppState::new(config, storage);
    Router::new()
        .route("/health", get(health))
        .route("/v1/images/upload", post(upload_image))
        .route("/v1/videos/upload", post(upload_video))
        .route("/v1/resources/{resource_id}", get(get_resource))
        .route("/v1/resources/{resource_id}/image", post(create_image_task))
        .route("/v1/resources/{resource_id}/video", post(create_video_task))
        .route("/v1/resources/{resource_id}/links", post(generate_link))
        .route("/v1/tasks/{task_id}", get(get_task))
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok".to_string(),
    })
}

async fn upload_image(
    State(state): State<AppState>,
    multipart: Multipart,
) -> Result<Json<UploadResponse>, ApiError> {
    let ingested = ingest_file_from_multipart(multipart, &state.config.temp_directory).await?;
    ensure_media_kind(&ingested.mime_type, MediaKind::Image)?;
    let manifest = store_original(&state, &ingested, MediaKind::Image).await?;

    Ok(Json(UploadResponse {
        resource_id: manifest.resource_id.clone(),
        task_id: None,
        manifest,
    }))
}

async fn upload_video(
    State(state): State<AppState>,
    multipart: Multipart,
) -> Result<Json<UploadResponse>, ApiError> {
    let (ingested, processing_request) =
        ingest_video_upload_from_multipart(multipart, &state.config.temp_directory).await?;
    ensure_media_kind(&ingested.mime_type, MediaKind::Video)?;
    let manifest = store_original(&state, &ingested, MediaKind::Video).await?;
    let request = processing_request.unwrap_or_else(default_video_processing_request);
    let (task, _) = state
        .tasks
        .create_task(
            manifest.resource_id.clone(),
            TaskOperation::VideoProcessing { request },
        )
        .await?;
    let manifest = state
        .manifests
        .record_task(&manifest.resource_id, task.task_id.clone())
        .await?;

    Ok(Json(UploadResponse {
        resource_id: manifest.resource_id.clone(),
        task_id: Some(task.task_id),
        manifest,
    }))
}

async fn get_resource(
    State(state): State<AppState>,
    Path(resource_id): Path<String>,
) -> Result<Json<ResourceManifest>, ApiError> {
    let resource_id = ResourceId(resource_id);
    validate_resource_id(&resource_id)?;
    Ok(Json(state.manifests.read(&resource_id).await?))
}

async fn get_task(
    State(state): State<AppState>,
    Path(task_id): Path<String>,
) -> Result<Json<TaskStatus>, ApiError> {
    Ok(Json(state.tasks.read_status(&TaskId(task_id)).await?))
}

async fn create_image_task(
    State(state): State<AppState>,
    Path(resource_id): Path<String>,
    Json(request): Json<ImageProcessingRequest>,
) -> Result<Json<CreateTaskResponse>, ApiError> {
    let resource_id = ResourceId(resource_id);
    validate_resource_id(&resource_id)?;
    state.manifests.read(&resource_id).await?;
    let (task, status) = state
        .tasks
        .create_task(
            resource_id.clone(),
            TaskOperation::ImageProcessing { request },
        )
        .await?;
    state
        .manifests
        .record_task(&resource_id, task.task_id.clone())
        .await?;

    Ok(Json(CreateTaskResponse {
        resource_id,
        task_id: task.task_id,
        status,
    }))
}

async fn create_video_task(
    State(state): State<AppState>,
    Path(resource_id): Path<String>,
    Json(request): Json<VideoProcessingRequest>,
) -> Result<Json<CreateTaskResponse>, ApiError> {
    let resource_id = ResourceId(resource_id);
    validate_resource_id(&resource_id)?;
    state.manifests.read(&resource_id).await?;
    let (task, status) = state
        .tasks
        .create_task(
            resource_id.clone(),
            TaskOperation::VideoProcessing { request },
        )
        .await?;
    state
        .manifests
        .record_task(&resource_id, task.task_id.clone())
        .await?;

    Ok(Json(CreateTaskResponse {
        resource_id,
        task_id: task.task_id,
        status,
    }))
}

async fn generate_link(
    State(state): State<AppState>,
    Path(resource_id): Path<String>,
    Json(request): Json<GenerateLinkRequest>,
) -> Result<Json<GeneratedLink>, ApiError> {
    let resource_id = ResourceId(resource_id);
    validate_resource_id(&resource_id)?;
    let manifest = state.manifests.read(&resource_id).await?;
    let object_key = request
        .object_key
        .unwrap_or_else(|| manifest.original.object_key.clone());
    let link = state
        .links
        .generate_link(&object_key, request.policy)
        .await?;
    Ok(Json(link))
}

async fn ingest_file_from_multipart(
    mut multipart: Multipart,
    temp_directory: &std::path::Path,
) -> Result<IngestedMedia, ApiError> {
    while let Some(field) = multipart.next_field().await? {
        if field.name() == Some("file") {
            return ingest_field(field, temp_directory).await;
        }
    }

    Err(ApiError::BadRequest(
        "multipart upload must contain a file field".to_string(),
    ))
}

async fn ingest_video_upload_from_multipart(
    mut multipart: Multipart,
    temp_directory: &std::path::Path,
) -> Result<(IngestedMedia, Option<VideoProcessingRequest>), ApiError> {
    let mut ingested_media = None;
    let mut processing_request = None;

    while let Some(field) = multipart.next_field().await? {
        match field.name() {
            Some("file") => {
                ingested_media = Some(ingest_field(field, temp_directory).await?);
            }
            Some("processing") => {
                let text = field.text().await?;
                processing_request = Some(serde_json::from_str(&text).map_err(|error| {
                    ApiError::BadRequest(format!("invalid processing JSON: {error}"))
                })?);
            }
            _ => {}
        }
    }

    let ingested_media = ingested_media.ok_or_else(|| {
        ApiError::BadRequest("multipart upload must contain a file field".to_string())
    })?;
    Ok((ingested_media, processing_request))
}

async fn ingest_field(
    mut field: axum::extract::multipart::Field<'_>,
    temp_directory: &std::path::Path,
) -> Result<IngestedMedia, ApiError> {
    let file_name = field.file_name().map(ToString::to_string);
    let content_type = field.content_type().map(ToString::to_string);
    let mut session = IngestSession::start(temp_directory, file_name, content_type).await?;

    while let Some(chunk) = field.chunk().await? {
        session.write_chunk(chunk).await?;
    }

    Ok(session.finish().await?)
}

async fn store_original(
    state: &AppState,
    ingested: &IngestedMedia,
    media_kind: MediaKind,
) -> Result<ResourceManifest, ApiError> {
    let original_key = original_object_key(&ingested.resource_id, &ingested.extension)?;
    let bytes = tokio::fs::read(ingested.temporary_file.path()).await?;

    state
        .storage
        .put_object_if_absent(PutObjectRequest {
            key: original_key.clone(),
            bytes: Bytes::from(bytes),
            content_type: Some(ingested.mime_type.clone()),
            metadata: BTreeMap::new(),
        })
        .await?;

    let metadata = match media_kind {
        MediaKind::Image => ImageProcessor::inspect_image(ingested.temporary_file.path())
            .unwrap_or_else(|_| MediaMetadata::default()),
        MediaKind::Video => MediaMetadata::default(),
    };

    let original = MediaObject {
        object_key: original_key,
        file_name: ingested.file_name.clone(),
        mime_type: ingested.mime_type.clone(),
        size_bytes: ingested.size_bytes,
        checksum_sha256: ingested.checksum_sha256.clone(),
        metadata,
    };

    Ok(state
        .manifests
        .create_if_missing(ingested.resource_id.clone(), media_kind, original)
        .await?)
}

fn ensure_media_kind(mime_type: &str, expected: MediaKind) -> Result<(), ApiError> {
    let is_valid = match expected {
        MediaKind::Image => mime_type.starts_with("image/"),
        MediaKind::Video => mime_type.starts_with("video/"),
    };

    if is_valid {
        Ok(())
    } else {
        Err(ApiError::UnsupportedMediaType(format!(
            "expected {expected:?}, got {mime_type}"
        )))
    }
}

fn default_video_processing_request() -> VideoProcessingRequest {
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

#[derive(Debug, Serialize)]
struct HealthResponse {
    status: String,
}

#[derive(Debug, Deserialize)]
struct GenerateLinkRequest {
    object_key: Option<String>,
    policy: LinkPolicy,
}

#[derive(Debug, Serialize)]
struct CreateTaskResponse {
    resource_id: ResourceId,
    task_id: TaskId,
    status: TaskStatus,
}

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("bad request: {0}")]
    BadRequest(String),
    #[error("unsupported media type: {0}")]
    UnsupportedMediaType(String),
    #[error(transparent)]
    Core(#[from] mediaforge_core::CoreError),
    #[error(transparent)]
    Ingest(#[from] IngestError),
    #[error(transparent)]
    Multipart(#[from] MultipartError),
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error(transparent)]
    Manifest(#[from] ManifestError),
    #[error(transparent)]
    Task(#[from] TaskError),
    #[error(transparent)]
    Link(#[from] LinkError),
    #[error("file IO error: {0}")]
    Io(#[from] std::io::Error),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = match &self {
            ApiError::BadRequest(_) => StatusCode::BAD_REQUEST,
            ApiError::UnsupportedMediaType(_) => StatusCode::UNSUPPORTED_MEDIA_TYPE,
            ApiError::Manifest(ManifestError::NotFound(_))
            | ApiError::Task(TaskError::NotFound(_))
            | ApiError::Storage(StorageError::NotFound(_)) => StatusCode::NOT_FOUND,
            ApiError::Link(LinkError::MissingCdnBaseUrl) => StatusCode::BAD_REQUEST,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };

        let body = Json(serde_json::json!({
            "error": self.to_string()
        }));
        (status, body).into_response()
    }
}

#[allow(dead_code)]
fn manifest_key_for_response(
    resource_id: &ResourceId,
) -> Result<String, mediaforge_core::CoreError> {
    manifest_key(resource_id)
}
