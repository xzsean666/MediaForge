use bytes::Bytes;
use chrono::Utc;
use mediaforge_core::{manifest_key, CoreError};
use mediaforge_storage::{DynObjectStorage, PutObjectRequest, StorageError};
use mediaforge_types::{DerivedMediaObject, MediaObject, ResourceId, ResourceManifest, TaskId};
use std::collections::BTreeMap;

#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error(transparent)]
    Core(#[from] CoreError),
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error("manifest not found for resource: {0}")]
    NotFound(ResourceId),
    #[error("failed to serialize manifest: {0}")]
    Serialize(#[from] serde_json::Error),
}

pub type ManifestResult<T> = Result<T, ManifestError>;

#[derive(Clone)]
pub struct ManifestRepository {
    storage: DynObjectStorage,
}

impl ManifestRepository {
    pub fn new(storage: DynObjectStorage) -> Self {
        Self { storage }
    }

    pub async fn read(&self, resource_id: &ResourceId) -> ManifestResult<ResourceManifest> {
        let key = manifest_key(resource_id)?;
        let bytes = self
            .storage
            .get_object(&key)
            .await
            .map_err(|error| match error {
                StorageError::NotFound(_) => ManifestError::NotFound(resource_id.clone()),
                other => ManifestError::Storage(other),
            })?;
        Ok(serde_json::from_slice(&bytes)?)
    }

    pub async fn exists(&self, resource_id: &ResourceId) -> ManifestResult<bool> {
        let key = manifest_key(resource_id)?;
        Ok(self.storage.object_exists(&key).await?)
    }

    pub async fn write(&self, manifest: &ResourceManifest) -> ManifestResult<()> {
        let key = manifest_key(&manifest.resource_id)?;
        let bytes = Bytes::from(serde_json::to_vec_pretty(manifest)?);
        self.storage
            .put_object(PutObjectRequest {
                key,
                bytes,
                content_type: Some("application/json".to_string()),
                metadata: BTreeMap::new(),
            })
            .await?;
        Ok(())
    }

    pub async fn create_if_missing(
        &self,
        resource_id: ResourceId,
        media_kind: mediaforge_types::MediaKind,
        original: MediaObject,
    ) -> ManifestResult<ResourceManifest> {
        if let Ok(existing) = self.read(&resource_id).await {
            return Ok(existing);
        }

        let now = Utc::now();
        let manifest = ResourceManifest {
            resource_id,
            media_kind,
            original,
            derived: Vec::new(),
            task_history: Vec::new(),
            created_at: now,
            updated_at: now,
        };
        self.write(&manifest).await?;
        Ok(manifest)
    }

    pub async fn merge_derived(
        &self,
        resource_id: &ResourceId,
        derived_object: DerivedMediaObject,
    ) -> ManifestResult<ResourceManifest> {
        let mut manifest = self.read(resource_id).await?;
        manifest.derived.retain(|existing| {
            !(existing.result_id == derived_object.result_id
                && existing.derived_kind == derived_object.derived_kind
                && existing.object.object_key == derived_object.object.object_key)
        });
        manifest.derived.push(derived_object);
        manifest.updated_at = Utc::now();
        self.write(&manifest).await?;
        Ok(manifest)
    }

    pub async fn record_task(
        &self,
        resource_id: &ResourceId,
        task_id: TaskId,
    ) -> ManifestResult<ResourceManifest> {
        let mut manifest = self.read(resource_id).await?;
        if !manifest.task_history.contains(&task_id) {
            manifest.task_history.push(task_id);
            manifest.updated_at = Utc::now();
            self.write(&manifest).await?;
        }
        Ok(manifest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mediaforge_storage::FilesystemObjectStorage;
    use mediaforge_types::{MediaKind, MediaMetadata};
    use std::sync::Arc;

    #[tokio::test]
    async fn manifest_round_trip_uses_storage() {
        let directory = tempfile::tempdir().unwrap();
        let storage = Arc::new(FilesystemObjectStorage::new(directory.path().to_path_buf()));
        let repository = ManifestRepository::new(storage);
        let resource_id = mediaforge_core::resource_id_from_bytes(b"asset");
        let original = MediaObject {
            object_key: "media/example/original/source.jpg".to_string(),
            file_name: Some("source.jpg".to_string()),
            mime_type: "image/jpeg".to_string(),
            size_bytes: 12,
            checksum_sha256: mediaforge_core::sha256_hex(b"asset"),
            metadata: MediaMetadata::default(),
        };

        let created = repository
            .create_if_missing(resource_id.clone(), MediaKind::Image, original)
            .await
            .unwrap();
        let loaded = repository.read(&resource_id).await.unwrap();

        assert_eq!(created.resource_id, loaded.resource_id);
        assert_eq!(loaded.media_kind, MediaKind::Image);
    }
}
