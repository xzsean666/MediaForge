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
            }
            ImageOperation::Crop { width, height, .. } => {
                if *width == 0 || *height == 0 {
                    return Err(ImageProcessingError::InvalidRequest(
                        "crop width and height must be greater than zero".to_string(),
                    ));
                }
            }
            ImageOperation::ImageWatermark { opacity, .. }
            | ImageOperation::TextWatermark { opacity, .. } => {
                if !(0.0..=1.0).contains(opacity) {
                    return Err(ImageProcessingError::InvalidRequest(
                        "watermark opacity must be between 0.0 and 1.0".to_string(),
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
        } => Ok(image.crop_imm(*x, *y, *width, *height)),
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
    ((current_width as f64 / current_height as f64) * target_height as f64).round() as u32
}

fn scale_height(current_width: u32, current_height: u32, target_width: u32) -> u32 {
    ((current_height as f64 / current_width as f64) * target_width as f64).round() as u32
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
}
