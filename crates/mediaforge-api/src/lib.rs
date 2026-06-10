use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::middleware;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use mediaforge_auth::require_auth;
use mediaforge_config::AppConfig;
use mediaforge_core::validate_resource_id;
use mediaforge_links::{LinkError, LinkGenerator};
use mediaforge_manifest::{ManifestError, ManifestRepository};
use mediaforge_storage::{create_object_storage, DynObjectStorage, StorageError};
use mediaforge_tasks::{TaskError, TaskRepository};
use mediaforge_types::{
    GeneratedLink, ImageProcessingRequest, LinkPolicy, ResourceId, ResourceManifest, TaskId,
    TaskOperation, TaskStatus, VideoProcessingRequest,
};
use mediaforge_uploads::{UploadError, UploadSessionRepository};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;

mod uploads;

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
    uploads: UploadSessionRepository,
    links: LinkGenerator,
}

impl AppState {
    pub fn new(config: AppConfig, storage: DynObjectStorage) -> Self {
        let manifests = ManifestRepository::new(storage.clone());
        let tasks = TaskRepository::new(storage.clone());
        let uploads = UploadSessionRepository::new(storage.clone());
        let links = LinkGenerator::new(&config, storage.clone());
        Self {
            config: Arc::new(config),
            storage,
            manifests,
            tasks,
            uploads,
            links,
        }
    }
}

pub async fn run_api(config: AppConfig) -> Result<(), ApiRunError> {
    if config.uses_default_signing_secret() {
        tracing::warn!(
            "MEDIAFORGE_LINK_SIGNING_SECRET is the insecure development default; \
             set a strong secret before serving production traffic"
        );
    }

    let storage = create_object_storage(&config).await?;
    let bind_address = config.http_bind;
    let router = build_router(config, storage);
    let listener = tokio::net::TcpListener::bind(bind_address).await?;
    tracing::info!(%bind_address, "mediaforge api listening");
    axum::serve(listener, router).await?;
    Ok(())
}

pub fn build_router(config: AppConfig, storage: DynObjectStorage) -> Router {
    // Auth middleware gets its own state (the auth config) independent of the
    // handler state. It is a no-op when auth is disabled and always lets
    // `/health` through.
    let auth_config = Arc::new(config.auth.clone());
    let state = AppState::new(config, storage);

    Router::new()
        .route("/health", get(health))
        .route("/v1/uploads", post(uploads::create_upload))
        .route("/v1/uploads/{upload_id}", get(uploads::get_upload))
        .route("/v1/uploads/{upload_id}", delete(uploads::delete_upload))
        .route(
            "/v1/uploads/{upload_id}/complete",
            post(uploads::complete_upload),
        )
        .route("/v1/resources/{resource_id}", get(get_resource))
        .route("/v1/resources/{resource_id}/image", post(create_image_task))
        .route("/v1/resources/{resource_id}/video", post(create_video_task))
        .route("/v1/resources/{resource_id}/links", post(generate_link))
        .route("/v1/tasks/{task_id}", get(get_task))
        .layer(middleware::from_fn_with_state(auth_config, require_auth))
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok".to_string(),
    })
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
    #[error("payload too large: {0}")]
    PayloadTooLarge(String),
    #[error(transparent)]
    Core(#[from] mediaforge_core::CoreError),
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error(transparent)]
    Manifest(#[from] ManifestError),
    #[error(transparent)]
    Task(#[from] TaskError),
    #[error(transparent)]
    Upload(#[from] UploadError),
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
            ApiError::PayloadTooLarge(_) => StatusCode::PAYLOAD_TOO_LARGE,
            ApiError::Manifest(ManifestError::NotFound(_))
            | ApiError::Task(TaskError::NotFound(_))
            | ApiError::Upload(UploadError::NotFound(_))
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
