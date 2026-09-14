use anyhow::{Result, ensure};
use image::{RgbaImage, imageops};

use crate::{Output, validate_image_size};

/// Half-open rectangle in logical desktop coordinates, including negative origins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub left: i64,
    pub top: i64,
    pub right: i64,
    pub bottom: i64,
}

impl Rect {
    pub fn from_output(output: &Output) -> Self {
        Self {
            left: i64::from(output.x),
            top: i64::from(output.y),
            right: i64::from(output.x) + i64::from(output.logical_width),
            bottom: i64::from(output.y) + i64::from(output.logical_height),
        }
    }

    pub fn width(self) -> i64 {
        self.right - self.left
    }

    pub fn height(self) -> i64 {
        self.bottom - self.top
    }

    pub fn contains(self, x: i64, y: i64) -> bool {
        x >= self.left && x < self.right && y >= self.top && y < self.bottom
    }

    pub fn intersection(self, other: Self) -> Option<Self> {
        let result = Self {
            left: self.left.max(other.left),
            top: self.top.max(other.top),
            right: self.right.min(other.right),
            bottom: self.bottom.min(other.bottom),
        };
        (result.width() > 0 && result.height() > 0).then_some(result)
    }
}

pub fn desktop_bounds<'a>(outputs: impl IntoIterator<Item = &'a Output>) -> Result<Rect> {
    let mut bounds: Option<Rect> = None;
    for output in outputs {
        ensure!(
            output.logical_width > 0 && output.logical_height > 0,
            "Output has no logical size"
        );
        let rect = Rect::from_output(output);
        bounds = Some(match bounds {
            None => rect,
            Some(b) => Rect {
                left: b.left.min(rect.left),
                top: b.top.min(rect.top),
                right: b.right.max(rect.right),
                bottom: b.bottom.max(rect.bottom),
            },
        });
    }
    let bounds = bounds.ok_or_else(|| anyhow::anyhow!("No outputs to compose"))?;
    validate_image_size(
        u32::try_from(bounds.width())?,
        u32::try_from(bounds.height())?,
    )?;
    Ok(bounds)
}

/// Crop the same frozen logical desktop that the selection UI displayed.
pub fn crop_region(desktop: &RgbaImage, bounds: Rect, region: Rect) -> Result<RgbaImage> {
    ensure!(
        i64::from(desktop.width()) == bounds.width()
            && i64::from(desktop.height()) == bounds.height(),
        "Desktop image does not match its logical bounds"
    );
    ensure!(
        bounds.intersection(region) == Some(region),
        "Selection is empty or outside the desktop"
    );
    Ok(imageops::crop_imm(
        desktop,
        (region.left - bounds.left) as u32,
        (region.top - bounds.top) as u32,
        region.width() as u32,
        region.height() as u32,
    )
    .to_image())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CapturedOutput, compose};
    use image::Rgba;

    #[test]
    fn crops_across_scaled_outputs_negative_origin_and_gap() {
        let frames = [
            CapturedOutput {
                output: Output {
                    x: -3,
                    y: -1,
                    logical_width: 2,
                    logical_height: 2,
                    ..Output::default()
                },
                image: RgbaImage::from_pixel(4, 4, Rgba([255, 0, 0, 255])),
            },
            CapturedOutput {
                output: Output {
                    x: 0,
                    y: -1,
                    logical_width: 2,
                    logical_height: 2,
                    ..Output::default()
                },
                image: RgbaImage::from_pixel(2, 2, Rgba([0, 255, 0, 255])),
            },
        ];
        let bounds = desktop_bounds(frames.iter().map(|f| &f.output)).unwrap();
        let desktop = compose(&frames).unwrap();
        let crop = crop_region(
            &desktop,
            bounds,
            Rect {
                left: -2,
                top: -1,
                right: 1,
                bottom: 1,
            },
        )
        .unwrap();
        assert_eq!(crop.dimensions(), (3, 2));
        assert_eq!(crop.get_pixel(0, 0).0, [255, 0, 0, 255]);
        assert_eq!(crop.get_pixel(1, 0).0, [0, 0, 0, 0]);
        assert_eq!(crop.get_pixel(2, 0).0, [0, 255, 0, 255]);
        assert!(crop_region(&desktop, bounds, Rect { right: 5, ..bounds }).is_err());
        assert!(
            crop_region(
                &desktop,
                bounds,
                Rect {
                    right: bounds.left,
                    ..bounds
                }
            )
            .is_err()
        );
        assert!(crop_region(&RgbaImage::new(1, 1), bounds, bounds).is_err());
    }
}
