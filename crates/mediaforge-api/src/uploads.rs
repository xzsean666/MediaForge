//! Presigned direct-to-storage upload endpoints.
//!
//! Clients never stream bytes through the API. Instead:
//! 1. `POST /v1/uploads` opens a session and returns a presigned PUT URL.
//! 2. The client PUTs the file straight to object storage.
//! 3. `POST /v1/uploads/{id}/complete` confirms the object and queues it for
//!    worker finalization (download, hash, content-address, manifest).
//! 4. `GET /v1/uploads/{id}` is polled until the finalized `resource_id` appears.

use crate::{ApiError, AppState};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use mediaforge_storage::StorageError;
use mediaforge_types::MediaKind;
use mediaforge_uploads::{NewUpload, UploadSession, UploadState};
use serde::{Deserialize, Serialize};

const MAX_SOURCE_TTL_SECONDS: u64 = 365 * 24 * 60 * 60;

#[derive(Debug, Deserialize)]
pub struct CreateUploadRequest {
    pub media_kind: MediaKind,
    #[serde(default)]
    pub file_name: Option<String>,
    #[serde(default)]
    pub content_type: Option<String>,
    /// Client-declared size, validated against the upload limit up front so an
    /// oversized upload is rejected before any bytes are sent.
    #[serde(default)]
    pub declared_size_bytes: Option<u64>,
    /// Optional TTL after which the worker reaper deletes the original source.
    #[serde(default)]
    pub source_expires_in_seconds: Option<u64>,
    /// Optional processing request (kind-specific JSON) applied after finalize.
    #[serde(default)]
    pub processing: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
pub struct CreateUploadResponse {
    pub upload_id: String,
    pub upload_url: String,
    pub object_key: String,
    pub method: &'static str,
    pub expires_at: DateTime<Utc>,
}

/// Opens an upload session and returns a presigned PUT URL.
pub async fn create_upload(
    State(state): State<AppState>,
    Json(request): Json<CreateUploadRequest>,
) -> Result<(StatusCode, Json<CreateUploadResponse>), ApiError> {
    enforce_declared_size(&state, request.declared_size_bytes)?;
    enforce_source_ttl(request.source_expires_in_seconds)?;

    let session = state
        .uploads
        .create(NewUpload {
            media_kind: request.media_kind,
            file_name: request.file_name,
            content_type: request.content_type,
            declared_size_bytes: request.declared_size_bytes,
            source_expires_in_seconds: request.source_expires_in_seconds,
            processing: request.processing,
        })
        .await?;

    let expiry = state.config.presigned_upload_expiry;
    // Presign without binding a content type so the client can PUT freely; the
    // declared content type is kept in the session for finalization.
    let upload_url = state
        .storage
        .presign_put_url(&session.staging_key, expiry, None)
        .await?;
    let expires_at =
        Utc::now() + ChronoDuration::from_std(expiry).unwrap_or(ChronoDuration::hours(1));

    Ok((
        StatusCode::CREATED,
        Json(CreateUploadResponse {
            upload_id: session.upload_id,
            upload_url,
            object_key: session.staging_key,
            method: "PUT",
            expires_at,
        }),
    ))
}

/// Confirms the client finished uploading and queues finalization.
pub async fn complete_upload(
    State(state): State<AppState>,
    Path(upload_id): Path<String>,
) -> Result<(StatusCode, Json<UploadSession>), ApiError> {
    let session = state.uploads.read(&upload_id).await?;

    if matches!(
        session.state,
        UploadState::Pending | UploadState::Finalizing | UploadState::Completed
    ) {
        let session = state.uploads.enqueue_finalization(&upload_id).await?;
        return Ok((StatusCode::ACCEPTED, Json(session)));
    }

    // Confirm the object actually landed in storage and recheck the real size
    // against the limit (the declared size cannot be trusted).
    let metadata = match state.storage.object_metadata(&session.staging_key).await {
        Ok(metadata) => metadata,
        Err(StorageError::NotFound(_)) => {
            return Err(ApiError::BadRequest(
                "no object found at the staging key; upload the file before completing".to_string(),
            ));
        }
        Err(other) => return Err(other.into()),
    };

    if let Some(limit) = state.config.upload_limit.effective_max_bytes() {
        if metadata.size_bytes > limit {
            // Reject and discard the oversized object.
            let _ = state.storage.delete_object(&session.staging_key).await;
            return Err(ApiError::PayloadTooLarge(format!(
                "uploaded object is {} bytes; limit is {limit} bytes",
                metadata.size_bytes
            )));
        }
    }

    let session = state.uploads.enqueue_finalization(&upload_id).await?;
    Ok((StatusCode::ACCEPTED, Json(session)))
}

/// Returns the current upload session, including the finalized `resource_id`
/// and processing task id once available.
pub async fn get_upload(
    State(state): State<AppState>,
    Path(upload_id): Path<String>,
) -> Result<Json<UploadSession>, ApiError> {
    Ok(Json(state.uploads.read(&upload_id).await?))
}

/// Cancels an upload: removes the session, queue marker, and staging object.
pub async fn delete_upload(
    State(state): State<AppState>,
    Path(upload_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    if let Ok(session) = state.uploads.read(&upload_id).await {
        let _ = state.storage.delete_object(&session.staging_key).await;
    }
    state.uploads.delete(&upload_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

fn enforce_declared_size(state: &AppState, declared: Option<u64>) -> Result<(), ApiError> {
    if let (Some(limit), Some(size)) = (state.config.upload_limit.effective_max_bytes(), declared) {
        if size > limit {
            return Err(ApiError::PayloadTooLarge(format!(
                "declared size {size} bytes exceeds limit {limit} bytes"
            )));
        }
    }
    Ok(())
}

fn enforce_source_ttl(source_expires_in_seconds: Option<u64>) -> Result<(), ApiError> {
    if let Some(seconds) = source_expires_in_seconds {
        if seconds > MAX_SOURCE_TTL_SECONDS {
            return Err(ApiError::BadRequest(format!(
                "source_expires_in_seconds must be at most {MAX_SOURCE_TTL_SECONDS}"
            )));
        }
    }
    Ok(())
}
