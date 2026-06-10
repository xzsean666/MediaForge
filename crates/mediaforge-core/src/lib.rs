use mediaforge_types::{ResourceId, ResultId, TaskId};
use serde::Serialize;
use sha2::{Digest, Sha256};

#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("resource id must use sha256:<64 lowercase hex chars> format")]
    InvalidResourceId,
    #[error("sha256 hash must contain 64 lowercase hex chars")]
    InvalidSha256,
    #[error("failed to serialize canonical parameters: {0}")]
    CanonicalSerialization(#[from] serde_json::Error),
}

pub type CoreResult<T> = Result<T, CoreError>;

pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

pub fn resource_id_from_bytes(bytes: &[u8]) -> ResourceId {
    ResourceId(format!("sha256:{}", sha256_hex(bytes)))
}

pub fn resource_id_from_sha256(hash_hex: &str) -> CoreResult<ResourceId> {
    validate_sha256_hex(hash_hex)?;
    Ok(ResourceId(format!("sha256:{hash_hex}")))
}

pub fn canonical_json<T>(value: &T) -> CoreResult<String>
where
    T: Serialize,
{
    Ok(serde_json::to_string(value)?)
}

pub fn result_id_from_parameters<T>(
    source_resource_id: &ResourceId,
    operation_name: &str,
    parameters: &T,
) -> CoreResult<ResultId>
where
    T: Serialize,
{
    let canonical_parameters = canonical_json(parameters)?;
    let identity_input = format!(
        "result\nresource={}\noperation={operation_name}\nparameters={canonical_parameters}",
        source_resource_id.as_ref()
    );
    Ok(ResultId(format!(
        "sha256:{}",
        sha256_hex(identity_input.as_bytes())
    )))
}

pub fn task_id_from_parameters<T>(
    source_resource_id: &ResourceId,
    operation_name: &str,
    parameters: &T,
) -> CoreResult<TaskId>
where
    T: Serialize,
{
    let canonical_parameters = canonical_json(parameters)?;
    let identity_input = format!(
        "task\nresource={}\noperation={operation_name}\nparameters={canonical_parameters}",
        source_resource_id.as_ref()
    );
    Ok(TaskId(format!(
        "sha256:{}",
        sha256_hex(identity_input.as_bytes())
    )))
}

pub fn validate_resource_id(resource_id: &ResourceId) -> CoreResult<()> {
    let hash_hex = resource_hash_hex(resource_id)?;
    validate_sha256_hex(hash_hex)
}

pub fn resource_hash_hex(resource_id: &ResourceId) -> CoreResult<&str> {
    let Some(hash_hex) = resource_id.as_ref().strip_prefix("sha256:") else {
        return Err(CoreError::InvalidResourceId);
    };
    validate_sha256_hex(hash_hex)?;
    Ok(hash_hex)
}

pub fn media_prefix(resource_id: &ResourceId) -> CoreResult<String> {
    let hash_hex = resource_hash_hex(resource_id)?;
    Ok(format!(
        "media/sha256/{}/{}/{}",
        &hash_hex[0..2],
        &hash_hex[2..4],
        resource_id.as_ref()
    ))
}

pub fn manifest_key(resource_id: &ResourceId) -> CoreResult<String> {
    Ok(format!("{}/manifest.json", media_prefix(resource_id)?))
}

pub fn original_object_key(resource_id: &ResourceId, extension: &str) -> CoreResult<String> {
    Ok(format!(
        "{}/original/source.{}",
        media_prefix(resource_id)?,
        normalize_extension(extension)
    ))
}

pub fn image_result_object_key(
    resource_id: &ResourceId,
    result_id: &ResultId,
    extension: &str,
) -> CoreResult<String> {
    Ok(format!(
        "{}/image/{}/output.{}",
        media_prefix(resource_id)?,
        result_id.as_ref(),
        normalize_extension(extension)
    ))
}

pub fn video_result_object_key(
    resource_id: &ResourceId,
    result_id: &ResultId,
    extension: &str,
) -> CoreResult<String> {
    Ok(format!(
        "{}/video/{}/output.{}",
        media_prefix(resource_id)?,
        result_id.as_ref(),
        normalize_extension(extension)
    ))
}

pub fn cover_object_key(
    resource_id: &ResourceId,
    result_id: &ResultId,
    extension: &str,
) -> CoreResult<String> {
    Ok(format!(
        "{}/cover/{}/cover.{}",
        media_prefix(resource_id)?,
        result_id.as_ref(),
        normalize_extension(extension)
    ))
}

pub fn screenshot_object_key(
    resource_id: &ResourceId,
    result_id: &ResultId,
    index: usize,
    extension: &str,
) -> CoreResult<String> {
    Ok(format!(
        "{}/screenshots/{}/shot-{index:06}.{}",
        media_prefix(resource_id)?,
        result_id.as_ref(),
        normalize_extension(extension)
    ))
}

pub fn hls_master_playlist_key(
    resource_id: &ResourceId,
    result_id: &ResultId,
) -> CoreResult<String> {
    Ok(format!(
        "{}/hls/{}/master.m3u8",
        media_prefix(resource_id)?,
        result_id.as_ref()
    ))
}

pub fn task_pending_key(task_id: &TaskId) -> String {
    format!("tasks/pending/{}.json", task_id.as_ref())
}

pub fn task_status_key(task_id: &TaskId) -> String {
    format!("tasks/status/{}.json", task_id.as_ref())
}

pub fn task_lease_key(task_id: &TaskId) -> String {
    format!("tasks/leases/{}.json", task_id.as_ref())
}

pub fn task_completed_key(task_id: &TaskId) -> String {
    format!("tasks/completed/{}.json", task_id.as_ref())
}

pub fn task_failed_key(task_id: &TaskId) -> String {
    format!("tasks/failed/{}.json", task_id.as_ref())
}

fn validate_sha256_hex(hash_hex: &str) -> CoreResult<()> {
    if hash_hex.len() != 64 {
        return Err(CoreError::InvalidSha256);
    }
    if !hash_hex
        .bytes()
        .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    {
        return Err(CoreError::InvalidSha256);
    }
    Ok(())
}

fn normalize_extension(extension: &str) -> String {
    extension.trim_start_matches('.').to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use mediaforge_types::{ImageOutputFormat, ImageProcessingRequest};

    #[test]
    fn resource_id_is_content_addressed() {
        let resource_id = resource_id_from_bytes(b"hello");
        assert_eq!(
            resource_id.as_ref(),
            "sha256:2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    #[test]
    fn media_prefix_uses_hash_partitions() {
        let resource_id = resource_id_from_bytes(b"hello");
        assert_eq!(
            media_prefix(&resource_id).unwrap(),
            "media/sha256/2c/f2/sha256:2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    #[test]
    fn result_id_is_deterministic_for_same_parameters() {
        let resource_id = resource_id_from_bytes(b"image");
        let request = ImageProcessingRequest {
            output_format: ImageOutputFormat::Webp,
            quality: Some(80),
            operations: Vec::new(),
        };

        let first = result_id_from_parameters(&resource_id, "image_processing", &request).unwrap();
        let second = result_id_from_parameters(&resource_id, "image_processing", &request).unwrap();

        assert_eq!(first, second);
    }

    #[test]
    fn task_id_differs_from_result_id() {
        let resource_id = resource_id_from_bytes(b"video");
        let parameters = serde_json::json!({"codec": "h264"});
        let task_id =
            task_id_from_parameters(&resource_id, "video_processing", &parameters).unwrap();
        let result_id =
            result_id_from_parameters(&resource_id, "video_processing", &parameters).unwrap();

        assert_ne!(task_id.as_ref(), result_id.as_ref());
    }
}
