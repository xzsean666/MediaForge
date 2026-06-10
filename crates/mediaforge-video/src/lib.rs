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

#[derive(Debug, Clone)]
pub struct FfmpegProcessor {
    ffmpeg_path: String,
    ffprobe_path: String,
}

impl FfmpegProcessor {
    pub fn new(ffmpeg_path: String, ffprobe_path: String) -> Self {
        Self {
            ffmpeg_path,
            ffprobe_path,
        }
    }

    pub async fn transcode(
        &self,
        input_path: impl AsRef<Path>,
        output_path: impl AsRef<Path>,
        profile: &VideoProfile,
    ) -> VideoProcessingResult<()> {
        validate_profile(profile)?;
        let arguments =
            build_transcode_arguments(input_path.as_ref(), output_path.as_ref(), profile);
        run_process(&self.ffmpeg_path, &arguments).await
    }

    pub async fn screenshot(
        &self,
        input_path: impl AsRef<Path>,
        output_path: impl AsRef<Path>,
        timestamp_seconds: f64,
        output_format: ImageOutputFormat,
    ) -> VideoProcessingResult<()> {
        if timestamp_seconds < 0.0 {
            return Err(VideoProcessingError::InvalidRequest(
                "screenshot timestamp must be non-negative".to_string(),
            ));
        }

        let mut arguments = vec![
            "-y".to_string(),
            "-ss".to_string(),
            timestamp_seconds.to_string(),
            "-i".to_string(),
            input_path.as_ref().display().to_string(),
            "-frames:v".to_string(),
            "1".to_string(),
        ];

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
        let arguments = vec![
            "-y".to_string(),
            "-i".to_string(),
            input_path.as_ref().display().to_string(),
            "-codec:v".to_string(),
            "libx264".to_string(),
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
        ];
        run_process(&self.ffmpeg_path, &arguments).await?;
        Ok(playlist)
    }

    pub async fn process_request(
        &self,
        input_path: impl AsRef<Path>,
        working_directory: impl AsRef<Path>,
        request: &VideoProcessingRequest,
    ) -> VideoProcessingResult<Vec<PathBuf>> {
        validate_profile(&request.profile)?;
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

pub fn build_transcode_arguments(
    input_path: &Path,
    output_path: &Path,
    profile: &VideoProfile,
) -> Vec<String> {
    let mut arguments = vec![
        "-y".to_string(),
        "-i".to_string(),
        input_path.display().to_string(),
        "-c:v".to_string(),
        codec_name(profile.codec).to_string(),
    ];

    if let Some(resolution) = profile.resolution {
        arguments.push("-vf".to_string());
        arguments.push(format!("scale=-2:{}", resolution.height()));
    }

    if let Some(crf) = profile.crf {
        arguments.push("-crf".to_string());
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
    arguments
}

fn validate_profile(profile: &VideoProfile) -> VideoProcessingResult<()> {
    if profile.container == VideoContainer::Mp4 && profile.codec == VideoCodec::Av1 {
        return Err(VideoProcessingError::InvalidRequest(
            "AV1 MP4 support depends on the FFmpeg build; use MKV for portable AV1 output"
                .to_string(),
        ));
    }

    if let Some(crf) = profile.crf {
        if crf > 63 {
            return Err(VideoProcessingError::InvalidRequest(
                "crf must be between 0 and 63".to_string(),
            ));
        }
    }

    Ok(())
}

fn codec_name(codec: VideoCodec) -> &'static str {
    match codec {
        VideoCodec::H264 => "libx264",
        VideoCodec::H265 => "libx265",
        VideoCodec::Av1 => "libsvtav1",
    }
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
        );

        assert!(arguments.contains(&"libx264".to_string()));
        assert!(arguments.contains(&"scale=-2:720".to_string()));
        assert!(arguments.contains(&"+faststart".to_string()));
    }
}
