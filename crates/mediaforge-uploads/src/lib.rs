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
use std::time::Duration;
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
    #[error("upload session update for {0} kept conflicting after {1} attempts")]
    Conflict(String, u32),
}

pub type UploadResult<T> = Result<T, UploadError>;

const MAX_UPLOAD_UPDATE_ATTEMPTS: u32 = 5;

/// Lifecycle of a presigned upload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UploadState {
    /// Presigned URL issued; awaiting the client's direct-to-storage PUT.
    Created,
    /// Client reported completion; queued for worker finalization.
    Pending,
    /// A worker claimed the upload and is finalizing it. Reclaimable after timeout.
    Finalizing,
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
        Ok(self.read_with_version(upload_id).await?.0)
    }

    async fn read_with_version(
        &self,
        upload_id: &str,
    ) -> UploadResult<(UploadSession, Option<String>)> {
        let key = upload_session_key(upload_id);
        let (bytes, version) =
            self.storage
                .get_object_with_version(&key)
                .await
                .map_err(|error| match error {
                    StorageError::NotFound(_) => UploadError::NotFound(upload_id.to_string()),
                    other => UploadError::Storage(other),
                })?;
        Ok((serde_json::from_slice(&bytes)?, version))
    }

    /// Moves a session into the finalization queue (idempotent).
    pub async fn enqueue_finalization(&self, upload_id: &str) -> UploadResult<UploadSession> {
        let mut session = self.read(upload_id).await?;
        match session.state {
            UploadState::Created | UploadState::Failed => {
                session.state = UploadState::Pending;
                session.error = None;
                session.updated_at = Utc::now();
                self.write(&session).await?;
                self.write_pending_marker(upload_id).await?;
            }
            UploadState::Pending => {
                self.write_pending_marker(upload_id).await?;
            }
            UploadState::Finalizing => {}
            UploadState::Completed => {
                self.remove_pending(upload_id).await?;
            }
        }
        Ok(session)
    }

    /// Claims a queued upload before worker finalization. A crashed worker's
    /// `Finalizing` state can be reclaimed once its lease-like timestamp expires.
    pub async fn claim_finalization(
        &self,
        upload_id: &str,
        lease_timeout: Duration,
    ) -> UploadResult<Option<UploadSession>> {
        let key = upload_session_key(upload_id);

        for _ in 0..MAX_UPLOAD_UPDATE_ATTEMPTS {
            let (mut session, version) = self.read_with_version(upload_id).await?;
            let claimable = match session.state {
                UploadState::Pending => true,
                UploadState::Finalizing => finalization_claim_expired(&session, lease_timeout),
                UploadState::Created | UploadState::Completed | UploadState::Failed => {
                    self.remove_pending(upload_id).await?;
                    return Ok(None);
                }
            };

            if !claimable {
                return Ok(None);
            }

            session.state = UploadState::Finalizing;
            session.error = None;
            session.updated_at = Utc::now();
            let committed = self
                .storage
                .put_object_if_match(self.session_put_request(key.clone(), &session)?, version)
                .await?;
            if committed {
                return Ok(Some(session));
            }
        }

        Err(UploadError::Conflict(
            upload_id.to_string(),
            MAX_UPLOAD_UPDATE_ATTEMPTS,
        ))
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
        if session.state == UploadState::Completed {
            self.remove_pending(upload_id).await?;
            return Ok(session);
        }

        session.attempts += 1;
        session.error = Some(error);
        session.updated_at = Utc::now();
        if terminal {
            session.state = UploadState::Failed;
        } else {
            session.state = UploadState::Pending;
        }
        self.write(&session).await?;
        if terminal {
            self.remove_pending(upload_id).await?;
        } else {
            self.write_pending_marker(upload_id).await?;
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
        self.storage
            .put_object(self.session_put_request(upload_session_key(&session.upload_id), session)?)
            .await?;
        Ok(())
    }

    async fn write_pending_marker(&self, upload_id: &str) -> UploadResult<()> {
        self.put_json(
            upload_pending_key(upload_id),
            &PendingMarker::new(upload_id),
        )
        .await
    }

    fn session_put_request(
        &self,
        key: String,
        session: &UploadSession,
    ) -> UploadResult<PutObjectRequest> {
        Ok(PutObjectRequest {
            key,
            bytes: Bytes::from(serde_json::to_vec_pretty(session)?),
            content_type: Some("application/json".to_string()),
            metadata: BTreeMap::new(),
        })
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

fn finalization_claim_expired(session: &UploadSession, lease_timeout: Duration) -> bool {
    let elapsed_seconds = (Utc::now() - session.updated_at).num_seconds();
    Duration::from_secs(elapsed_seconds.max(0) as u64) >= lease_timeout
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
    async fn completed_upload_is_not_requeued_by_duplicate_complete() {
        let directory = tempfile::tempdir().unwrap();
        let storage = Arc::new(FilesystemObjectStorage::new(directory.path().to_path_buf()));
        let repository = UploadSessionRepository::new(storage);

        let session = repository.create(new_image_upload()).await.unwrap();
        repository
            .enqueue_finalization(&session.upload_id)
            .await
            .unwrap();
        repository
            .mark_finalized(
                &session.upload_id,
                mediaforge_core::resource_id_from_bytes(b"asset"),
                None,
            )
            .await
            .unwrap();

        let session = repository
            .enqueue_finalization(&session.upload_id)
            .await
            .unwrap();
        assert_eq!(session.state, UploadState::Completed);
        assert!(repository.list_pending().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn finalization_claim_only_succeeds_once_until_timeout() {
        let directory = tempfile::tempdir().unwrap();
        let storage = Arc::new(FilesystemObjectStorage::new(directory.path().to_path_buf()));
        let repository = UploadSessionRepository::new(storage);

        let session = repository.create(new_image_upload()).await.unwrap();
        repository
            .enqueue_finalization(&session.upload_id)
            .await
            .unwrap();

        let claimed = repository
            .claim_finalization(&session.upload_id, Duration::from_secs(600))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(claimed.state, UploadState::Finalizing);

        assert!(repository
            .claim_finalization(&session.upload_id, Duration::from_secs(600))
            .await
            .unwrap()
            .is_none());

        let mut stale = repository.read(&session.upload_id).await.unwrap();
        stale.updated_at = Utc::now() - chrono::Duration::seconds(10);
        repository.write(&stale).await.unwrap();

        assert!(repository
            .claim_finalization(&session.upload_id, Duration::from_secs(1))
            .await
            .unwrap()
            .is_some());
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
