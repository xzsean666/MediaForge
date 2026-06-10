use bytes::Bytes;
use mediaforge_core::{resource_id_from_sha256, sha256_hex, CoreError};
use mediaforge_types::ResourceId;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum IngestError {
    #[error(transparent)]
    Core(#[from] CoreError),
    #[error("temporary file error: {0}")]
    Io(#[from] std::io::Error),
    #[error("ingested media contains no bytes")]
    EmptyMedia,
}

pub type IngestResult<T> = Result<T, IngestError>;

#[derive(Debug)]
pub struct TemporaryMediaFile {
    path: PathBuf,
}

impl TemporaryMediaFile {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TemporaryMediaFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[derive(Debug)]
pub struct IngestedMedia {
    pub resource_id: ResourceId,
    pub checksum_sha256: String,
    pub size_bytes: u64,
    pub mime_type: String,
    pub extension: String,
    pub file_name: Option<String>,
    pub temporary_file: TemporaryMediaFile,
}

pub struct IngestSession {
    file: tokio::fs::File,
    path: PathBuf,
    hasher: Sha256,
    size_bytes: u64,
    file_name: Option<String>,
    declared_content_type: Option<String>,
}

impl IngestSession {
    pub async fn start(
        temp_directory: impl AsRef<Path>,
        file_name: Option<String>,
        declared_content_type: Option<String>,
    ) -> IngestResult<Self> {
        tokio::fs::create_dir_all(temp_directory.as_ref()).await?;
        let path = temp_directory
            .as_ref()
            .join(format!("ingest-{}.bin", Uuid::new_v4()));
        let file = tokio::fs::File::create(&path).await?;

        Ok(Self {
            file,
            path,
            hasher: Sha256::new(),
            size_bytes: 0,
            file_name,
            declared_content_type,
        })
    }

    pub async fn write_chunk(&mut self, bytes: Bytes) -> IngestResult<()> {
        self.hasher.update(&bytes);
        self.size_bytes += bytes.len() as u64;
        self.file.write_all(&bytes).await?;
        Ok(())
    }

    pub async fn finish(mut self) -> IngestResult<IngestedMedia> {
        if self.size_bytes == 0 {
            return Err(IngestError::EmptyMedia);
        }

        self.file.flush().await?;
        self.file.sync_all().await?;
        drop(self.file);

        let checksum_sha256 = hex_string(self.hasher.finalize().as_slice());
        let resource_id = resource_id_from_sha256(&checksum_sha256)?;
        let mime_type = detect_mime_type(self.file_name.as_deref(), self.declared_content_type);
        let extension = extension_for_file(self.file_name.as_deref(), &mime_type);

        Ok(IngestedMedia {
            resource_id,
            checksum_sha256,
            size_bytes: self.size_bytes,
            mime_type,
            extension,
            file_name: self.file_name,
            temporary_file: TemporaryMediaFile { path: self.path },
        })
    }
}

pub async fn ingest_bytes(
    bytes: Bytes,
    temp_directory: impl AsRef<Path>,
    file_name: Option<String>,
    declared_content_type: Option<String>,
) -> IngestResult<IngestedMedia> {
    let mut session =
        IngestSession::start(temp_directory, file_name, declared_content_type).await?;
    session.write_chunk(bytes).await?;
    session.finish().await
}

pub async fn file_sha256_and_size(path: impl AsRef<Path>) -> IngestResult<(String, u64)> {
    let bytes = tokio::fs::read(path).await?;
    Ok((sha256_hex(&bytes), bytes.len() as u64))
}

fn detect_mime_type(file_name: Option<&str>, declared_content_type: Option<String>) -> String {
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

fn extension_for_file(file_name: Option<&str>, mime_type: &str) -> String {
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
    async fn ingest_bytes_writes_temp_file_and_hashes_content() {
        let directory = tempfile::tempdir().unwrap();
        let media = ingest_bytes(
            Bytes::from_static(b"hello"),
            directory.path(),
            Some("hello.txt".to_string()),
            Some("text/plain".to_string()),
        )
        .await
        .unwrap();

        assert_eq!(media.size_bytes, 5);
        assert_eq!(
            media.resource_id.as_ref(),
            "sha256:2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
        assert!(media.temporary_file.path().exists());
    }
}
