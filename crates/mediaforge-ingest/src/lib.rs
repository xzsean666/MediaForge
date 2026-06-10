//! Source inspection helpers.
//!
//! Uploads no longer stream through the API: clients PUT bytes directly to
//! object storage via a presigned URL, and the worker downloads the staged
//! object for finalization. This crate provides the pure, reusable pieces the
//! worker needs — content hashing, MIME detection, extension inference — plus a
//! convenience [`inspect_file`] that turns a downloaded file into an
//! [`InspectedSource`] (content-addressed resource id + metadata).

use mediaforge_core::{resource_id_from_sha256, CoreError};
use mediaforge_types::ResourceId;
use sha2::{Digest, Sha256};
use std::path::Path;
use tokio::io::AsyncReadExt;

#[derive(Debug, thiserror::Error)]
pub enum IngestError {
    #[error(transparent)]
    Core(#[from] CoreError),
    #[error("source file error: {0}")]
    Io(#[from] std::io::Error),
    #[error("source contains no bytes")]
    EmptyMedia,
}

pub type IngestResult<T> = Result<T, IngestError>;

/// Result of inspecting a downloaded source file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InspectedSource {
    pub resource_id: ResourceId,
    pub checksum_sha256: String,
    pub size_bytes: u64,
    pub mime_type: String,
    pub extension: String,
}

/// Inspects a downloaded source file: streams it once to compute the SHA-256
/// (yielding the content-addressed [`ResourceId`]) and resolves MIME type and
/// extension. `declared_content_type` and `file_name` are the client-supplied
/// hints captured at upload time. Memory use is bounded regardless of file size.
pub async fn inspect_file(
    path: impl AsRef<Path>,
    file_name: Option<&str>,
    declared_content_type: Option<String>,
) -> IngestResult<InspectedSource> {
    let (checksum_sha256, size_bytes) = file_sha256_and_size(path.as_ref()).await?;
    if size_bytes == 0 {
        return Err(IngestError::EmptyMedia);
    }
    let resource_id = resource_id_from_sha256(&checksum_sha256)?;
    let mime_type = detect_mime_type(file_name, declared_content_type);
    let extension = extension_for_file(file_name, &mime_type);
    Ok(InspectedSource {
        resource_id,
        checksum_sha256,
        size_bytes,
        mime_type,
        extension,
    })
}

/// Streams a file through SHA-256, returning `(hex_digest, size_bytes)` with
/// bounded memory use (does not read the whole file into memory).
pub async fn file_sha256_and_size(path: impl AsRef<Path>) -> IngestResult<(String, u64)> {
    let mut file = tokio::fs::File::open(path).await?;
    let mut hasher = Sha256::new();
    let mut size_bytes: u64 = 0;
    let mut buffer = vec![0u8; 1024 * 1024];

    loop {
        let read = file.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        size_bytes += read as u64;
    }

    Ok((hex_string(hasher.finalize().as_slice()), size_bytes))
}

/// Resolves a MIME type from the client-declared content type, falling back to
/// guessing from the file name, then to `application/octet-stream`.
pub fn detect_mime_type(file_name: Option<&str>, declared_content_type: Option<String>) -> String {
    if let Some(content_type) = declared_content_type.filter(|value| !value.trim().is_empty()) {
        return content_type;
    }

    file_name
        .and_then(|name| {
            mime_guess::from_path(name)
                .first_raw()
                .map(ToString::to_string)
        })
        .unwrap_or_else(|| "application/octet-stream".to_string())
}

/// Resolves a file extension from the file name, falling back to a small map
/// keyed by MIME type, then to `bin`.
pub fn extension_for_file(file_name: Option<&str>, mime_type: &str) -> String {
    if let Some(extension) = file_name.and_then(|name| Path::new(name).extension()) {
        if let Some(extension) = extension.to_str() {
            return extension.to_ascii_lowercase();
        }
    }

    match mime_type {
        "image/jpeg" => "jpg",
        "image/png" => "png",
        "image/webp" => "webp",
        "image/avif" => "avif",
        "video/mp4" => "mp4",
        "video/quicktime" => "mov",
        "video/x-matroska" => "mkv",
        "video/webm" => "webm",
        _ => "bin",
    }
    .to_string()
}

fn hex_string(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn inspect_file_hashes_content_and_resolves_resource_id() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("hello.txt");
        tokio::fs::write(&path, b"hello").await.unwrap();

        let inspected = inspect_file(&path, Some("hello.txt"), Some("text/plain".to_string()))
            .await
            .unwrap();

        assert_eq!(inspected.size_bytes, 5);
        assert_eq!(
            inspected.resource_id.as_ref(),
            "sha256:2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
        assert_eq!(inspected.mime_type, "text/plain");
    }

    #[tokio::test]
    async fn empty_source_is_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("empty.bin");
        tokio::fs::write(&path, b"").await.unwrap();
        let error = inspect_file(&path, None, None).await.unwrap_err();
        assert!(matches!(error, IngestError::EmptyMedia));
    }

    #[test]
    fn extension_falls_back_to_mime_type() {
        assert_eq!(extension_for_file(None, "video/mp4"), "mp4");
        assert_eq!(extension_for_file(Some("a.PNG"), "image/png"), "png");
        assert_eq!(extension_for_file(None, "application/unknown"), "bin");
    }
}
