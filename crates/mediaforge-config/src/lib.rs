use std::env;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

/// Default value of `MEDIAFORGE_LINK_SIGNING_SECRET`. Used in development only;
/// running in production with this value is insecure and triggers a startup warning.
pub const DEVELOPMENT_LINK_SIGNING_SECRET: &str = "development-only-change-me";
const LEGACY_DEVELOPMENT_LINK_SIGNING_SECRET: &str = "development-only-secret";

/// Default maximum upload size when the upload limit is enabled (5 GiB).
pub const DEFAULT_MAX_UPLOAD_BYTES: u64 = 5 * 1024 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("invalid runtime mode: {0}")]
    InvalidRuntimeMode(String),
    #[error("invalid storage backend: {0}")]
    InvalidStorageBackend(String),
    #[error("invalid FFmpeg video acceleration: {0}")]
    InvalidFfmpegVideoAcceleration(String),
    #[error("invalid socket address in {name}: {value}")]
    InvalidSocketAddress { name: &'static str, value: String },
    #[error("invalid unsigned integer in {name}: {value}")]
    InvalidUnsignedInteger { name: &'static str, value: String },
    #[error("missing required environment variable: {0}")]
    MissingRequired(&'static str),
}

pub type ConfigResult<T> = Result<T, ConfigError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeMode {
    Api,
    Worker,
    Combined,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageBackend {
    S3,
    Filesystem,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FfmpegVideoAcceleration {
    None,
    Nvidia,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppConfig {
    pub runtime_mode: RuntimeMode,
    pub http_bind: SocketAddr,
    pub storage: StorageConfig,
    pub cdn_base_url: Option<String>,
    pub link_signing_secret: String,
    pub auth: AuthConfig,
    pub upload_limit: UploadLimitConfig,
    pub temp_directory: PathBuf,
    pub ffmpeg_path: String,
    pub ffprobe_path: String,
    pub ffmpeg_threads: Option<usize>,
    pub ffmpeg_video_acceleration: FfmpegVideoAcceleration,
    pub worker_concurrency: usize,
    pub worker_poll_interval: Duration,
    pub task_lease_timeout: Duration,
    pub task_max_attempts: u32,
    pub presigned_upload_expiry: Duration,
    pub source_reaper_enabled: bool,
    pub sqlite_cache_path: Option<PathBuf>,
    pub log_level: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageConfig {
    pub backend: StorageBackend,
    pub s3: S3Config,
    pub filesystem_root: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct S3Config {
    pub endpoint: Option<String>,
    pub region: String,
    pub bucket: String,
    pub access_key_id: Option<String>,
    pub secret_access_key: Option<String>,
    pub force_path_style: bool,
}

/// API authentication. Disabled by default; when enabled, every endpoint except
/// `/health` and tus `OPTIONS` requires a valid HS256 JWT signed with `jwt_secret`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthConfig {
    pub enabled: bool,
    pub jwt_secret: Option<String>,
    pub leeway_seconds: u64,
}

/// Upload size limiting. Enabled by default with a generous ceiling so that a
/// single request cannot exhaust local disk; can be turned off entirely.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UploadLimitConfig {
    pub enabled: bool,
    pub max_bytes: u64,
}

impl UploadLimitConfig {
    /// The effective byte ceiling, or `None` when limiting is disabled.
    pub fn effective_max_bytes(&self) -> Option<u64> {
        if self.enabled {
            Some(self.max_bytes)
        } else {
            None
        }
    }
}

impl AppConfig {
    pub fn from_env() -> ConfigResult<Self> {
        let runtime_mode = parse_runtime_mode(env_string("MEDIAFORGE_MODE", "combined"))?;
        let http_bind = parse_socket_addr(
            "MEDIAFORGE_HTTP_BIND",
            env_string("MEDIAFORGE_HTTP_BIND", "0.0.0.0:8080"),
        )?;
        let storage_backend =
            parse_storage_backend(env_string("MEDIAFORGE_STORAGE_BACKEND", "s3"))?;
        let worker_concurrency = parse_usize(
            "MEDIAFORGE_WORKER_CONCURRENCY",
            env_string("MEDIAFORGE_WORKER_CONCURRENCY", "2"),
        )?
        .max(1);
        let poll_interval_seconds = parse_u64(
            "MEDIAFORGE_WORKER_POLL_INTERVAL_SECONDS",
            env_string("MEDIAFORGE_WORKER_POLL_INTERVAL_SECONDS", "5"),
        )?;
        let lease_timeout_seconds = parse_u64(
            "MEDIAFORGE_TASK_LEASE_TIMEOUT_SECONDS",
            env_string("MEDIAFORGE_TASK_LEASE_TIMEOUT_SECONDS", "600"),
        )?;
        let task_max_attempts = parse_u32(
            "MEDIAFORGE_TASK_MAX_ATTEMPTS",
            env_string("MEDIAFORGE_TASK_MAX_ATTEMPTS", "3"),
        )?
        .max(1);
        let presigned_upload_expiry_seconds = parse_u64(
            "MEDIAFORGE_PRESIGNED_UPLOAD_EXPIRY_SECONDS",
            env_string("MEDIAFORGE_PRESIGNED_UPLOAD_EXPIRY_SECONDS", "3600"),
        )?;
        let source_reaper_enabled =
            parse_bool(env_string("MEDIAFORGE_SOURCE_REAPER_ENABLED", "true"));

        let storage = StorageConfig {
            backend: storage_backend,
            s3: S3Config {
                endpoint: optional_env("MEDIAFORGE_S3_ENDPOINT"),
                region: env_string("MEDIAFORGE_S3_REGION", "auto"),
                bucket: required_when_s3(storage_backend, "MEDIAFORGE_S3_BUCKET")?,
                access_key_id: optional_env("MEDIAFORGE_S3_ACCESS_KEY_ID"),
                secret_access_key: optional_env("MEDIAFORGE_S3_SECRET_ACCESS_KEY"),
                force_path_style: parse_bool(env_string("MEDIAFORGE_S3_FORCE_PATH_STYLE", "false")),
            },
            filesystem_root: PathBuf::from(env_string(
                "MEDIAFORGE_FILESYSTEM_STORAGE_ROOT",
                "/tmp/mediaforge-object-store",
            )),
        };

        let auth = AuthConfig {
            enabled: parse_bool(env_string("MEDIAFORGE_AUTH_ENABLED", "false")),
            jwt_secret: optional_env("MEDIAFORGE_JWT_SECRET"),
            leeway_seconds: parse_u64(
                "MEDIAFORGE_AUTH_LEEWAY_SECONDS",
                env_string("MEDIAFORGE_AUTH_LEEWAY_SECONDS", "30"),
            )?,
        };
        if auth.enabled && auth.jwt_secret.is_none() {
            return Err(ConfigError::MissingRequired("MEDIAFORGE_JWT_SECRET"));
        }

        let upload_limit = UploadLimitConfig {
            enabled: parse_bool(env_string("MEDIAFORGE_UPLOAD_LIMIT_ENABLED", "true")),
            max_bytes: parse_u64(
                "MEDIAFORGE_MAX_UPLOAD_BYTES",
                env_string("MEDIAFORGE_MAX_UPLOAD_BYTES", DEFAULT_MAX_UPLOAD_BYTES_STR),
            )?,
        };

        Ok(Self {
            runtime_mode,
            http_bind,
            storage,
            cdn_base_url: optional_env("MEDIAFORGE_CDN_BASE_URL"),
            link_signing_secret: env_string(
                "MEDIAFORGE_LINK_SIGNING_SECRET",
                DEVELOPMENT_LINK_SIGNING_SECRET,
            ),
            auth,
            upload_limit,
            temp_directory: PathBuf::from(env_string("MEDIAFORGE_TEMP_DIR", "/tmp/mediaforge")),
            ffmpeg_path: env_string("MEDIAFORGE_FFMPEG_PATH", "ffmpeg"),
            ffprobe_path: env_string("MEDIAFORGE_FFPROBE_PATH", "ffprobe"),
            ffmpeg_threads: optional_usize("MEDIAFORGE_FFMPEG_THREADS")?,
            ffmpeg_video_acceleration: parse_ffmpeg_video_acceleration(env_string(
                "MEDIAFORGE_FFMPEG_VIDEO_ACCELERATION",
                "none",
            ))?,
            worker_concurrency,
            worker_poll_interval: Duration::from_secs(poll_interval_seconds),
            task_lease_timeout: Duration::from_secs(lease_timeout_seconds),
            task_max_attempts,
            presigned_upload_expiry: Duration::from_secs(presigned_upload_expiry_seconds),
            source_reaper_enabled,
            sqlite_cache_path: optional_env("MEDIAFORGE_SQLITE_CACHE_PATH").map(PathBuf::from),
            log_level: env_string("MEDIAFORGE_LOG_LEVEL", "info"),
        })
    }

    /// Returns true when the link signing secret is still the insecure development
    /// default. Callers should warn loudly before serving production traffic.
    pub fn uses_default_signing_secret(&self) -> bool {
        self.link_signing_secret == DEVELOPMENT_LINK_SIGNING_SECRET
            || self.link_signing_secret == LEGACY_DEVELOPMENT_LINK_SIGNING_SECRET
    }
}

const DEFAULT_MAX_UPLOAD_BYTES_STR: &str = "5368709120";

fn env_string(name: &'static str, default_value: &'static str) -> String {
    env::var(name).unwrap_or_else(|_| default_value.to_string())
}

fn optional_env(name: &'static str) -> Option<String> {
    env::var(name).ok().filter(|value| !value.trim().is_empty())
}

fn optional_usize(name: &'static str) -> ConfigResult<Option<usize>> {
    optional_env(name)
        .map(|value| parse_usize(name, value))
        .transpose()
}

/// Returns the bucket name, requiring a non-empty value when the S3 backend is
/// selected. Previously this silently fell back to a placeholder bucket, which
/// caused confusing runtime failures against object storage.
fn required_when_s3(backend: StorageBackend, name: &'static str) -> ConfigResult<String> {
    match env::var(name) {
        Ok(value) if !value.trim().is_empty() => Ok(value),
        _ if backend == StorageBackend::S3 => Err(ConfigError::MissingRequired(name)),
        // For the filesystem backend the bucket is unused; keep a stable placeholder.
        _ => Ok("mediaforge".to_string()),
    }
}

fn parse_runtime_mode(value: String) -> ConfigResult<RuntimeMode> {
    match value.as_str() {
        "api" => Ok(RuntimeMode::Api),
        "worker" => Ok(RuntimeMode::Worker),
        "combined" => Ok(RuntimeMode::Combined),
        _ => Err(ConfigError::InvalidRuntimeMode(value)),
    }
}

fn parse_storage_backend(value: String) -> ConfigResult<StorageBackend> {
    match value.as_str() {
        "s3" => Ok(StorageBackend::S3),
        "filesystem" => Ok(StorageBackend::Filesystem),
        _ => Err(ConfigError::InvalidStorageBackend(value)),
    }
}

fn parse_ffmpeg_video_acceleration(value: String) -> ConfigResult<FfmpegVideoAcceleration> {
    match value.as_str() {
        "none" | "cpu" | "software" => Ok(FfmpegVideoAcceleration::None),
        "nvidia" | "nvenc" => Ok(FfmpegVideoAcceleration::Nvidia),
        _ => Err(ConfigError::InvalidFfmpegVideoAcceleration(value)),
    }
}

fn parse_socket_addr(name: &'static str, value: String) -> ConfigResult<SocketAddr> {
    value
        .parse()
        .map_err(|_| ConfigError::InvalidSocketAddress { name, value })
}

fn parse_usize(name: &'static str, value: String) -> ConfigResult<usize> {
    value
        .parse()
        .map_err(|_| ConfigError::InvalidUnsignedInteger { name, value })
}

fn parse_u64(name: &'static str, value: String) -> ConfigResult<u64> {
    value
        .parse()
        .map_err(|_| ConfigError::InvalidUnsignedInteger { name, value })
}

fn parse_u32(name: &'static str, value: String) -> ConfigResult<u32> {
    value
        .parse()
        .map_err(|_| ConfigError::InvalidUnsignedInteger { name, value })
}

fn parse_bool(value: String) -> bool {
    matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upload_limit_effective_bytes_respects_toggle() {
        let enabled = UploadLimitConfig {
            enabled: true,
            max_bytes: 1024,
        };
        assert_eq!(enabled.effective_max_bytes(), Some(1024));

        let disabled = UploadLimitConfig {
            enabled: false,
            max_bytes: 1024,
        };
        assert_eq!(disabled.effective_max_bytes(), None);
    }

    #[test]
    fn default_max_upload_bytes_string_matches_constant() {
        assert_eq!(
            DEFAULT_MAX_UPLOAD_BYTES_STR.parse::<u64>().unwrap(),
            DEFAULT_MAX_UPLOAD_BYTES
        );
    }

    #[test]
    fn required_when_s3_errors_without_bucket() {
        // Ensure the variable is unset for this test.
        std::env::remove_var("MEDIAFORGE_S3_BUCKET");
        let result = required_when_s3(StorageBackend::S3, "MEDIAFORGE_S3_BUCKET");
        assert!(matches!(result, Err(ConfigError::MissingRequired(_))));

        let filesystem = required_when_s3(StorageBackend::Filesystem, "MEDIAFORGE_S3_BUCKET");
        assert_eq!(filesystem.unwrap(), "mediaforge");
    }

    #[test]
    fn ffmpeg_video_acceleration_aliases_are_parsed() {
        assert_eq!(
            parse_ffmpeg_video_acceleration("none".to_string()).unwrap(),
            FfmpegVideoAcceleration::None
        );
        assert_eq!(
            parse_ffmpeg_video_acceleration("cpu".to_string()).unwrap(),
            FfmpegVideoAcceleration::None
        );
        assert_eq!(
            parse_ffmpeg_video_acceleration("nvidia".to_string()).unwrap(),
            FfmpegVideoAcceleration::Nvidia
        );
        assert_eq!(
            parse_ffmpeg_video_acceleration("nvenc".to_string()).unwrap(),
            FfmpegVideoAcceleration::Nvidia
        );
    }

    #[test]
    fn invalid_ffmpeg_video_acceleration_is_rejected() {
        assert!(matches!(
            parse_ffmpeg_video_acceleration("cuda".to_string()),
            Err(ConfigError::InvalidFfmpegVideoAcceleration(_))
        ));
    }

    #[test]
    fn development_signing_secrets_are_detected() {
        let mut config = AppConfig::from_env().unwrap_or_else(|_| AppConfig {
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
                filesystem_root: PathBuf::from("/tmp/mediaforge-object-store"),
            },
            cdn_base_url: None,
            link_signing_secret: DEVELOPMENT_LINK_SIGNING_SECRET.to_string(),
            auth: AuthConfig {
                enabled: false,
                jwt_secret: None,
                leeway_seconds: 30,
            },
            upload_limit: UploadLimitConfig {
                enabled: true,
                max_bytes: DEFAULT_MAX_UPLOAD_BYTES,
            },
            temp_directory: PathBuf::from("/tmp/mediaforge"),
            ffmpeg_path: "ffmpeg".to_string(),
            ffprobe_path: "ffprobe".to_string(),
            ffmpeg_threads: None,
            ffmpeg_video_acceleration: FfmpegVideoAcceleration::None,
            worker_concurrency: 1,
            worker_poll_interval: Duration::from_secs(1),
            task_lease_timeout: Duration::from_secs(600),
            task_max_attempts: 3,
            presigned_upload_expiry: Duration::from_secs(3600),
            source_reaper_enabled: true,
            sqlite_cache_path: None,
            log_level: "info".to_string(),
        });

        config.link_signing_secret = DEVELOPMENT_LINK_SIGNING_SECRET.to_string();
        assert!(config.uses_default_signing_secret());
        config.link_signing_secret = LEGACY_DEVELOPMENT_LINK_SIGNING_SECRET.to_string();
        assert!(config.uses_default_signing_secret());
        config.link_signing_secret = "not-a-default-secret".to_string();
        assert!(!config.uses_default_signing_secret());
    }
}
