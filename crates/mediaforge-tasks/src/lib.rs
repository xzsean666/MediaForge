use bytes::Bytes;
use chrono::Utc;
use mediaforge_core::{
    task_completed_key, task_failed_key, task_id_from_parameters, task_lease_key, task_pending_key,
    task_status_key, CoreError,
};
use mediaforge_storage::{DynObjectStorage, PutObjectRequest, StorageError};
use mediaforge_types::{ResourceId, TaskDescriptor, TaskId, TaskOperation, TaskState, TaskStatus};
use serde::Serialize;
use std::collections::BTreeMap;

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

#[derive(Debug, Serialize)]
struct TaskLease<'a> {
    task_id: &'a TaskId,
    worker_id: &'a str,
    leased_at: chrono::DateTime<Utc>,
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

    pub async fn claim_task(&self, task_id: &TaskId, worker_id: &str) -> TaskResult<bool> {
        let lease = TaskLease {
            task_id,
            worker_id,
            leased_at: Utc::now(),
        };
        let created = self
            .put_json_if_absent(task_lease_key(task_id), &lease)
            .await?;
        if created {
            let descriptor = self.read_descriptor(task_id).await?;
            self.write_status(TaskStatus {
                task_id: task_id.clone(),
                resource_id: descriptor.resource_id,
                state: TaskState::Leased,
                message: Some(format!("leased by {worker_id}")),
                updated_at: Utc::now(),
            })
            .await?;
        }
        Ok(created)
    }

    pub async fn mark_running(&self, task_id: &TaskId) -> TaskResult<TaskStatus> {
        let descriptor = self.read_descriptor(task_id).await?;
        let status = TaskStatus {
            task_id: task_id.clone(),
            resource_id: descriptor.resource_id,
            state: TaskState::Running,
            message: None,
            updated_at: Utc::now(),
        };
        self.write_status(status.clone()).await?;
        Ok(status)
    }

    pub async fn mark_completed(
        &self,
        task_id: &TaskId,
        message: Option<String>,
    ) -> TaskResult<TaskStatus> {
        let descriptor = self.read_descriptor(task_id).await?;
        let status = TaskStatus {
            task_id: task_id.clone(),
            resource_id: descriptor.resource_id,
            state: TaskState::Completed,
            message,
            updated_at: Utc::now(),
        };
        self.write_status(status.clone()).await?;
        self.put_json(task_completed_key(task_id), &status).await?;
        Ok(status)
    }

    pub async fn mark_failed(&self, task_id: &TaskId, message: String) -> TaskResult<TaskStatus> {
        let descriptor = self.read_descriptor(task_id).await?;
        let status = TaskStatus {
            task_id: task_id.clone(),
            resource_id: descriptor.resource_id,
            state: TaskState::Failed,
            message: Some(message),
            updated_at: Utc::now(),
        };
        self.write_status(status.clone()).await?;
        self.put_json(task_failed_key(task_id), &status).await?;
        Ok(status)
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
}
