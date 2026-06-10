use async_trait::async_trait;
use bytes::Bytes;
use mediaforge_config::{AppConfig, S3Config, StorageBackend};
use std::collections::BTreeMap;
use std::fmt::{Debug, Display};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

pub type DynObjectStorage = Arc<dyn ObjectStorage>;

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("object not found: {0}")]
    NotFound(String),
    #[error("invalid object key: {0}")]
    InvalidObjectKey(String),
    #[error("filesystem storage error: {0}")]
    Filesystem(#[from] std::io::Error),
    #[error("s3 storage error: {0}")]
    S3(String),
    #[error("presign error: {0}")]
    Presign(String),
}

pub type StorageResult<T> = Result<T, StorageError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PutObjectRequest {
    pub key: String,
    pub bytes: Bytes,
    pub content_type: Option<String>,
    pub metadata: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredObjectMetadata {
    pub key: String,
    pub size_bytes: u64,
    pub content_type: Option<String>,
}

#[async_trait]
pub trait ObjectStorage: Send + Sync {
    async fn put_object(&self, request: PutObjectRequest) -> StorageResult<()>;
    async fn put_object_if_absent(&self, request: PutObjectRequest) -> StorageResult<bool>;

    /// Writes the contents of a local file without buffering the whole file in
    /// memory (S3 streams it via a file-backed body). Used for media bytes.
    async fn put_object_streaming(
        &self,
        key: &str,
        file_path: &Path,
        content_type: Option<String>,
    ) -> StorageResult<()>;

    /// Conditional write for optimistic concurrency. When `expected_version` is
    /// `Some`, the write only succeeds if the object's current version matches;
    /// when `None`, it only succeeds if the object is absent. Returns `false`
    /// on a precondition (version) conflict instead of erroring.
    async fn put_object_if_match(
        &self,
        request: PutObjectRequest,
        expected_version: Option<String>,
    ) -> StorageResult<bool>;

    async fn get_object(&self, key: &str) -> StorageResult<Bytes>;

    /// Streams an object to a local file without buffering it in memory.
    /// Returns the number of bytes written. Used by the worker to download
    /// large media (e.g. videos) for hashing and processing.
    async fn get_object_to_file(&self, key: &str, file_path: &Path) -> StorageResult<u64>;

    /// Like [`get_object`](Self::get_object) but also returns an opaque version
    /// token (S3 ETag / filesystem content hash) for use with
    /// [`put_object_if_match`](Self::put_object_if_match).
    async fn get_object_with_version(
        &self,
        key: &str,
    ) -> StorageResult<(Bytes, Option<String>)>;

    /// Server-side copy from one key to another (S3 CopyObject). Used to move a
    /// finalized upload from its staging key to its content-addressed key
    /// without round-tripping the bytes through the worker.
    async fn copy_object(&self, source_key: &str, destination_key: &str) -> StorageResult<()>;

    async fn object_metadata(&self, key: &str) -> StorageResult<StoredObjectMetadata>;
    async fn object_exists(&self, key: &str) -> StorageResult<bool>;
    async fn delete_object(&self, key: &str) -> StorageResult<()>;
    async fn list_keys(&self, prefix: &str) -> StorageResult<Vec<String>>;
    async fn presign_get_url(&self, key: &str, expires_in: Duration) -> StorageResult<String>;

    /// Presigned PUT URL letting a client upload bytes directly to the backend
    /// without proxying through the API.
    async fn presign_put_url(
        &self,
        key: &str,
        expires_in: Duration,
        content_type: Option<&str>,
    ) -> StorageResult<String>;
}

pub async fn create_object_storage(config: &AppConfig) -> StorageResult<DynObjectStorage> {
    match config.storage.backend {
        StorageBackend::S3 => Ok(Arc::new(
            S3ObjectStorage::from_config(&config.storage.s3).await?,
        )),
        StorageBackend::Filesystem => Ok(Arc::new(FilesystemObjectStorage::new(
            config.storage.filesystem_root.clone(),
        ))),
    }
}

#[derive(Debug, Clone)]
pub struct FilesystemObjectStorage {
    root: PathBuf,
    // Serializes conditional writes so the read-current-version / write pair in
    // `put_object_if_match` is atomic. The filesystem backend is single-process
    // (development only, per Agent.md); production uses S3 conditional writes.
    cas_lock: Arc<tokio::sync::Mutex<()>>,
}

impl FilesystemObjectStorage {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            cas_lock: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    fn path_for_key(&self, key: &str) -> StorageResult<PathBuf> {
        validate_object_key(key)?;
        Ok(self.root.join(key))
    }

    async fn current_version(&self, key: &str) -> StorageResult<Option<String>> {
        let path = self.path_for_key(key)?;
        match tokio::fs::read(&path).await {
            Ok(bytes) => Ok(Some(content_version(&bytes))),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(StorageError::Filesystem(error)),
        }
    }
}

#[async_trait]
impl ObjectStorage for FilesystemObjectStorage {
    async fn put_object(&self, request: PutObjectRequest) -> StorageResult<()> {
        let path = self.path_for_key(&request.key)?;
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(path, request.bytes).await?;
        Ok(())
    }

    async fn put_object_if_absent(&self, request: PutObjectRequest) -> StorageResult<bool> {
        use tokio::io::AsyncWriteExt;

        let path = self.path_for_key(&request.key)?;
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        let open_result = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .await;

        let mut file = match open_result {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => return Ok(false),
            Err(error) => return Err(StorageError::Filesystem(error)),
        };

        file.write_all(&request.bytes).await?;
        file.flush().await?;
        Ok(true)
    }

    async fn get_object(&self, key: &str) -> StorageResult<Bytes> {
        let path = self.path_for_key(key)?;
        match tokio::fs::read(path).await {
            Ok(bytes) => Ok(Bytes::from(bytes)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Err(StorageError::NotFound(key.to_string()))
            }
            Err(error) => Err(StorageError::Filesystem(error)),
        }
    }

    async fn get_object_to_file(&self, key: &str, file_path: &Path) -> StorageResult<u64> {
        let source = self.path_for_key(key)?;
        if let Some(parent) = file_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        match tokio::fs::copy(&source, file_path).await {
            Ok(bytes) => Ok(bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Err(StorageError::NotFound(key.to_string()))
            }
            Err(error) => Err(StorageError::Filesystem(error)),
        }
    }

    async fn object_metadata(&self, key: &str) -> StorageResult<StoredObjectMetadata> {
        let path = self.path_for_key(key)?;
        match tokio::fs::metadata(path).await {
            Ok(metadata) => Ok(StoredObjectMetadata {
                key: key.to_string(),
                size_bytes: metadata.len(),
                content_type: None,
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Err(StorageError::NotFound(key.to_string()))
            }
            Err(error) => Err(StorageError::Filesystem(error)),
        }
    }

    async fn object_exists(&self, key: &str) -> StorageResult<bool> {
        let path = self.path_for_key(key)?;
        match tokio::fs::metadata(path).await {
            Ok(_) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(StorageError::Filesystem(error)),
        }
    }

    async fn delete_object(&self, key: &str) -> StorageResult<()> {
        let path = self.path_for_key(key)?;
        match tokio::fs::remove_file(path).await {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(StorageError::Filesystem(error)),
        }
    }

    async fn list_keys(&self, prefix: &str) -> StorageResult<Vec<String>> {
        validate_object_key(prefix)?;
        let start = self.root.join(prefix);
        if tokio::fs::metadata(&start).await.is_err() {
            return Ok(Vec::new());
        }

        let root = self.root.clone();
        let mut pending = vec![start];
        let mut keys = Vec::new();

        while let Some(directory) = pending.pop() {
            let mut entries = tokio::fs::read_dir(directory).await?;
            while let Some(entry) = entries.next_entry().await? {
                let path = entry.path();
                let metadata = entry.metadata().await?;
                if metadata.is_dir() {
                    pending.push(path);
                } else if metadata.is_file() {
                    keys.push(path_to_key(&root, &path)?);
                }
            }
        }

        keys.sort();
        Ok(keys)
    }

    async fn put_object_streaming(
        &self,
        key: &str,
        file_path: &Path,
        _content_type: Option<String>,
    ) -> StorageResult<()> {
        let path = self.path_for_key(key)?;
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::copy(file_path, &path).await?;
        Ok(())
    }

    async fn put_object_if_match(
        &self,
        request: PutObjectRequest,
        expected_version: Option<String>,
    ) -> StorageResult<bool> {
        let _guard = self.cas_lock.lock().await;
        let current = self.current_version(&request.key).await?;
        if current != expected_version {
            return Ok(false);
        }
        self.put_object(request).await?;
        Ok(true)
    }

    async fn get_object_with_version(
        &self,
        key: &str,
    ) -> StorageResult<(Bytes, Option<String>)> {
        let bytes = self.get_object(key).await?;
        let version = content_version(&bytes);
        Ok((bytes, Some(version)))
    }

    async fn copy_object(&self, source_key: &str, destination_key: &str) -> StorageResult<()> {
        let source_path = self.path_for_key(source_key)?;
        let destination_path = self.path_for_key(destination_key)?;
        if let Some(parent) = destination_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        match tokio::fs::copy(&source_path, &destination_path).await {
            Ok(_) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Err(StorageError::NotFound(source_key.to_string()))
            }
            Err(error) => Err(StorageError::Filesystem(error)),
        }
    }

    async fn presign_get_url(&self, key: &str, _expires_in: Duration) -> StorageResult<String> {
        // The filesystem backend has no presigning; returning a local `file://`
        // path would leak server paths. Filesystem deployments use HMAC links.
        validate_object_key(key)?;
        Err(StorageError::Presign(
            "filesystem backend does not support presigned URLs; use the S3 backend".to_string(),
        ))
    }

    async fn presign_put_url(
        &self,
        key: &str,
        _expires_in: Duration,
        _content_type: Option<&str>,
    ) -> StorageResult<String> {
        validate_object_key(key)?;
        Err(StorageError::Presign(
            "filesystem backend does not support presigned uploads; use the S3 backend".to_string(),
        ))
    }
}

#[derive(Clone)]
pub struct S3ObjectStorage {
    client: aws_sdk_s3::Client,
    bucket: String,
}

impl S3ObjectStorage {
    pub async fn from_config(config: &S3Config) -> StorageResult<Self> {
        use aws_config::BehaviorVersion;
        use aws_credential_types::Credentials;
        use aws_types::region::Region;

        let mut loader = aws_config::defaults(BehaviorVersion::latest())
            .region(Region::new(config.region.clone()));

        if let Some(endpoint) = &config.endpoint {
            loader = loader.endpoint_url(endpoint);
        }

        if let (Some(access_key_id), Some(secret_access_key)) =
            (&config.access_key_id, &config.secret_access_key)
        {
            loader = loader.credentials_provider(Credentials::new(
                access_key_id,
                secret_access_key,
                None,
                None,
                "mediaforge",
            ));
        }

        let shared_config = loader.load().await;
        let s3_config = aws_sdk_s3::config::Builder::from(&shared_config)
            .force_path_style(config.force_path_style)
            .build();

        Ok(Self {
            client: aws_sdk_s3::Client::from_conf(s3_config),
            bucket: config.bucket.clone(),
        })
    }
}

#[async_trait]
impl ObjectStorage for S3ObjectStorage {
    async fn put_object(&self, request: PutObjectRequest) -> StorageResult<()> {
        use aws_sdk_s3::primitives::ByteStream;
        use std::collections::HashMap;

        validate_object_key(&request.key)?;
        let metadata: HashMap<String, String> = request.metadata.into_iter().collect();

        self.client
            .put_object()
            .bucket(&self.bucket)
            .key(&request.key)
            .set_content_type(request.content_type)
            .set_metadata(Some(metadata))
            .body(ByteStream::from(request.bytes))
            .send()
            .await
            .map_err(|error| StorageError::S3(describe_sdk_error(&error)))?;

        Ok(())
    }

    async fn put_object_if_absent(&self, request: PutObjectRequest) -> StorageResult<bool> {
        use aws_sdk_s3::primitives::ByteStream;
        use std::collections::HashMap;

        validate_object_key(&request.key)?;
        let fallback_request = request.clone();
        let metadata: HashMap<String, String> = request.metadata.into_iter().collect();

        let result = self
            .client
            .put_object()
            .bucket(&self.bucket)
            .key(&request.key)
            .if_none_match("*")
            .set_content_type(request.content_type)
            .set_metadata(Some(metadata))
            .body(ByteStream::from(request.bytes.to_vec()))
            .send()
            .await;

        match result {
            Ok(_) => Ok(true),
            Err(error) => {
                let description = describe_sdk_error(&error);
                if looks_like_precondition_failure(&description) {
                    Ok(false)
                } else if looks_like_not_implemented(&description) {
                    if self.object_exists(&fallback_request.key).await? {
                        Ok(false)
                    } else {
                        self.put_object(fallback_request).await?;
                        Ok(true)
                    }
                } else {
                    Err(StorageError::S3(description))
                }
            }
        }
    }

    async fn get_object(&self, key: &str) -> StorageResult<Bytes> {
        validate_object_key(key)?;
        let output = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
            .map_err(|error| {
                let description = describe_sdk_error(&error);
                if looks_like_not_found(&description) {
                    StorageError::NotFound(key.to_string())
                } else {
                    StorageError::S3(description)
                }
            })?;

        let bytes = output
            .body
            .collect()
            .await
            .map_err(|error| StorageError::S3(describe_sdk_error(&error)))?
            .into_bytes();
        Ok(bytes)
    }

    async fn get_object_to_file(&self, key: &str, file_path: &Path) -> StorageResult<u64> {
        use tokio::io::AsyncWriteExt;

        validate_object_key(key)?;
        let mut output = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
            .map_err(|error| {
                let description = describe_sdk_error(&error);
                if looks_like_not_found(&description) {
                    StorageError::NotFound(key.to_string())
                } else {
                    StorageError::S3(description)
                }
            })?;

        if let Some(parent) = file_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let mut file = tokio::fs::File::create(file_path).await?;
        let mut total: u64 = 0;
        while let Some(chunk) = output
            .body
            .try_next()
            .await
            .map_err(|error| StorageError::S3(describe_sdk_error(&error)))?
        {
            file.write_all(&chunk).await?;
            total += chunk.len() as u64;
        }
        file.flush().await?;
        Ok(total)
    }

    async fn object_metadata(&self, key: &str) -> StorageResult<StoredObjectMetadata> {
        validate_object_key(key)?;
        let output = self
            .client
            .head_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
            .map_err(|error| {
                let description = describe_sdk_error(&error);
                if looks_like_not_found(&description) {
                    StorageError::NotFound(key.to_string())
                } else {
                    StorageError::S3(description)
                }
            })?;

        Ok(StoredObjectMetadata {
            key: key.to_string(),
            size_bytes: output.content_length().unwrap_or_default() as u64,
            content_type: output.content_type().map(ToString::to_string),
        })
    }

    async fn object_exists(&self, key: &str) -> StorageResult<bool> {
        validate_object_key(key)?;
        let result = self
            .client
            .head_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await;

        match result {
            Ok(_) => Ok(true),
            Err(error) => {
                let description = describe_sdk_error(&error);
                if looks_like_not_found(&description) {
                    Ok(false)
                } else {
                    Err(StorageError::S3(description))
                }
            }
        }
    }

    async fn delete_object(&self, key: &str) -> StorageResult<()> {
        validate_object_key(key)?;
        self.client
            .delete_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
            .map_err(|error| StorageError::S3(describe_sdk_error(&error)))?;
        Ok(())
    }

    async fn list_keys(&self, prefix: &str) -> StorageResult<Vec<String>> {
        validate_object_key(prefix)?;
        let mut keys = Vec::new();
        let mut continuation_token = None;

        loop {
            let response = self
                .client
                .list_objects_v2()
                .bucket(&self.bucket)
                .prefix(prefix)
                .set_continuation_token(continuation_token)
                .send()
                .await
                .map_err(|error| StorageError::S3(describe_sdk_error(&error)))?;

            for object in response.contents() {
                if let Some(key) = object.key() {
                    keys.push(key.to_string());
                }
            }

            continuation_token = response.next_continuation_token().map(ToString::to_string);
            if continuation_token.is_none() {
                break;
            }
        }

        Ok(keys)
    }

    async fn put_object_streaming(
        &self,
        key: &str,
        file_path: &Path,
        content_type: Option<String>,
    ) -> StorageResult<()> {
        use aws_sdk_s3::primitives::ByteStream;

        validate_object_key(key)?;
        let body = ByteStream::from_path(file_path)
            .await
            .map_err(|error| StorageError::S3(error.to_string()))?;

        self.client
            .put_object()
            .bucket(&self.bucket)
            .key(key)
            .set_content_type(content_type)
            .body(body)
            .send()
            .await
            .map_err(|error| StorageError::S3(describe_sdk_error(&error)))?;
        Ok(())
    }

    async fn put_object_if_match(
        &self,
        request: PutObjectRequest,
        expected_version: Option<String>,
    ) -> StorageResult<bool> {
        use aws_sdk_s3::primitives::ByteStream;
        use std::collections::HashMap;

        validate_object_key(&request.key)?;
        let metadata: HashMap<String, String> = request.metadata.into_iter().collect();
        let mut builder = self
            .client
            .put_object()
            .bucket(&self.bucket)
            .key(&request.key)
            .set_content_type(request.content_type)
            .set_metadata(Some(metadata))
            .body(ByteStream::from(request.bytes));

        match expected_version {
            Some(version) => builder = builder.if_match(version),
            None => builder = builder.if_none_match("*"),
        }

        match builder.send().await {
            Ok(_) => Ok(true),
            Err(error) => {
                let description = describe_sdk_error(&error);
                if looks_like_precondition_failure(&description) {
                    Ok(false)
                } else {
                    Err(StorageError::S3(description))
                }
            }
        }
    }

    async fn get_object_with_version(
        &self,
        key: &str,
    ) -> StorageResult<(Bytes, Option<String>)> {
        validate_object_key(key)?;
        let output = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
            .map_err(|error| {
                let description = describe_sdk_error(&error);
                if looks_like_not_found(&description) {
                    StorageError::NotFound(key.to_string())
                } else {
                    StorageError::S3(description)
                }
            })?;

        let version = output.e_tag().map(ToString::to_string);
        let bytes = output
            .body
            .collect()
            .await
            .map_err(|error| StorageError::S3(describe_sdk_error(&error)))?
            .into_bytes();
        Ok((bytes, version))
    }

    async fn copy_object(&self, source_key: &str, destination_key: &str) -> StorageResult<()> {
        validate_object_key(source_key)?;
        validate_object_key(destination_key)?;
        let copy_source = format!("{}/{}", self.bucket, encode_copy_source(source_key));

        self.client
            .copy_object()
            .bucket(&self.bucket)
            .key(destination_key)
            .copy_source(copy_source)
            .send()
            .await
            .map_err(|error| {
                let description = describe_sdk_error(&error);
                if looks_like_not_found(&description) {
                    StorageError::NotFound(source_key.to_string())
                } else {
                    StorageError::S3(description)
                }
            })?;
        Ok(())
    }

    async fn presign_get_url(&self, key: &str, expires_in: Duration) -> StorageResult<String> {
        use aws_sdk_s3::presigning::PresigningConfig;

        validate_object_key(key)?;
        let presigning_config = PresigningConfig::expires_in(expires_in)
            .map_err(|error| StorageError::Presign(describe_sdk_error(&error)))?;
        let presigned_request = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(key)
            .presigned(presigning_config)
            .await
            .map_err(|error| StorageError::Presign(describe_sdk_error(&error)))?;

        Ok(presigned_request.uri().to_string())
    }

    async fn presign_put_url(
        &self,
        key: &str,
        expires_in: Duration,
        content_type: Option<&str>,
    ) -> StorageResult<String> {
        use aws_sdk_s3::presigning::PresigningConfig;

        validate_object_key(key)?;
        let presigning_config = PresigningConfig::expires_in(expires_in)
            .map_err(|error| StorageError::Presign(describe_sdk_error(&error)))?;
        let presigned_request = self
            .client
            .put_object()
            .bucket(&self.bucket)
            .key(key)
            .set_content_type(content_type.map(ToString::to_string))
            .presigned(presigning_config)
            .await
            .map_err(|error| StorageError::Presign(describe_sdk_error(&error)))?;

        Ok(presigned_request.uri().to_string())
    }
}

fn validate_object_key(key: &str) -> StorageResult<()> {
    if key.is_empty()
        || key.starts_with('/')
        || key.contains('\\')
        || key.split('/').any(|part| part == "..")
    {
        return Err(StorageError::InvalidObjectKey(key.to_string()));
    }
    Ok(())
}

fn path_to_key(root: &Path, path: &Path) -> StorageResult<String> {
    let relative_path = path
        .strip_prefix(root)
        .map_err(|_| StorageError::InvalidObjectKey(path.display().to_string()))?;
    Ok(relative_path.to_string_lossy().replace('\\', "/"))
}

/// Opaque version token for the filesystem backend: the SHA-256 of the current
/// contents. Stable and cheap to compare for the development CAS path.
fn content_version(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

/// Percent-encodes an S3 copy-source key, leaving path separators intact.
fn encode_copy_source(key: &str) -> String {
    let mut encoded = String::with_capacity(key.len());
    for byte in key.bytes() {
        match byte {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                encoded.push(byte as char)
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

fn looks_like_not_found(error: &str) -> bool {
    error.contains("NotFound")
        || error.contains("NoSuchKey")
        || error.contains("404")
        || error.contains("not found")
}

fn looks_like_precondition_failure(error: &str) -> bool {
    error.contains("PreconditionFailed")
        || error.contains("Precondition Failed")
        || error.contains("412")
}

fn looks_like_not_implemented(error: &str) -> bool {
    error.contains("NotImplemented") || error.contains("not implemented")
}

fn describe_sdk_error(error: &(impl Debug + Display)) -> String {
    let display = error.to_string();
    let debug = format!("{error:?}");
    if debug == display {
        display
    } else {
        format!("{display}; debug={debug}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn filesystem_put_if_absent_is_idempotent() {
        let directory = tempfile::tempdir().unwrap();
        let storage = FilesystemObjectStorage::new(directory.path().to_path_buf());
        let request = PutObjectRequest {
            key: "media/example.txt".to_string(),
            bytes: Bytes::from_static(b"first"),
            content_type: Some("text/plain".to_string()),
            metadata: BTreeMap::new(),
        };

        assert!(storage.put_object_if_absent(request.clone()).await.unwrap());
        assert!(!storage.put_object_if_absent(request).await.unwrap());
        assert_eq!(
            storage.get_object("media/example.txt").await.unwrap(),
            Bytes::from_static(b"first")
        );
    }

    #[tokio::test]
    async fn filesystem_lists_prefix_keys() {
        let directory = tempfile::tempdir().unwrap();
        let storage = FilesystemObjectStorage::new(directory.path().to_path_buf());

        storage
            .put_object(PutObjectRequest {
                key: "tasks/pending/a.json".to_string(),
                bytes: Bytes::from_static(b"{}"),
                content_type: None,
                metadata: BTreeMap::new(),
            })
            .await
            .unwrap();
        storage
            .put_object(PutObjectRequest {
                key: "tasks/pending/nested/b.json".to_string(),
                bytes: Bytes::from_static(b"{}"),
                content_type: None,
                metadata: BTreeMap::new(),
            })
            .await
            .unwrap();

        let keys = storage.list_keys("tasks/pending").await.unwrap();
        assert_eq!(
            keys,
            vec![
                "tasks/pending/a.json".to_string(),
                "tasks/pending/nested/b.json".to_string()
            ]
        );
    }

    #[tokio::test]
    async fn filesystem_cas_rejects_stale_version() {
        let directory = tempfile::tempdir().unwrap();
        let storage = FilesystemObjectStorage::new(directory.path().to_path_buf());
        let make = |body: &'static [u8]| PutObjectRequest {
            key: "manifest.json".to_string(),
            bytes: Bytes::from_static(body),
            content_type: None,
            metadata: BTreeMap::new(),
        };

        // Create: expected_version None succeeds only when absent.
        assert!(storage.put_object_if_match(make(b"v1"), None).await.unwrap());
        assert!(!storage.put_object_if_match(make(b"v2"), None).await.unwrap());

        let (_, version) = storage
            .get_object_with_version("manifest.json")
            .await
            .unwrap();
        // Matching version succeeds; stale version is rejected.
        assert!(storage
            .put_object_if_match(make(b"v3"), version.clone())
            .await
            .unwrap());
        assert!(!storage
            .put_object_if_match(make(b"v4"), version)
            .await
            .unwrap());
        assert_eq!(
            storage.get_object("manifest.json").await.unwrap(),
            Bytes::from_static(b"v3")
        );
    }

    #[tokio::test]
    async fn filesystem_copy_object_duplicates_bytes() {
        let directory = tempfile::tempdir().unwrap();
        let storage = FilesystemObjectStorage::new(directory.path().to_path_buf());
        storage
            .put_object(PutObjectRequest {
                key: "uploads/staging/abc/source".to_string(),
                bytes: Bytes::from_static(b"payload"),
                content_type: None,
                metadata: BTreeMap::new(),
            })
            .await
            .unwrap();

        storage
            .copy_object("uploads/staging/abc/source", "media/final/source.bin")
            .await
            .unwrap();
        assert_eq!(
            storage.get_object("media/final/source.bin").await.unwrap(),
            Bytes::from_static(b"payload")
        );

        let missing = storage
            .copy_object("uploads/staging/missing", "media/x")
            .await
            .unwrap_err();
        assert!(matches!(missing, StorageError::NotFound(_)));
    }

    #[test]
    fn encode_copy_source_preserves_slashes() {
        assert_eq!(
            encode_copy_source("uploads/staging/id/source"),
            "uploads/staging/id/source"
        );
        assert_eq!(
            encode_copy_source("media/sha256:abc/source"),
            "media/sha256%3Aabc/source"
        );
    }

    #[tokio::test]
    async fn filesystem_presign_is_unsupported() {
        let directory = tempfile::tempdir().unwrap();
        let storage = FilesystemObjectStorage::new(directory.path().to_path_buf());
        let error = storage
            .presign_put_url("media/x", Duration::from_secs(60), None)
            .await
            .unwrap_err();
        assert!(matches!(error, StorageError::Presign(_)));
    }

    #[tokio::test]
    async fn filesystem_rejects_parent_directory_keys() {
        let directory = tempfile::tempdir().unwrap();
        let storage = FilesystemObjectStorage::new(directory.path().to_path_buf());

        let error = storage.get_object("../secret").await.unwrap_err();
        assert!(matches!(error, StorageError::InvalidObjectKey(_)));
    }
}
