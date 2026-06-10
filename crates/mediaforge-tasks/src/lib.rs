use bytes::Bytes;
use chrono::{DateTime, Utc};
use mediaforge_core::{
    task_completed_key, task_failed_key, task_id_from_parameters, task_lease_key, task_pending_key,
    task_status_key, CoreError,
};
use mediaforge_storage::{DynObjectStorage, PutObjectRequest, StorageError};
use mediaforge_types::{ResourceId, TaskDescriptor, TaskId, TaskOperation, TaskState, TaskStatus};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::time::Duration;

#[derive(Debug, thiserror::Error)]
pub enum TaskError {
    #[error(transparent)]
    Core(#[from] CoreError),
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error("task not found: {0}")]
    NotFound(TaskId),
    #[error("failed to serialize task data: {0}")]
    Serialize(#[from] serde_json::Error),
}

pub type TaskResult<T> = Result<T, TaskError>;

#[derive(Clone)]
pub struct TaskRepository {
    storage: DynObjectStorage,
}

#[derive(Debug, Serialize, Deserialize)]
struct TaskLease {
    task_id: TaskId,
    worker_id: String,
    leased_at: DateTime<Utc>,
}

impl TaskRepository {
    pub fn new(storage: DynObjectStorage) -> Self {
        Self { storage }
    }

    pub async fn create_task(
        &self,
        resource_id: ResourceId,
        operation: TaskOperation,
    ) -> TaskResult<(TaskDescriptor, TaskStatus)> {
        let operation_name = operation_name(&operation);
        let parameters = serde_json::to_value(&operation)?;
        let task_id = task_id_from_parameters(&resource_id, operation_name, &parameters)?;
        let now = Utc::now();

        let descriptor = TaskDescriptor {
            task_id: task_id.clone(),
            resource_id: resource_id.clone(),
            operation,
            parameters,
            created_at: now,
        };
        let status = TaskStatus {
            task_id: task_id.clone(),
            resource_id,
            state: TaskState::Pending,
            message: None,
            attempts: 0,
            updated_at: now,
        };

        self.put_json_if_absent(task_pending_key(&task_id), &descriptor)
            .await?;
        self.put_json_if_absent(task_status_key(&task_id), &status)
            .await?;

        let current_status = self.read_status(&task_id).await.unwrap_or(status);
        Ok((descriptor, current_status))
    }

    pub async fn list_pending(&self) -> TaskResult<Vec<TaskDescriptor>> {
        let keys = self.storage.list_keys("tasks/pending").await?;
        let mut tasks = Vec::new();

        for key in keys {
            let bytes = self.storage.get_object(&key).await?;
            tasks.push(serde_json::from_slice(&bytes)?);
        }

        Ok(tasks)
    }

    pub async fn read_status(&self, task_id: &TaskId) -> TaskResult<TaskStatus> {
        let key = task_status_key(task_id);
        let bytes = self
            .storage
            .get_object(&key)
            .await
            .map_err(|error| match error {
                StorageError::NotFound(_) => TaskError::NotFound(task_id.clone()),
                other => TaskError::Storage(other),
            })?;
        Ok(serde_json::from_slice(&bytes)?)
    }

    pub async fn read_descriptor(&self, task_id: &TaskId) -> TaskResult<TaskDescriptor> {
        let key = task_pending_key(task_id);
        let bytes = self
            .storage
            .get_object(&key)
            .await
            .map_err(|error| match error {
                StorageError::NotFound(_) => TaskError::NotFound(task_id.clone()),
                other => TaskError::Storage(other),
            })?;
        Ok(serde_json::from_slice(&bytes)?)
    }

    /// Atomically claims a pending task by creating its lease object. Returns
    /// `true` only if this caller acquired the lease.
    pub async fn claim_task(&self, task_id: &TaskId, worker_id: &str) -> TaskResult<bool> {
        let created = self.write_lease_if_absent(task_id, worker_id).await?;
        if created {
            self.transition(
                task_id,
                TaskState::Leased,
                Some(format!("leased by {worker_id}")),
            )
            .await?;
        }
        Ok(created)
    }

    /// Claims a task, reclaiming it from a crashed worker when its lease is
    /// older than `lease_timeout`. Returns `true` if this caller now owns it.
    pub async fn claim_or_reclaim_task(
        &self,
        task_id: &TaskId,
        worker_id: &str,
        lease_timeout: Duration,
    ) -> TaskResult<bool> {
        if self.write_lease_if_absent(task_id, worker_id).await? {
            self.transition(
                task_id,
                TaskState::Leased,
                Some(format!("leased by {worker_id}")),
            )
            .await?;
            return Ok(true);
        }

        // A lease already exists; steal it only if it has expired.
        match self.read_lease(task_id).await? {
            Some(lease) if lease_age(&lease) >= lease_timeout => {
                self.write_lease(task_id, worker_id).await?;
                self.transition(
                    task_id,
                    TaskState::Leased,
                    Some(format!("reclaimed by {worker_id}")),
                )
                .await?;
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    pub async fn mark_running(&self, task_id: &TaskId) -> TaskResult<TaskStatus> {
        self.transition(task_id, TaskState::Running, None).await
    }

    pub async fn mark_completed(
        &self,
        task_id: &TaskId,
        message: Option<String>,
    ) -> TaskResult<TaskStatus> {
        let status = self
            .transition(task_id, TaskState::Completed, message)
            .await?;
        self.put_json(task_completed_key(task_id), &status).await?;
        // Remove the task from the work queue so `list_pending` does not keep
        // re-reading completed descriptors forever.
        self.cleanup_queue(task_id).await?;
        Ok(status)
    }

    /// Records a failure. Retries (state returns to `Pending`, lease released)
    /// until `attempts` reaches `max_attempts`, after which the task is marked
    /// permanently `Failed` and removed from the queue.
    pub async fn mark_failed(
        &self,
        task_id: &TaskId,
        message: String,
        max_attempts: u32,
    ) -> TaskResult<TaskStatus> {
        let mut status = self.read_status(task_id).await?;
        status.attempts += 1;
        status.message = Some(message);
        status.updated_at = Utc::now();

        if status.attempts >= max_attempts.max(1) {
            status.state = TaskState::Failed;
            self.write_status(status.clone()).await?;
            self.put_json(task_failed_key(task_id), &status).await?;
            self.cleanup_queue(task_id).await?;
        } else {
            // Release the lease and requeue so any worker can retry it.
            status.state = TaskState::Pending;
            self.write_status(status.clone()).await?;
            let _ = self.storage.delete_object(&task_lease_key(task_id)).await;
        }
        Ok(status)
    }

    /// Writes a new status preserving `resource_id` and `attempts`.
    async fn transition(
        &self,
        task_id: &TaskId,
        state: TaskState,
        message: Option<String>,
    ) -> TaskResult<TaskStatus> {
        let previous = self.read_status(task_id).await?;
        let status = TaskStatus {
            task_id: task_id.clone(),
            resource_id: previous.resource_id,
            state,
            message,
            attempts: previous.attempts,
            updated_at: Utc::now(),
        };
        self.write_status(status.clone()).await?;
        Ok(status)
    }

    async fn cleanup_queue(&self, task_id: &TaskId) -> TaskResult<()> {
        let _ = self.storage.delete_object(&task_pending_key(task_id)).await;
        let _ = self.storage.delete_object(&task_lease_key(task_id)).await;
        Ok(())
    }

    async fn write_lease_if_absent(&self, task_id: &TaskId, worker_id: &str) -> TaskResult<bool> {
        self.put_json_if_absent(task_lease_key(task_id), &self.new_lease(task_id, worker_id))
            .await
    }

    async fn write_lease(&self, task_id: &TaskId, worker_id: &str) -> TaskResult<()> {
        self.put_json(task_lease_key(task_id), &self.new_lease(task_id, worker_id))
            .await
    }

    fn new_lease(&self, task_id: &TaskId, worker_id: &str) -> TaskLease {
        TaskLease {
            task_id: task_id.clone(),
            worker_id: worker_id.to_string(),
            leased_at: Utc::now(),
        }
    }

    async fn read_lease(&self, task_id: &TaskId) -> TaskResult<Option<TaskLease>> {
        match self.storage.get_object(&task_lease_key(task_id)).await {
            Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
            Err(StorageError::NotFound(_)) => Ok(None),
            Err(other) => Err(TaskError::Storage(other)),
        }
    }

    async fn write_status(&self, status: TaskStatus) -> TaskResult<()> {
        self.put_json(task_status_key(&status.task_id), &status)
            .await
    }

    async fn put_json<T>(&self, key: String, value: &T) -> TaskResult<()>
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

    async fn put_json_if_absent<T>(&self, key: String, value: &T) -> TaskResult<bool>
    where
        T: Serialize,
    {
        Ok(self
            .storage
            .put_object_if_absent(PutObjectRequest {
                key,
                bytes: Bytes::from(serde_json::to_vec_pretty(value)?),
                content_type: Some("application/json".to_string()),
                metadata: BTreeMap::new(),
            })
            .await?)
    }
}

fn operation_name(operation: &TaskOperation) -> &'static str {
    match operation {
        TaskOperation::ImageProcessing { .. } => "image_processing",
        TaskOperation::VideoProcessing { .. } => "video_processing",
    }
}

/// How long ago a lease was taken. Saturates to zero if the lease timestamp is
/// in the future (clock skew) so a skewed lease is never treated as expired.
fn lease_age(lease: &TaskLease) -> Duration {
    let elapsed_seconds = (Utc::now() - lease.leased_at).num_seconds();
    Duration::from_secs(elapsed_seconds.max(0) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mediaforge_storage::FilesystemObjectStorage;
    use mediaforge_types::{
        TaskOperation, VideoCodec, VideoContainer, VideoProcessingRequest, VideoProfile,
    };
    use std::sync::Arc;

    #[tokio::test]
    async fn repeated_task_creation_returns_same_task_id() {
        let directory = tempfile::tempdir().unwrap();
        let storage = Arc::new(FilesystemObjectStorage::new(directory.path().to_path_buf()));
        let repository = TaskRepository::new(storage);
        let resource_id = mediaforge_core::resource_id_from_bytes(b"video");
        let operation = TaskOperation::VideoProcessing {
            request: VideoProcessingRequest {
                profile: VideoProfile {
                    codec: VideoCodec::H264,
                    container: VideoContainer::Mp4,
                    resolution: None,
                    crf: Some(23),
                    bitrate_kbps: None,
                },
                screenshots: Vec::new(),
                generate_cover: false,
                generate_hls: false,
            },
        };

        let (first, _) = repository
            .create_task(resource_id.clone(), operation.clone())
            .await
            .unwrap();
        let (second, _) = repository
            .create_task(resource_id, operation)
            .await
            .unwrap();

        assert_eq!(first.task_id, second.task_id);
    }

    #[tokio::test]
    async fn claim_task_only_succeeds_once() {
        let directory = tempfile::tempdir().unwrap();
        let storage = Arc::new(FilesystemObjectStorage::new(directory.path().to_path_buf()));
        let repository = TaskRepository::new(storage);
        let resource_id = mediaforge_core::resource_id_from_bytes(b"video");
        let operation = TaskOperation::VideoProcessing {
            request: VideoProcessingRequest {
                profile: VideoProfile {
                    codec: VideoCodec::H264,
                    container: VideoContainer::Mp4,
                    resolution: None,
                    crf: Some(23),
                    bitrate_kbps: None,
                },
                screenshots: Vec::new(),
                generate_cover: false,
                generate_hls: false,
            },
        };
        let (task, _) = repository
            .create_task(resource_id, operation)
            .await
            .unwrap();

        assert!(repository
            .claim_task(&task.task_id, "worker-a")
            .await
            .unwrap());
        assert!(!repository
            .claim_task(&task.task_id, "worker-b")
            .await
            .unwrap());
    }

    fn sample_operation() -> TaskOperation {
        TaskOperation::VideoProcessing {
            request: VideoProcessingRequest {
                profile: VideoProfile {
                    codec: VideoCodec::H264,
                    container: VideoContainer::Mp4,
                    resolution: None,
                    crf: Some(23),
                    bitrate_kbps: None,
                },
                screenshots: Vec::new(),
                generate_cover: false,
                generate_hls: false,
            },
        }
    }

    async fn test_repository() -> (tempfile::TempDir, TaskRepository) {
        let directory = tempfile::tempdir().unwrap();
        let storage = Arc::new(FilesystemObjectStorage::new(directory.path().to_path_buf()));
        (directory, TaskRepository::new(storage))
    }

    #[tokio::test]
    async fn completing_a_task_removes_it_from_the_queue() {
        let (_dir, repository) = test_repository().await;
        let resource_id = mediaforge_core::resource_id_from_bytes(b"video");
        let (task, _) = repository
            .create_task(resource_id, sample_operation())
            .await
            .unwrap();

        repository
            .claim_task(&task.task_id, "worker-a")
            .await
            .unwrap();
        repository
            .mark_completed(&task.task_id, None)
            .await
            .unwrap();

        // The descriptor is gone from the pending queue, so the worker stops
        // re-reading it on every poll.
        assert!(repository.list_pending().await.unwrap().is_empty());
        // Status is still queryable.
        let status = repository.read_status(&task.task_id).await.unwrap();
        assert_eq!(status.state, TaskState::Completed);
    }

    #[tokio::test]
    async fn failed_task_retries_then_becomes_terminal() {
        let (_dir, repository) = test_repository().await;
        let resource_id = mediaforge_core::resource_id_from_bytes(b"video");
        let (task, _) = repository
            .create_task(resource_id, sample_operation())
            .await
            .unwrap();
        repository
            .claim_task(&task.task_id, "worker-a")
            .await
            .unwrap();

        // First failure (attempt 1 of 2): requeued as Pending and reclaimable.
        let status = repository
            .mark_failed(&task.task_id, "boom".to_string(), 2)
            .await
            .unwrap();
        assert_eq!(status.state, TaskState::Pending);
        assert_eq!(status.attempts, 1);
        assert!(repository
            .read_lease(&task.task_id)
            .await
            .unwrap()
            .is_none());

        // Second failure reaches max_attempts: terminal Failed, dequeued.
        let status = repository
            .mark_failed(&task.task_id, "boom again".to_string(), 2)
            .await
            .unwrap();
        assert_eq!(status.state, TaskState::Failed);
        assert_eq!(status.attempts, 2);
        assert!(repository.list_pending().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn expired_lease_can_be_reclaimed() {
        let (_dir, repository) = test_repository().await;
        let resource_id = mediaforge_core::resource_id_from_bytes(b"video");
        let (task, _) = repository
            .create_task(resource_id, sample_operation())
            .await
            .unwrap();
        repository
            .claim_task(&task.task_id, "crashed-worker")
            .await
            .unwrap();

        // A fresh lease cannot be stolen.
        assert!(!repository
            .claim_or_reclaim_task(&task.task_id, "worker-b", Duration::from_secs(600))
            .await
            .unwrap());

        // With a zero timeout the (older-than-instant) lease is reclaimable.
        assert!(repository
            .claim_or_reclaim_task(&task.task_id, "worker-b", Duration::from_secs(0))
            .await
            .unwrap());
        let lease = repository.read_lease(&task.task_id).await.unwrap().unwrap();
        assert_eq!(lease.worker_id, "worker-b");
    }
}
