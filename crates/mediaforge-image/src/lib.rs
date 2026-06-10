use image::imageops::FilterType;
use image::{DynamicImage, GenericImageView, ImageFormat};
use mediaforge_types::{
    ImageOperation, ImageOutputFormat, ImageProcessingRequest, MediaMetadata, ResizeFit,
    RotationDegrees,
};
use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum ImageProcessingError {
    #[error("invalid image processing request: {0}")]
    InvalidRequest(String),
    #[error(
        "image watermarking requires worker-managed watermark downloads and is not enabled yet"
    )]
    ImageWatermarkUnsupported,
    #[error("text watermarking is not enabled yet")]
    TextWatermarkUnsupported,
    #[error("image IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("image processing error: {0}")]
    Image(#[from] image::ImageError),
}

pub type ImageProcessingResult<T> = Result<T, ImageProcessingError>;

const MAX_IMAGE_DIMENSION: u32 = 16_384;
const MAX_TEXT_WATERMARK_CHARS: usize = 512;
const MAX_TEXT_WATERMARK_FONT_SIZE: u32 = 512;

#[derive(Debug, Clone)]
pub struct ProcessedImage {
    pub metadata: MediaMetadata,
    pub output_format: ImageOutputFormat,
}

pub struct ImageProcessor;

impl ImageProcessor {
    pub fn process_file(
        input_path: impl AsRef<Path>,
        output_path: impl AsRef<Path>,
        request: &ImageProcessingRequest,
    ) -> ImageProcessingResult<ProcessedImage> {
        validate_request(request)?;

        let mut image = image::ImageReader::open(input_path)?
            .with_guessed_format()?
            .decode()?;

        for operation in &request.operations {
            image = apply_operation(image, operation)?;
        }

        write_image(
            &image,
            output_path.as_ref(),
            request.output_format,
            request.quality,
        )?;
        let (width, height) = image.dimensions();

        Ok(ProcessedImage {
            metadata: MediaMetadata {
                width: Some(width),
                height: Some(height),
                duration_seconds: None,
                codec: None,
                bitrate_bps: None,
            },
            output_format: request.output_format,
        })
    }

    pub fn inspect_image(path: impl AsRef<Path>) -> ImageProcessingResult<MediaMetadata> {
        let image = image::ImageReader::open(path)?
            .with_guessed_format()?
            .decode()?;
        let (width, height) = image.dimensions();
        Ok(MediaMetadata {
            width: Some(width),
            height: Some(height),
            duration_seconds: None,
            codec: None,
            bitrate_bps: None,
        })
    }
}

fn validate_request(request: &ImageProcessingRequest) -> ImageProcessingResult<()> {
    if let Some(quality) = request.quality {
        if !(1..=100).contains(&quality) {
            return Err(ImageProcessingError::InvalidRequest(
                "quality must be between 1 and 100".to_string(),
            ));
        }
    }

    for operation in &request.operations {
        match operation {
            ImageOperation::Resize { width, height, .. } => {
                if width.is_none() && height.is_none() {
                    return Err(ImageProcessingError::InvalidRequest(
                        "resize requires width, height, or both".to_string(),
                    ));
                }
                validate_optional_dimension("resize width", *width)?;
                validate_optional_dimension("resize height", *height)?;
            }
            ImageOperation::Crop { width, height, .. } => {
                validate_dimension("crop width", *width)?;
                validate_dimension("crop height", *height)?;
            }
            ImageOperation::ImageWatermark { opacity, .. } => {
                validate_opacity(*opacity)?;
            }
            ImageOperation::TextWatermark {
                text,
                font_size,
                color_hex,
                opacity,
                ..
            } => {
                validate_opacity(*opacity)?;
                if text.is_empty() || text.chars().count() > MAX_TEXT_WATERMARK_CHARS {
                    return Err(ImageProcessingError::InvalidRequest(format!(
                        "text watermark must contain 1 to {MAX_TEXT_WATERMARK_CHARS} characters"
                    )));
                }
                if *font_size == 0 || *font_size > MAX_TEXT_WATERMARK_FONT_SIZE {
                    return Err(ImageProcessingError::InvalidRequest(format!(
                        "font_size must be between 1 and {MAX_TEXT_WATERMARK_FONT_SIZE}"
                    )));
                }
                if !is_hex_color(color_hex) {
                    return Err(ImageProcessingError::InvalidRequest(
                        "color_hex must use #RRGGBB format".to_string(),
                    ));
                }
            }
            ImageOperation::Rotate { .. } => {}
        }
    }

    Ok(())
}

fn apply_operation(
    image: DynamicImage,
    operation: &ImageOperation,
) -> ImageProcessingResult<DynamicImage> {
    match operation {
        ImageOperation::Resize { width, height, fit } => {
            let current_width = image.width();
            let current_height = image.height();
            let target_width = match width {
                Some(width) => *width,
                None => scale_width(
                    current_width,
                    current_height,
                    height.expect("validated height"),
                ),
            };
            let target_height = match height {
                Some(height) => *height,
                None => scale_height(
                    current_width,
                    current_height,
                    width.expect("validated width"),
                ),
            };
            validate_dimension("resize target width", target_width)?;
            validate_dimension("resize target height", target_height)?;

            Ok(match fit {
                ResizeFit::Contain => {
                    image.resize(target_width, target_height, FilterType::Lanczos3)
                }
                ResizeFit::Cover => {
                    image.resize_to_fill(target_width, target_height, FilterType::Lanczos3)
                }
                ResizeFit::Fill => {
                    image.resize_exact(target_width, target_height, FilterType::Lanczos3)
                }
            })
        }
        ImageOperation::Crop {
            x,
            y,
            width,
            height,
        } => {
            if *x >= image.width()
                || *y >= image.height()
                || *width > image.width().saturating_sub(*x)
                || *height > image.height().saturating_sub(*y)
            {
                return Err(ImageProcessingError::InvalidRequest(
                    "crop rectangle must fit within the source image".to_string(),
                ));
            }
            Ok(image.crop_imm(*x, *y, *width, *height))
        }
        ImageOperation::Rotate { degrees } => Ok(match degrees {
            RotationDegrees::Deg90 => image.rotate90(),
            RotationDegrees::Deg180 => image.rotate180(),
            RotationDegrees::Deg270 => image.rotate270(),
        }),
        ImageOperation::ImageWatermark { .. } => {
            Err(ImageProcessingError::ImageWatermarkUnsupported)
        }
        ImageOperation::TextWatermark { .. } => Err(ImageProcessingError::TextWatermarkUnsupported),
    }
}

fn scale_width(current_width: u32, current_height: u32, target_height: u32) -> u32 {
    ((current_width as f64 / current_height as f64) * target_height as f64)
        .round()
        .max(1.0) as u32
}

fn scale_height(current_width: u32, current_height: u32, target_width: u32) -> u32 {
    ((current_height as f64 / current_width as f64) * target_width as f64)
        .round()
        .max(1.0) as u32
}

fn validate_optional_dimension(name: &str, value: Option<u32>) -> ImageProcessingResult<()> {
    if let Some(value) = value {
        validate_dimension(name, value)?;
    }
    Ok(())
}

fn validate_dimension(name: &str, value: u32) -> ImageProcessingResult<()> {
    if value == 0 || value > MAX_IMAGE_DIMENSION {
        return Err(ImageProcessingError::InvalidRequest(format!(
            "{name} must be between 1 and {MAX_IMAGE_DIMENSION}"
        )));
    }
    Ok(())
}

fn validate_opacity(opacity: f32) -> ImageProcessingResult<()> {
    if !(0.0..=1.0).contains(&opacity) {
        return Err(ImageProcessingError::InvalidRequest(
            "watermark opacity must be between 0.0 and 1.0".to_string(),
        ));
    }
    Ok(())
}

fn is_hex_color(value: &str) -> bool {
    let Some(hex) = value.strip_prefix('#') else {
        return false;
    };
    hex.len() == 6 && hex.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn write_image(
    image: &DynamicImage,
    output_path: &Path,
    output_format: ImageOutputFormat,
    quality: Option<u8>,
) -> ImageProcessingResult<()> {
    let mut file = std::fs::File::create(output_path)?;

    match output_format {
        ImageOutputFormat::Jpeg => {
            let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(
                &mut file,
                quality.unwrap_or(85),
            );
            encoder.encode_image(image)?;
        }
        ImageOutputFormat::Png => image.write_to(&mut file, ImageFormat::Png)?,
        ImageOutputFormat::Webp => image.write_to(&mut file, ImageFormat::WebP)?,
        ImageOutputFormat::Avif => image.write_to(&mut file, ImageFormat::Avif)?,
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageBuffer, Rgba};
    use mediaforge_types::{ImageOperation, ImageOutputFormat, ResizeFit};

    #[test]
    fn resize_operation_writes_output_file() {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("input.png");
        let output = directory.path().join("output.jpg");

        let image: ImageBuffer<Rgba<u8>, Vec<u8>> =
            ImageBuffer::from_pixel(20, 10, Rgba([0, 0, 0, 255]));
        image.save(&input).unwrap();

        let processed = ImageProcessor::process_file(
            &input,
            &output,
            &ImageProcessingRequest {
                output_format: ImageOutputFormat::Jpeg,
                quality: Some(80),
                operations: vec![ImageOperation::Resize {
                    width: Some(10),
                    height: Some(10),
                    fit: ResizeFit::Fill,
                }],
            },
        )
        .unwrap();

        assert!(output.exists());
        assert_eq!(processed.metadata.width, Some(10));
        assert_eq!(processed.metadata.height, Some(10));
    }

    #[test]
    fn rejects_invalid_resize_dimensions() {
        let request = ImageProcessingRequest {
            output_format: ImageOutputFormat::Jpeg,
            quality: Some(80),
            operations: vec![ImageOperation::Resize {
                width: Some(0),
                height: Some(10),
                fit: ResizeFit::Fill,
            }],
        };

        assert!(matches!(
            validate_request(&request),
            Err(ImageProcessingError::InvalidRequest(_))
        ));
    }

    #[test]
    fn crop_must_fit_source_image() {
        let image = DynamicImage::new_rgba8(20, 10);
        let result = apply_operation(
            image,
            &ImageOperation::Crop {
                x: 15,
                y: 0,
                width: 10,
                height: 10,
            },
        );

        assert!(matches!(
            result,
            Err(ImageProcessingError::InvalidRequest(_))
        ));
    }
}
