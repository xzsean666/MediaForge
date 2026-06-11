use mediaforge_types::{
    ImageOutputFormat, MediaMetadata, VideoCodec, VideoContainer, VideoProcessingRequest,
    VideoProfile,
};
use std::path::{Path, PathBuf};
use tokio::process::Command;

#[derive(Debug, thiserror::Error)]
pub enum VideoProcessingError {
    #[error("invalid video processing request: {0}")]
    InvalidRequest(String),
    #[error("ffmpeg process failed: {0}")]
    FfmpegFailed(String),
    #[error("ffprobe process failed: {0}")]
    FfprobeFailed(String),
    #[error("video IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("ffprobe JSON parse error: {0}")]
    Json(#[from] serde_json::Error),
}

pub type VideoProcessingResult<T> = Result<T, VideoProcessingError>;

const MAX_SCREENSHOT_REQUESTS: usize = 20;
const MAX_VIDEO_BITRATE_KBPS: u32 = 200_000;
const MAX_NVIDIA_CQ: u8 = 51;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoAcceleration {
    None,
    Nvidia,
}

#[derive(Debug, Clone)]
pub struct FfmpegProcessor {
    ffmpeg_path: String,
    ffprobe_path: String,
    thread_count: Option<usize>,
    video_acceleration: VideoAcceleration,
}

impl FfmpegProcessor {
    pub fn new(
        ffmpeg_path: String,
        ffprobe_path: String,
        thread_count: Option<usize>,
        video_acceleration: VideoAcceleration,
    ) -> Self {
        Self {
            ffmpeg_path,
            ffprobe_path,
            thread_count,
            video_acceleration,
        }
    }

    pub async fn transcode(
        &self,
        input_path: impl AsRef<Path>,
        output_path: impl AsRef<Path>,
        profile: &VideoProfile,
    ) -> VideoProcessingResult<()> {
        let arguments = build_transcode_arguments(
            input_path.as_ref(),
            output_path.as_ref(),
            profile,
            self.thread_count,
            self.video_acceleration,
        )?;
        run_process(&self.ffmpeg_path, &arguments).await
    }

    pub async fn screenshot(
        &self,
        input_path: impl AsRef<Path>,
        output_path: impl AsRef<Path>,
        timestamp_seconds: f64,
        output_format: ImageOutputFormat,
    ) -> VideoProcessingResult<()> {
        validate_screenshot_timestamp(timestamp_seconds)?;

        let mut arguments = base_ffmpeg_arguments(self.thread_count);
        arguments.extend([
            "-y".to_string(),
            "-ss".to_string(),
            timestamp_seconds.to_string(),
            "-i".to_string(),
            input_path.as_ref().display().to_string(),
            "-frames:v".to_string(),
            "1".to_string(),
        ]);

        if output_format == ImageOutputFormat::Webp {
            arguments.push("-c:v".to_string());
            arguments.push("libwebp".to_string());
        }

        arguments.push(output_path.as_ref().display().to_string());
        run_process(&self.ffmpeg_path, &arguments).await
    }

    pub async fn generate_hls(
        &self,
        input_path: impl AsRef<Path>,
        output_directory: impl AsRef<Path>,
    ) -> VideoProcessingResult<PathBuf> {
        tokio::fs::create_dir_all(output_directory.as_ref()).await?;
        let playlist = output_directory.as_ref().join("master.m3u8");
        let segment_pattern = output_directory.as_ref().join("segment-%06d.ts");
        let video_codec = hls_codec_name(self.video_acceleration);
        let mut arguments = base_ffmpeg_arguments(self.thread_count);
        arguments.extend([
            "-y".to_string(),
            "-i".to_string(),
            input_path.as_ref().display().to_string(),
            "-codec:v".to_string(),
            video_codec.to_string(),
            "-codec:a".to_string(),
            "aac".to_string(),
            "-f".to_string(),
            "hls".to_string(),
            "-hls_time".to_string(),
            "6".to_string(),
            "-hls_playlist_type".to_string(),
            "vod".to_string(),
            "-hls_segment_filename".to_string(),
            segment_pattern.display().to_string(),
            playlist.display().to_string(),
        ]);
        run_process(&self.ffmpeg_path, &arguments).await?;
        Ok(playlist)
    }

    pub async fn process_request(
        &self,
        input_path: impl AsRef<Path>,
        working_directory: impl AsRef<Path>,
        request: &VideoProcessingRequest,
    ) -> VideoProcessingResult<Vec<PathBuf>> {
        validate_request(request)?;
        tokio::fs::create_dir_all(working_directory.as_ref()).await?;
        let mut outputs = Vec::new();

        let transcoded = working_directory
            .as_ref()
            .join(format!("output.{}", request.profile.container.extension()));
        self.transcode(input_path.as_ref(), &transcoded, &request.profile)
            .await?;
        outputs.push(transcoded);

        if request.generate_cover {
            let cover = working_directory.as_ref().join("cover.jpg");
            self.screenshot(input_path.as_ref(), &cover, 1.0, ImageOutputFormat::Jpeg)
                .await?;
            outputs.push(cover);
        }

        for (index, screenshot) in request.screenshots.iter().enumerate() {
            let output = working_directory.as_ref().join(format!(
                "screenshot-{index:06}.{}",
                screenshot.output_format.extension()
            ));
            self.screenshot(
                input_path.as_ref(),
                &output,
                screenshot.timestamp_seconds,
                screenshot.output_format,
            )
            .await?;
            outputs.push(output);
        }

        if request.generate_hls {
            let hls_directory = working_directory.as_ref().join("hls");
            outputs.push(
                self.generate_hls(input_path.as_ref(), &hls_directory)
                    .await?,
            );
        }

        Ok(outputs)
    }

    pub async fn probe_metadata(
        &self,
        input_path: impl AsRef<Path>,
    ) -> VideoProcessingResult<MediaMetadata> {
        let output = Command::new(&self.ffprobe_path)
            .arg("-v")
            .arg("error")
            .arg("-show_entries")
            .arg("format=duration,bit_rate:stream=codec_name,width,height")
            .arg("-of")
            .arg("json")
            .arg(input_path.as_ref())
            .output()
            .await?;

        if !output.status.success() {
            return Err(VideoProcessingError::FfprobeFailed(
                String::from_utf8_lossy(&output.stderr).to_string(),
            ));
        }

        let value: serde_json::Value = serde_json::from_slice(&output.stdout)?;
        Ok(MediaMetadata {
            width: value
                .pointer("/streams/0/width")
                .and_then(|value| value.as_u64())
                .map(|value| value as u32),
            height: value
                .pointer("/streams/0/height")
                .and_then(|value| value.as_u64())
                .map(|value| value as u32),
            duration_seconds: value
                .pointer("/format/duration")
                .and_then(|value| value.as_str())
                .and_then(|value| value.parse().ok()),
            codec: value
                .pointer("/streams/0/codec_name")
                .and_then(|value| value.as_str())
                .map(ToString::to_string),
            bitrate_bps: value
                .pointer("/format/bit_rate")
                .and_then(|value| value.as_str())
                .and_then(|value| value.parse().ok()),
        })
    }
}

pub fn validate_request(request: &VideoProcessingRequest) -> VideoProcessingResult<()> {
    validate_profile(&request.profile, VideoAcceleration::None)?;

    if request.screenshots.len() > MAX_SCREENSHOT_REQUESTS {
        return Err(VideoProcessingError::InvalidRequest(format!(
            "screenshots must contain at most {MAX_SCREENSHOT_REQUESTS} entries"
        )));
    }

    for screenshot in &request.screenshots {
        validate_screenshot_timestamp(screenshot.timestamp_seconds)?;
    }

    Ok(())
}

pub fn build_transcode_arguments(
    input_path: &Path,
    output_path: &Path,
    profile: &VideoProfile,
    thread_count: Option<usize>,
    video_acceleration: VideoAcceleration,
) -> VideoProcessingResult<Vec<String>> {
    validate_profile(profile, video_acceleration)?;

    let mut arguments = base_ffmpeg_arguments(thread_count);
    arguments.extend([
        "-y".to_string(),
        "-i".to_string(),
        input_path.display().to_string(),
        "-c:v".to_string(),
        codec_name(profile.codec, video_acceleration)?.to_string(),
    ]);

    if let Some(resolution) = profile.resolution {
        arguments.push("-vf".to_string());
        arguments.push(format!("scale=-2:{}", resolution.height()));
    }

    if let Some(crf) = profile.crf {
        arguments.push(
            match video_acceleration {
                VideoAcceleration::None => "-crf",
                VideoAcceleration::Nvidia => "-cq",
            }
            .to_string(),
        );
        arguments.push(crf.to_string());
    }

    if let Some(bitrate_kbps) = profile.bitrate_kbps {
        arguments.push("-b:v".to_string());
        arguments.push(format!("{bitrate_kbps}k"));
    }

    if profile.container == VideoContainer::Mp4 {
        arguments.push("-movflags".to_string());
        arguments.push("+faststart".to_string());
    }

    arguments.push("-c:a".to_string());
    arguments.push("aac".to_string());
    arguments.push(output_path.display().to_string());
    Ok(arguments)
}

fn validate_profile(
    profile: &VideoProfile,
    video_acceleration: VideoAcceleration,
) -> VideoProcessingResult<()> {
    if profile.container == VideoContainer::Mp4 && profile.codec == VideoCodec::Av1 {
        return Err(VideoProcessingError::InvalidRequest(
            "AV1 MP4 support depends on the FFmpeg build; use MKV for portable AV1 output"
                .to_string(),
        ));
    }

    if video_acceleration == VideoAcceleration::Nvidia && profile.codec == VideoCodec::Av1 {
        return Err(VideoProcessingError::InvalidRequest(
            "NVIDIA acceleration currently supports H.264 and H.265 output; use CPU encoding for AV1"
                .to_string(),
        ));
    }

    if let Some(crf) = profile.crf {
        if crf > 63 {
            return Err(VideoProcessingError::InvalidRequest(
                "crf must be between 0 and 63".to_string(),
            ));
        }

        if video_acceleration == VideoAcceleration::Nvidia && crf > MAX_NVIDIA_CQ {
            return Err(VideoProcessingError::InvalidRequest(format!(
                "crf must be between 0 and {MAX_NVIDIA_CQ} when NVIDIA acceleration is enabled"
            )));
        }
    }

    if let Some(bitrate_kbps) = profile.bitrate_kbps {
        if bitrate_kbps == 0 || bitrate_kbps > MAX_VIDEO_BITRATE_KBPS {
            return Err(VideoProcessingError::InvalidRequest(format!(
                "bitrate_kbps must be between 1 and {MAX_VIDEO_BITRATE_KBPS}"
            )));
        }
    }

    Ok(())
}

fn validate_screenshot_timestamp(timestamp_seconds: f64) -> VideoProcessingResult<()> {
    if !timestamp_seconds.is_finite() || timestamp_seconds < 0.0 {
        return Err(VideoProcessingError::InvalidRequest(
            "screenshot timestamp must be finite and non-negative".to_string(),
        ));
    }
    Ok(())
}

fn codec_name(
    codec: VideoCodec,
    video_acceleration: VideoAcceleration,
) -> VideoProcessingResult<&'static str> {
    match (video_acceleration, codec) {
        (VideoAcceleration::None, VideoCodec::H264) => Ok("libx264"),
        (VideoAcceleration::None, VideoCodec::H265) => Ok("libx265"),
        (VideoAcceleration::None, VideoCodec::Av1) => Ok("libsvtav1"),
        (VideoAcceleration::Nvidia, VideoCodec::H264) => Ok("h264_nvenc"),
        (VideoAcceleration::Nvidia, VideoCodec::H265) => Ok("hevc_nvenc"),
        (VideoAcceleration::Nvidia, VideoCodec::Av1) => Err(VideoProcessingError::InvalidRequest(
            "NVIDIA acceleration currently supports H.264 and H.265 output; use CPU encoding for AV1"
                .to_string(),
        )),
    }
}

fn hls_codec_name(video_acceleration: VideoAcceleration) -> &'static str {
    match video_acceleration {
        VideoAcceleration::None => "libx264",
        VideoAcceleration::Nvidia => "h264_nvenc",
    }
}

fn base_ffmpeg_arguments(thread_count: Option<usize>) -> Vec<String> {
    let mut arguments = Vec::new();
    if let Some(thread_count) = thread_count {
        arguments.push("-threads".to_string());
        arguments.push(thread_count.max(1).to_string());
    }
    arguments
}

async fn run_process(executable: &str, arguments: &[String]) -> VideoProcessingResult<()> {
    let output = Command::new(executable).args(arguments).output().await?;
    if output.status.success() {
        return Ok(());
    }

    Err(VideoProcessingError::FfmpegFailed(
        String::from_utf8_lossy(&output.stderr).to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use mediaforge_types::VideoResolution;

    #[test]
    fn transcode_arguments_are_built_without_shell_interpolation() {
        let profile = VideoProfile {
            codec: VideoCodec::H264,
            container: VideoContainer::Mp4,
            resolution: Some(VideoResolution::P720),
            crf: Some(23),
            bitrate_kbps: None,
        };

        let arguments = build_transcode_arguments(
            Path::new("/tmp/input.mp4"),
            Path::new("/tmp/output.mp4"),
            &profile,
            Some(1),
            VideoAcceleration::None,
        )
        .unwrap();

        assert!(arguments
            .windows(2)
            .any(|pair| pair[0] == "-threads" && pair[1] == "1"));
        assert!(arguments.contains(&"libx264".to_string()));
        assert!(arguments.contains(&"scale=-2:720".to_string()));
        assert!(arguments.contains(&"+faststart".to_string()));
    }

    #[test]
    fn nvidia_transcode_arguments_use_nvenc_quality_option() {
        let profile = VideoProfile {
            codec: VideoCodec::H264,
            container: VideoContainer::Mp4,
            resolution: Some(VideoResolution::P720),
            crf: Some(23),
            bitrate_kbps: None,
        };

        let arguments = build_transcode_arguments(
            Path::new("/tmp/input.mp4"),
            Path::new("/tmp/output.mp4"),
            &profile,
            Some(1),
            VideoAcceleration::Nvidia,
        )
        .unwrap();

        assert!(arguments.contains(&"h264_nvenc".to_string()));
        assert!(arguments
            .windows(2)
            .any(|pair| pair[0] == "-cq" && pair[1] == "23"));
        assert!(!arguments.contains(&"-crf".to_string()));
    }

    #[test]
    fn nvidia_acceleration_rejects_av1() {
        let profile = VideoProfile {
            codec: VideoCodec::Av1,
            container: VideoContainer::Mkv,
            resolution: Some(VideoResolution::P720),
            crf: Some(23),
            bitrate_kbps: None,
        };

        let result = build_transcode_arguments(
            Path::new("/tmp/input.mkv"),
            Path::new("/tmp/output.mkv"),
            &profile,
            Some(1),
            VideoAcceleration::Nvidia,
        );

        assert!(matches!(
            result,
            Err(VideoProcessingError::InvalidRequest(_))
        ));
    }

    #[test]
    fn rejects_too_many_screenshots() {
        let request = VideoProcessingRequest {
            profile: VideoProfile {
                codec: VideoCodec::H264,
                container: VideoContainer::Mp4,
                resolution: Some(VideoResolution::P720),
                crf: Some(23),
                bitrate_kbps: None,
            },
            screenshots: (0..=MAX_SCREENSHOT_REQUESTS)
                .map(|_| mediaforge_types::ScreenshotRequest {
                    timestamp_seconds: 1.0,
                    output_format: ImageOutputFormat::Jpeg,
                })
                .collect(),
            generate_cover: false,
            generate_hls: false,
        };

        assert!(matches!(
            validate_request(&request),
            Err(VideoProcessingError::InvalidRequest(_))
        ));
    }
}
