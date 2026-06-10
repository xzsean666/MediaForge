use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use hmac::{Hmac, Mac};
use mediaforge_config::AppConfig;
use mediaforge_storage::DynObjectStorage;
use mediaforge_types::{GeneratedLink, LinkKind, LinkPolicy};
use sha2::Sha256;
use std::time::Duration;

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, thiserror::Error)]
pub enum LinkError {
    #[error("cdn base url is required for this link policy")]
    MissingCdnBaseUrl,
    #[error("invalid HMAC key")]
    InvalidSigningKey,
    #[error("storage presign failed: {0}")]
    Storage(#[from] mediaforge_storage::StorageError),
}

pub type LinkResult<T> = Result<T, LinkError>;

#[derive(Clone)]
pub struct LinkGenerator {
    cdn_base_url: Option<String>,
    signing_secret: String,
    storage: DynObjectStorage,
}

impl LinkGenerator {
    pub fn new(config: &AppConfig, storage: DynObjectStorage) -> Self {
        Self {
            cdn_base_url: config.cdn_base_url.clone(),
            signing_secret: config.link_signing_secret.clone(),
            storage,
        }
    }

    pub async fn generate_link(
        &self,
        object_key: &str,
        policy: LinkPolicy,
    ) -> LinkResult<GeneratedLink> {
        match policy {
            LinkPolicy::Public => Ok(GeneratedLink {
                url: self.public_url(object_key)?,
                link_kind: LinkKind::Public,
                expires_at: None,
            }),
            LinkPolicy::Temporary { expires_in_seconds } => {
                let expires_at = Utc::now() + ChronoDuration::seconds(expires_in_seconds as i64);
                Ok(GeneratedLink {
                    url: self.signed_cdn_url(object_key, expires_at, "read")?,
                    link_kind: LinkKind::Temporary,
                    expires_at: Some(expires_at),
                })
            }
            LinkPolicy::Signed {
                expires_in_seconds,
                permission,
            } => {
                let expires_at = Utc::now() + ChronoDuration::seconds(expires_in_seconds as i64);
                Ok(GeneratedLink {
                    url: self.signed_cdn_url(object_key, expires_at, &permission)?,
                    link_kind: LinkKind::Signed,
                    expires_at: Some(expires_at),
                })
            }
            LinkPolicy::StoragePresigned { expires_in_seconds } => {
                let expires_at = Utc::now() + ChronoDuration::seconds(expires_in_seconds as i64);
                let url = self
                    .storage
                    .presign_get_url(object_key, Duration::from_secs(expires_in_seconds))
                    .await?;
                Ok(GeneratedLink {
                    url,
                    link_kind: LinkKind::StoragePresigned,
                    expires_at: Some(expires_at),
                })
            }
        }
    }

    pub fn validate_signature(
        &self,
        object_key: &str,
        expires_at_unix: i64,
        permission: &str,
        signature: &str,
        now: DateTime<Utc>,
    ) -> LinkResult<bool> {
        if now.timestamp() > expires_at_unix {
            return Ok(false);
        }

        let expected = self.signature(object_key, expires_at_unix, permission)?;
        Ok(expected == signature)
    }

    fn public_url(&self, object_key: &str) -> LinkResult<String> {
        let base_url = self
            .cdn_base_url
            .as_ref()
            .ok_or(LinkError::MissingCdnBaseUrl)?;
        Ok(join_url(base_url, object_key))
    }

    fn signed_cdn_url(
        &self,
        object_key: &str,
        expires_at: DateTime<Utc>,
        permission: &str,
    ) -> LinkResult<String> {
        let base_url = self.public_url(object_key)?;
        let expires = expires_at.timestamp();
        let signature = self.signature(object_key, expires, permission)?;
        Ok(format!(
            "{base_url}?expires={expires}&permission={}&signature={signature}",
            urlencoding::encode(permission)
        ))
    }

    fn signature(
        &self,
        object_key: &str,
        expires_at_unix: i64,
        permission: &str,
    ) -> LinkResult<String> {
        let mut mac = HmacSha256::new_from_slice(self.signing_secret.as_bytes())
            .map_err(|_| LinkError::InvalidSigningKey)?;
        let payload = format!("{object_key}\n{expires_at_unix}\n{permission}");
        mac.update(payload.as_bytes());
        Ok(URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes()))
    }
}

fn join_url(base_url: &str, object_key: &str) -> String {
    format!(
        "{}/{}",
        base_url.trim_end_matches('/'),
        object_key
            .split('/')
            .map(urlencoding::encode)
            .collect::<Vec<_>>()
            .join("/")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use mediaforge_config::{AppConfig, RuntimeMode, S3Config, StorageBackend, StorageConfig};
    use mediaforge_storage::FilesystemObjectStorage;
    use std::path::PathBuf;
    use std::sync::Arc;

    fn test_config() -> AppConfig {
        AppConfig {
            runtime_mode: RuntimeMode::Combined,
            http_bind: "127.0.0.1:8080".parse().unwrap(),
            storage: StorageConfig {
                backend: StorageBackend::Filesystem,
                s3: S3Config {
                    endpoint: None,
                    region: "auto".to_string(),
                    bucket: "mediaforge".to_string(),
                    access_key_id: None,
                    secret_access_key: None,
                    force_path_style: false,
                },
                filesystem_root: PathBuf::from("/tmp/mediaforge-test"),
            },
            cdn_base_url: Some("https://cdn.example.com/media".to_string()),
            link_signing_secret: "secret".to_string(),
            jwt_secret: None,
            temp_directory: PathBuf::from("/tmp/mediaforge"),
            ffmpeg_path: "ffmpeg".to_string(),
            ffprobe_path: "ffprobe".to_string(),
            ffmpeg_threads: Some(1),
            worker_concurrency: 1,
            worker_poll_interval: Duration::from_secs(1),
            sqlite_cache_path: None,
            log_level: "info".to_string(),
        }
    }

    #[tokio::test]
    async fn temporary_link_can_be_validated() {
        let config = test_config();
        let storage = Arc::new(FilesystemObjectStorage::new(PathBuf::from(
            "/tmp/mediaforge-test",
        )));
        let generator = LinkGenerator::new(&config, storage);

        let link = generator
            .generate_link(
                "media/sha256/aa/bb/object.jpg",
                LinkPolicy::Temporary {
                    expires_in_seconds: 60,
                },
            )
            .await
            .unwrap();

        let expires_at = link.expires_at.unwrap();
        let signature = link.url.split("signature=").nth(1).unwrap();
        assert!(generator
            .validate_signature(
                "media/sha256/aa/bb/object.jpg",
                expires_at.timestamp(),
                "read",
                signature,
                Utc::now(),
            )
            .unwrap());
    }
}
