use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt::{Display, Formatter};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ResourceId(pub String);

impl Display for ResourceId {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl AsRef<str> for ResourceId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ResultId(pub String);

impl Display for ResultId {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl AsRef<str> for ResultId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TaskId(pub String);

impl Display for TaskId {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl AsRef<str> for TaskId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaKind {
    Image,
    Video,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResourceManifest {
    pub resource_id: ResourceId,
    pub media_kind: MediaKind,
    pub original: MediaObject,
    pub derived: Vec<DerivedMediaObject>,
    pub task_history: Vec<TaskId>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MediaObject {
    pub object_key: String,
    pub file_name: Option<String>,
    pub mime_type: String,
    pub size_bytes: u64,
    pub checksum_sha256: String,
    pub metadata: MediaMetadata,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct MediaMetadata {
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub duration_seconds: Option<f64>,
    pub codec: Option<String>,
    pub bitrate_bps: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DerivedMediaObject {
    pub result_id: ResultId,
    pub derived_kind: DerivedMediaKind,
    pub object: MediaObject,
    pub parameters: serde_json::Value,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DerivedMediaKind {
    ImageVariant,
    VideoVariant,
    HlsPlaylist,
    HlsSegment,
    Screenshot,
    Cover,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImageProcessingRequest {
    pub output_format: ImageOutputFormat,
    pub quality: Option<u8>,
    #[serde(default)]
    pub operations: Vec<ImageOperation>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageOutputFormat {
    Jpeg,
    Png,
    Webp,
    Avif,
}

impl ImageOutputFormat {
    pub fn extension(self) -> &'static str {
        match self {
            ImageOutputFormat::Jpeg => "jpg",
            ImageOutputFormat::Png => "png",
            ImageOutputFormat::Webp => "webp",
            ImageOutputFormat::Avif => "avif",
        }
    }

    pub fn mime_type(self) -> &'static str {
        match self {
            ImageOutputFormat::Jpeg => "image/jpeg",
            ImageOutputFormat::Png => "image/png",
            ImageOutputFormat::Webp => "image/webp",
            ImageOutputFormat::Avif => "image/avif",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ImageOperation {
    Resize {
        width: Option<u32>,
        height: Option<u32>,
        fit: ResizeFit,
    },
    Crop {
        x: u32,
        y: u32,
        width: u32,
        height: u32,
    },
    Rotate {
        degrees: RotationDegrees,
    },
    ImageWatermark {
        object_key: String,
        position: WatermarkPosition,
        opacity: f32,
    },
    TextWatermark {
        text: String,
        font_size: u32,
        color_hex: String,
        position: WatermarkPosition,
        opacity: f32,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResizeFit {
    Contain,
    Cover,
    Fill,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RotationDegrees {
    Deg90,
    Deg180,
    Deg270,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WatermarkPosition {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
    Center,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VideoProcessingRequest {
    pub profile: VideoProfile,
    #[serde(default)]
    pub screenshots: Vec<ScreenshotRequest>,
    pub generate_cover: bool,
    pub generate_hls: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VideoProfile {
    pub codec: VideoCodec,
    pub container: VideoContainer,
    pub resolution: Option<VideoResolution>,
    pub crf: Option<u8>,
    pub bitrate_kbps: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VideoCodec {
    H264,
    H265,
    Av1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VideoContainer {
    Mp4,
    Mkv,
}

impl VideoContainer {
    pub fn extension(self) -> &'static str {
        match self {
            VideoContainer::Mp4 => "mp4",
            VideoContainer::Mkv => "mkv",
        }
    }

    pub fn mime_type(self) -> &'static str {
        match self {
            VideoContainer::Mp4 => "video/mp4",
            VideoContainer::Mkv => "video/x-matroska",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VideoResolution {
    P360,
    P480,
    P720,
    P1080,
    P1440,
    P2160,
}

impl VideoResolution {
    pub fn height(self) -> u32 {
        match self {
            VideoResolution::P360 => 360,
            VideoResolution::P480 => 480,
            VideoResolution::P720 => 720,
            VideoResolution::P1080 => 1080,
            VideoResolution::P1440 => 1440,
            VideoResolution::P2160 => 2160,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScreenshotRequest {
    pub timestamp_seconds: f64,
    pub output_format: ImageOutputFormat,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum TaskOperation {
    ImageProcessing { request: ImageProcessingRequest },
    VideoProcessing { request: VideoProcessingRequest },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskDescriptor {
    pub task_id: TaskId,
    pub resource_id: ResourceId,
    pub operation: TaskOperation,
    pub parameters: serde_json::Value,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskStatus {
    pub task_id: TaskId,
    pub resource_id: ResourceId,
    pub state: TaskState,
    pub message: Option<String>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    Pending,
    Leased,
    Running,
    Completed,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GeneratedLink {
    pub url: String,
    pub link_kind: LinkKind,
    pub expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkKind {
    Public,
    Temporary,
    Signed,
    StoragePresigned,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LinkPolicy {
    Public,
    Temporary {
        expires_in_seconds: u64,
    },
    Signed {
        expires_in_seconds: u64,
        permission: String,
    },
    StoragePresigned {
        expires_in_seconds: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResourceQueryResponse {
    pub manifest: ResourceManifest,
    pub links: BTreeMap<String, GeneratedLink>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UploadResponse {
    pub resource_id: ResourceId,
    pub task_id: Option<TaskId>,
    pub manifest: ResourceManifest,
}
