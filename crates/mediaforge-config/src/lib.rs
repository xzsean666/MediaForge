use std::env;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("invalid runtime mode: {0}")]
    InvalidRuntimeMode(String),
    #[error("invalid storage backend: {0}")]
    InvalidStorageBackend(String),
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppConfig {
    pub runtime_mode: RuntimeMode,
    pub http_bind: SocketAddr,
    pub storage: StorageConfig,
    pub cdn_base_url: Option<String>,
    pub link_signing_secret: String,
    pub jwt_secret: Option<String>,
    pub temp_directory: PathBuf,
    pub ffmpeg_path: String,
    pub ffprobe_path: String,
    pub ffmpeg_threads: Option<usize>,
    pub worker_concurrency: usize,
    pub worker_poll_interval: Duration,
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
        )?;
        let poll_interval_seconds = parse_u64(
            "MEDIAFORGE_WORKER_POLL_INTERVAL_SECONDS",
            env_string("MEDIAFORGE_WORKER_POLL_INTERVAL_SECONDS", "5"),
        )?;

        let storage = StorageConfig {
            backend: storage_backend,
            s3: S3Config {
                endpoint: optional_env("MEDIAFORGE_S3_ENDPOINT"),
                region: env_string("MEDIAFORGE_S3_REGION", "auto"),
                bucket: required_when_s3(storage_backend, "MEDIAFORGE_S3_BUCKET", "mediaforge")?,
                access_key_id: optional_env("MEDIAFORGE_S3_ACCESS_KEY_ID"),
                secret_access_key: optional_env("MEDIAFORGE_S3_SECRET_ACCESS_KEY"),
                force_path_style: parse_bool(env_string("MEDIAFORGE_S3_FORCE_PATH_STYLE", "false")),
            },
            filesystem_root: PathBuf::from(env_string(
                "MEDIAFORGE_FILESYSTEM_STORAGE_ROOT",
                "/tmp/mediaforge-object-store",
            )),
        };

        Ok(Self {
            runtime_mode,
            http_bind,
            storage,
            cdn_base_url: optional_env("MEDIAFORGE_CDN_BASE_URL"),
            link_signing_secret: env_string(
                "MEDIAFORGE_LINK_SIGNING_SECRET",
                "development-only-secret",
            ),
            jwt_secret: optional_env("MEDIAFORGE_JWT_SECRET"),
            temp_directory: PathBuf::from(env_string("MEDIAFORGE_TEMP_DIR", "/tmp/mediaforge")),
            ffmpeg_path: env_string("MEDIAFORGE_FFMPEG_PATH", "ffmpeg"),
            ffprobe_path: env_string("MEDIAFORGE_FFPROBE_PATH", "ffprobe"),
            ffmpeg_threads: optional_usize("MEDIAFORGE_FFMPEG_THREADS")?,
            worker_concurrency,
            worker_poll_interval: Duration::from_secs(poll_interval_seconds),
            sqlite_cache_path: optional_env("MEDIAFORGE_SQLITE_CACHE_PATH").map(PathBuf::from),
            log_level: env_string("MEDIAFORGE_LOG_LEVEL", "info"),
        })
    }
}

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

fn required_when_s3(
    backend: StorageBackend,
    name: &'static str,
    default_value: &'static str,
) -> ConfigResult<String> {
    match env::var(name) {
        Ok(value) if !value.trim().is_empty() => Ok(value),
        _ if backend == StorageBackend::S3 => Ok(default_value.to_string()),
        _ => Ok(default_value.to_string()),
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

fn parse_bool(value: String) -> bool {
    matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES")
}
