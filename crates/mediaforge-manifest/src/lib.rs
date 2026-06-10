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
    #[error("manifest update for {0} kept conflicting after {1} attempts")]
    Conflict(ResourceId, u32),
}

/// How many times a read-modify-write is retried on a concurrent-update
/// (version) conflict before giving up.
const MAX_MANIFEST_UPDATE_ATTEMPTS: u32 = 5;

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
        Ok(self.read_with_version(resource_id).await?.0)
    }

    /// Reads a manifest together with its storage version token, for use with
    /// optimistic-concurrency updates ([`put_object_if_match`]).
    async fn read_with_version(
        &self,
        resource_id: &ResourceId,
    ) -> ManifestResult<(ResourceManifest, Option<String>)> {
        let key = manifest_key(resource_id)?;
        let (bytes, version) =
            self.storage
                .get_object_with_version(&key)
                .await
                .map_err(|error| match error {
                    StorageError::NotFound(_) => ManifestError::NotFound(resource_id.clone()),
                    other => ManifestError::Storage(other),
                })?;
        Ok((serde_json::from_slice(&bytes)?, version))
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
            resource_id: resource_id.clone(),
            media_kind,
            original,
            derived: Vec::new(),
            task_history: Vec::new(),
            created_at: now,
            updated_at: now,
        };

        // Conditional create: if another worker created it first, return theirs.
        let key = manifest_key(&resource_id)?;
        let created = self
            .storage
            .put_object_if_match(self.manifest_put_request(key, &manifest)?, None)
            .await?;
        if created {
            Ok(manifest)
        } else {
            self.read(&resource_id).await
        }
    }

    pub async fn merge_derived(
        &self,
        resource_id: &ResourceId,
        derived_object: DerivedMediaObject,
    ) -> ManifestResult<ResourceManifest> {
        self.update_with_retry(resource_id, |manifest| {
            manifest.derived.retain(|existing| {
                !(existing.result_id == derived_object.result_id
                    && existing.derived_kind == derived_object.derived_kind
                    && existing.object.object_key == derived_object.object.object_key)
            });
            manifest.derived.push(derived_object.clone());
        })
        .await
    }

    pub async fn record_task(
        &self,
        resource_id: &ResourceId,
        task_id: TaskId,
    ) -> ManifestResult<ResourceManifest> {
        self.update_with_retry(resource_id, |manifest| {
            if !manifest.task_history.contains(&task_id) {
                manifest.task_history.push(task_id.clone());
            }
        })
        .await
    }

    /// Read-modify-write a manifest under optimistic concurrency. `mutate` is
    /// applied to a fresh copy each attempt and the write only commits if the
    /// manifest was not changed by another writer in between; otherwise it
    /// retries. Prevents concurrent derived/task_history updates from clobbering
    /// each other (e.g. combined mode, multiple workers).
    async fn update_with_retry(
        &self,
        resource_id: &ResourceId,
        mutate: impl Fn(&mut ResourceManifest),
    ) -> ManifestResult<ResourceManifest> {
        let key = manifest_key(resource_id)?;
        for _ in 0..MAX_MANIFEST_UPDATE_ATTEMPTS {
            let (mut manifest, version) = self.read_with_version(resource_id).await?;
            mutate(&mut manifest);
            manifest.updated_at = Utc::now();
            let committed = self
                .storage
                .put_object_if_match(self.manifest_put_request(key.clone(), &manifest)?, version)
                .await?;
            if committed {
                return Ok(manifest);
            }
        }
        Err(ManifestError::Conflict(
            resource_id.clone(),
            MAX_MANIFEST_UPDATE_ATTEMPTS,
        ))
    }

    fn manifest_put_request(
        &self,
        key: String,
        manifest: &ResourceManifest,
    ) -> ManifestResult<PutObjectRequest> {
        Ok(PutObjectRequest {
            key,
            bytes: Bytes::from(serde_json::to_vec_pretty(manifest)?),
            content_type: Some("application/json".to_string()),
            metadata: BTreeMap::new(),
        })
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

    #[tokio::test]
    async fn concurrent_merges_do_not_lose_entries() {
        use mediaforge_types::{DerivedMediaKind, DerivedMediaObject, ResultId};

        let directory = tempfile::tempdir().unwrap();
        let storage = Arc::new(FilesystemObjectStorage::new(directory.path().to_path_buf()));
        let repository = ManifestRepository::new(storage);
        let resource_id = mediaforge_core::resource_id_from_bytes(b"asset");
        let original = MediaObject {
            object_key: "media/example/original/source.jpg".to_string(),
            file_name: None,
            mime_type: "image/jpeg".to_string(),
            size_bytes: 12,
            checksum_sha256: mediaforge_core::sha256_hex(b"asset"),
            metadata: MediaMetadata::default(),
        };
        repository
            .create_if_missing(resource_id.clone(), MediaKind::Image, original.clone())
            .await
            .unwrap();

        let derived = |suffix: &str| DerivedMediaObject {
            result_id: ResultId(format!("sha256:{suffix}")),
            derived_kind: DerivedMediaKind::ImageVariant,
            object: MediaObject {
                object_key: format!("media/example/image/{suffix}/output.webp"),
                ..original.clone()
            },
            parameters: serde_json::Value::Null,
            created_at: chrono::Utc::now(),
        };
        let derived_a = derived("aaa");
        let derived_b = derived("bbb");

        // Two concurrent merges against the same manifest; the CAS retry loop
        // must serialize them so neither write is lost.
        let repo_a = repository.clone();
        let repo_b = repository.clone();
        let id_a = resource_id.clone();
        let id_b = resource_id.clone();
        let merge_a = tokio::spawn(async move { repo_a.merge_derived(&id_a, derived_a).await });
        let merge_b = tokio::spawn(async move { repo_b.merge_derived(&id_b, derived_b).await });
        merge_a.await.unwrap().unwrap();
        merge_b.await.unwrap().unwrap();

        let manifest = repository.read(&resource_id).await.unwrap();
        assert_eq!(manifest.derived.len(), 2, "both derivatives must survive");
    }
}
