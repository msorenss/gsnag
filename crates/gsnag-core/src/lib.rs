//! Display geometry and captured images, independent of Wayland and GTK.

use anyhow::{Result, ensure};
use image::{RgbaImage, imageops};
use serde::Serialize;

mod region;
pub use region::{Rect, crop_region, desktop_bounds};

#[derive(Debug, Clone, Default, Serialize)]
pub struct Output {
    pub id: u32,
    pub name: String,
    pub description: String,
    pub x: i32,
    pub y: i32,
    pub logical_width: u32,
    pub logical_height: u32,
    pub pixel_width: u32,
    pub pixel_height: u32,
    pub scale: i32,
    pub transform: u32,
}

pub struct CapturedOutput {
    pub output: Output,
    /// Pixels in display orientation, before logical scaling.
    pub image: RgbaImage,
}

/// Compose at one pixel per logical desktop coordinate. Gaps stay transparent.
pub fn compose(frames: &[CapturedOutput]) -> Result<RgbaImage> {
    let bounds = desktop_bounds(frames.iter().map(|frame| &frame.output))?;
    let (width, height) = (bounds.width() as u32, bounds.height() as u32);
    let mut result = RgbaImage::new(width, height);
    for frame in frames {
        let o = &frame.output;
        let scaled = imageops::resize(
            &frame.image,
            o.logical_width,
            o.logical_height,
            imageops::FilterType::Lanczos3,
        );
        imageops::overlay(
            &mut result,
            &scaled,
            i64::from(o.x) - bounds.left,
            i64::from(o.y) - bounds.top,
        );
    }
    Ok(result)
}

pub fn validate_image_size(width: u32, height: u32) -> Result<()> {
    ensure!(width > 0 && height > 0, "Empty image dimensions");
    ensure!(
        u64::from(width) * u64::from(height) <= 100_000_000,
        "Image exceeds 100 megapixels"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;

    #[test]
    fn composes_negative_origins_mixed_scales_and_gaps() {
        let frames = vec![
            CapturedOutput {
                output: Output {
                    x: -2,
                    logical_width: 2,
                    logical_height: 2,
                    ..Output::default()
                },
                image: RgbaImage::from_pixel(4, 4, Rgba([255, 0, 0, 255])),
            },
            CapturedOutput {
                output: Output {
                    x: 1,
                    y: 1,
                    logical_width: 1,
                    logical_height: 1,
                    ..Output::default()
                },
                image: RgbaImage::from_pixel(1, 1, Rgba([0, 255, 0, 255])),
            },
        ];
        let result = compose(&frames).unwrap();
        assert_eq!(result.dimensions(), (4, 2));
        assert_eq!(result.get_pixel(0, 0).0, [255, 0, 0, 255]);
        assert_eq!(result.get_pixel(2, 1).0, [0, 0, 0, 0]);
        assert_eq!(result.get_pixel(3, 1).0, [0, 255, 0, 255]);
    }

    #[test]
    fn rejects_empty_and_unbounded_images() {
        assert!(compose(&[]).is_err());
        assert!(validate_image_size(0, 10).is_err());
        assert!(validate_image_size(u32::MAX, u32::MAX).is_err());
    }
}
