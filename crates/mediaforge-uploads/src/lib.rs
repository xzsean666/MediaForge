//! Presigned-upload session lifecycle.
//!
//! Clients upload media bytes directly to object storage via a presigned PUT
//! URL; the API never proxies the bytes. This crate owns the durable session
//! record and the finalization queue that the worker drains. State lives only
//! in object storage (no database), matching the stateless system constraint.

use bytes::Bytes;
use chrono::{DateTime, Utc};
use mediaforge_core::{upload_pending_key, upload_pending_prefix, upload_session_key, CoreError};
use mediaforge_storage::{DynObjectStorage, PutObjectRequest, StorageError};
use mediaforge_types::{MediaKind, ResourceId, TaskId};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum UploadError {
    #[error(transparent)]
    Core(#[from] CoreError),
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error("upload session not found: {0}")]
    NotFound(String),
    #[error("failed to serialize upload session: {0}")]
    Serialize(#[from] serde_json::Error),
}

pub type UploadResult<T> = Result<T, UploadError>;

/// Lifecycle of a presigned upload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UploadState {
    /// Presigned URL issued; awaiting the client's direct-to-storage PUT.
    Created,
    /// Client reported completion; queued for worker finalization.
    Pending,
    /// Finalized: bytes hashed, content-addressed, manifest written.
    Completed,
    /// Finalization failed permanently.
    Failed,
}

/// Durable record of a single upload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UploadSession {
    pub upload_id: String,
    pub media_kind: MediaKind,
    pub staging_key: String,
    pub file_name: Option<String>,
    pub content_type: Option<String>,
    pub declared_size_bytes: Option<u64>,
    /// Optional TTL for the original source object after finalization.
    pub source_expires_in_seconds: Option<u64>,
    /// Raw processing request JSON supplied by the client (kind-specific).
    pub processing: Option<serde_json::Value>,
    pub state: UploadState,
    pub attempts: u32,
    pub resource_id: Option<ResourceId>,
    pub processing_task_id: Option<TaskId>,
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Parameters for opening a new upload session.
#[derive(Debug, Clone)]
pub struct NewUpload {
    pub media_kind: MediaKind,
    pub file_name: Option<String>,
    pub content_type: Option<String>,
    pub declared_size_bytes: Option<u64>,
    pub source_expires_in_seconds: Option<u64>,
    pub processing: Option<serde_json::Value>,
}

#[derive(Clone)]
pub struct UploadSessionRepository {
    storage: DynObjectStorage,
}

impl UploadSessionRepository {
    pub fn new(storage: DynObjectStorage) -> Self {
        Self { storage }
    }

    /// Creates a session and returns it. The caller derives a presigned PUT URL
    /// for `session.staging_key` separately.
    pub async fn create(&self, request: NewUpload) -> UploadResult<UploadSession> {
        let upload_id = Uuid::new_v4().to_string();
        let staging_key = mediaforge_core::upload_staging_key(&upload_id);
        let now = Utc::now();
        let session = UploadSession {
            upload_id,
            media_kind: request.media_kind,
            staging_key,
            file_name: request.file_name,
            content_type: request.content_type,
            declared_size_bytes: request.declared_size_bytes,
            source_expires_in_seconds: request.source_expires_in_seconds,
            processing: request.processing,
            state: UploadState::Created,
            attempts: 0,
            resource_id: None,
            processing_task_id: None,
            error: None,
            created_at: now,
            updated_at: now,
        };
        self.write(&session).await?;
        Ok(session)
    }

    pub async fn read(&self, upload_id: &str) -> UploadResult<UploadSession> {
        let key = upload_session_key(upload_id);
        let bytes = self
            .storage
            .get_object(&key)
            .await
            .map_err(|error| match error {
                StorageError::NotFound(_) => UploadError::NotFound(upload_id.to_string()),
                other => UploadError::Storage(other),
            })?;
        Ok(serde_json::from_slice(&bytes)?)
    }

    /// Moves a session into the finalization queue (idempotent).
    pub async fn enqueue_finalization(&self, upload_id: &str) -> UploadResult<UploadSession> {
        let mut session = self.read(upload_id).await?;
        if session.state == UploadState::Created || session.state == UploadState::Failed {
            session.state = UploadState::Pending;
            session.error = None;
            session.updated_at = Utc::now();
            self.write(&session).await?;
        }
        self.put_json(upload_pending_key(upload_id), &PendingMarker::new(upload_id))
            .await?;
        Ok(session)
    }

    /// Returns the upload ids currently awaiting finalization.
    pub async fn list_pending(&self) -> UploadResult<Vec<String>> {
        let keys = self.storage.list_keys(&upload_pending_prefix()).await?;
        let mut upload_ids = Vec::new();
        for key in keys {
            let bytes = self.storage.get_object(&key).await?;
            let marker: PendingMarker = serde_json::from_slice(&bytes)?;
            upload_ids.push(marker.upload_id.into_owned());
        }
        Ok(upload_ids)
    }

    pub async fn mark_finalized(
        &self,
        upload_id: &str,
        resource_id: ResourceId,
        processing_task_id: Option<TaskId>,
    ) -> UploadResult<UploadSession> {
        let mut session = self.read(upload_id).await?;
        session.state = UploadState::Completed;
        session.resource_id = Some(resource_id);
        session.processing_task_id = processing_task_id;
        session.error = None;
        session.updated_at = Utc::now();
        self.write(&session).await?;
        self.remove_pending(upload_id).await?;
        Ok(session)
    }

    /// Records a finalization failure. `terminal` removes it from the queue.
    pub async fn mark_failed(
        &self,
        upload_id: &str,
        error: String,
        terminal: bool,
    ) -> UploadResult<UploadSession> {
        let mut session = self.read(upload_id).await?;
        session.attempts += 1;
        session.error = Some(error);
        session.updated_at = Utc::now();
        if terminal {
            session.state = UploadState::Failed;
        }
        self.write(&session).await?;
        if terminal {
            self.remove_pending(upload_id).await?;
        }
        Ok(session)
    }

    /// Deletes a session and its queue marker. Staging object cleanup is the
    /// caller's responsibility (it knows `session.staging_key`).
    pub async fn delete(&self, upload_id: &str) -> UploadResult<()> {
        self.storage
            .delete_object(&upload_session_key(upload_id))
            .await?;
        self.remove_pending(upload_id).await?;
        Ok(())
    }

    async fn remove_pending(&self, upload_id: &str) -> UploadResult<()> {
        self.storage
            .delete_object(&upload_pending_key(upload_id))
            .await?;
        Ok(())
    }

    async fn write(&self, session: &UploadSession) -> UploadResult<()> {
        self.put_json(upload_session_key(&session.upload_id), session)
            .await
    }

    async fn put_json<T>(&self, key: String, value: &T) -> UploadResult<()>
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

#[derive(Debug, Serialize, Deserialize)]
struct PendingMarker<'a> {
    #[serde(borrow)]
    upload_id: std::borrow::Cow<'a, str>,
}

impl<'a> PendingMarker<'a> {
    fn new(upload_id: &'a str) -> Self {
        Self {
            upload_id: std::borrow::Cow::Borrowed(upload_id),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mediaforge_storage::FilesystemObjectStorage;
    use std::sync::Arc;

    fn new_image_upload() -> NewUpload {
        NewUpload {
            media_kind: MediaKind::Image,
            file_name: Some("photo.jpg".to_string()),
            content_type: Some("image/jpeg".to_string()),
            declared_size_bytes: Some(1024),
            source_expires_in_seconds: None,
            processing: None,
        }
    }

    #[tokio::test]
    async fn create_then_finalize_round_trips() {
        let directory = tempfile::tempdir().unwrap();
        let storage = Arc::new(FilesystemObjectStorage::new(directory.path().to_path_buf()));
        let repository = UploadSessionRepository::new(storage);

        let session = repository.create(new_image_upload()).await.unwrap();
        assert_eq!(session.state, UploadState::Created);

        repository
            .enqueue_finalization(&session.upload_id)
            .await
            .unwrap();
        assert_eq!(
            repository.list_pending().await.unwrap(),
            vec![session.upload_id.clone()]
        );

        let resource_id = mediaforge_core::resource_id_from_bytes(b"asset");
        repository
            .mark_finalized(&session.upload_id, resource_id.clone(), None)
            .await
            .unwrap();

        let loaded = repository.read(&session.upload_id).await.unwrap();
        assert_eq!(loaded.state, UploadState::Completed);
        assert_eq!(loaded.resource_id, Some(resource_id));
        assert!(repository.list_pending().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn missing_session_is_not_found() {
        let directory = tempfile::tempdir().unwrap();
        let storage = Arc::new(FilesystemObjectStorage::new(directory.path().to_path_buf()));
        let repository = UploadSessionRepository::new(storage);
        let error = repository.read("nope").await.unwrap_err();
        assert!(matches!(error, UploadError::NotFound(_)));
    }
}
