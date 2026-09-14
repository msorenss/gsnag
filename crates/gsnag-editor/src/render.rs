//! The canvas, file exports and clipboard all use the same Cairo/Pango renderer.
use anyhow::{Result, ensure};
use gtk4::{
    cairo::{Context, Format, ImageSurface, LineCap, LineJoin},
    pango,
};
use image::{RgbaImage, imageops};

use crate::model::{Annotation, Bounds, Document, Kind, Point};

pub struct Rendered {
    pub image: RgbaImage,
    pub surface: ImageSurface,
    /// Source-image coordinate represented by the top-left exported pixel.
    pub origin: Point,
}

pub fn image_surface(image: &RgbaImage) -> Result<ImageSurface> {
    let stride = Format::ARgb32.stride_for_width(image.width())?;
    ensure!(
        stride as u32 == image.width() * 4,
        "Unexpected Cairo image stride"
    );
    let mut data = Vec::with_capacity(image.as_raw().len());
    for pixel in image.pixels() {
        let [r, g, b, a] = pixel.0;
        let pre = |v| (u32::from(v) * u32::from(a) + 127) / 255;
        data.extend_from_slice(
            &((u32::from(a) << 24) | (pre(r) << 16) | (pre(g) << 8) | pre(b)).to_ne_bytes(),
        );
    }
    Ok(ImageSurface::create_for_data(
        data,
        Format::ARgb32,
        i32::try_from(image.width())?,
        i32::try_from(image.height())?,
        stride,
    )?)
}

fn to_rgba(surface: &mut ImageSurface) -> Result<RgbaImage> {
    surface.flush();
    let (width, height, stride) = (
        surface.width() as u32,
        surface.height() as u32,
        surface.stride() as usize,
    );
    let mut image = RgbaImage::new(width, height);
    let data = surface.data()?;
    for (x, y, pixel) in image.enumerate_pixels_mut() {
        let start = y as usize * stride + x as usize * 4;
        let argb = u32::from_ne_bytes(data[start..start + 4].try_into()?);
        let a = argb >> 24;
        let straight = |v: u32| {
            if a == 0 {
                0
            } else {
                ((v * 255 + a / 2) / a).min(255) as u8
            }
        };
        *pixel = image::Rgba([
            straight((argb >> 16) & 255),
            straight((argb >> 8) & 255),
            straight(argb & 255),
            a as u8,
        ]);
    }
    Ok(image)
}

fn rect_path(cr: &Context, b: Bounds) {
    cr.rectangle(b.x, b.y, b.width, b.height);
}
fn color(cr: &Context, rgba: [f64; 4]) {
    cr.set_source_rgba(rgba[0], rgba[1], rgba[2], rgba[3]);
}
fn text(cr: &Context, value: &str, b: Bounds, size: f64, center: bool) -> Result<()> {
    let layout = pangocairo::functions::create_layout(cr);
    let mut font = pango::FontDescription::from_string("Sans");
    font.set_absolute_size(size * pango::SCALE as f64);
    layout.set_font_description(Some(&font));
    layout.set_text(value);
    layout.set_width((b.width.max(1.0) * pango::SCALE as f64) as i32);
    layout.set_wrap(pango::WrapMode::WordChar);
    if center {
        layout.set_alignment(pango::Alignment::Center);
    }
    let (_, height) = layout.pixel_size();
    cr.save()?;
    rect_path(cr, b);
    cr.clip();
    cr.move_to(
        b.x,
        if center {
            b.y + (b.height - height as f64) / 2.0
        } else {
            b.y
        },
    );
    pangocairo::functions::show_layout(cr, &layout);
    cr.restore()?;
    Ok(())
}

/// Draw one annotation in original-image coordinates. A missing base is used
/// for lightweight drag previews of image filters; committed filters use pixels.
pub(crate) fn annotation(cr: &Context, a: &Annotation, base: Option<&RgbaImage>) -> Result<()> {
    cr.save()?;
    cr.set_line_width(a.style.width);
    cr.set_line_cap(LineCap::Round);
    cr.set_line_join(LineJoin::Round);
    color(cr, a.style.color);
    let b = a.bounds;
    match a.kind {
        Kind::Rect => {
            rect_path(cr, b);
            if a.style.filled {
                cr.fill()?;
            } else {
                cr.stroke()?;
            }
        }
        Kind::Ellipse => {
            cr.save()?;
            cr.translate(b.x + b.width / 2.0, b.y + b.height / 2.0);
            cr.scale(b.width / 2.0, b.height / 2.0);
            cr.arc(0.0, 0.0, 1.0, 0.0, std::f64::consts::TAU);
            cr.restore()?;
            if a.style.filled {
                cr.fill()?;
            } else {
                cr.stroke()?;
            }
        }
        Kind::Line | Kind::Arrow | Kind::Freehand => {
            if let Some(first) = a.points.first() {
                cr.move_to(first.x, first.y);
                for p in a.points.iter().skip(1) {
                    cr.line_to(p.x, p.y);
                }
                cr.stroke()?;
            }
            if a.kind == Kind::Arrow
                && let Some([tip, left, right]) = a.arrow_head()
            {
                cr.move_to(tip.x, tip.y);
                cr.line_to(left.x, left.y);
                cr.line_to(right.x, right.y);
                cr.close_path();
                cr.fill()?;
            }
        }
        Kind::Highlight => {
            cr.set_source_rgba(
                a.style.color[0],
                a.style.color[1],
                a.style.color[2],
                a.style.color[3] * 0.32,
            );
            rect_path(cr, b);
            cr.fill()?;
        }
        Kind::Text => text(cr, &a.text, b, a.style.font_size, false)?,
        Kind::Step => {
            cr.save()?;
            cr.translate(b.x + b.width / 2.0, b.y + b.height / 2.0);
            cr.scale(b.width / 2.0, b.height / 2.0);
            cr.arc(0.0, 0.0, 1.0, 0.0, std::f64::consts::TAU);
            cr.restore()?;
            cr.fill()?;
            cr.set_source_rgb(1.0, 1.0, 1.0);
            text(
                cr,
                &a.number.to_string(),
                b,
                a.style.font_size.min(b.height * 0.65),
                true,
            )?;
        }
        Kind::Callout => {
            // The bottom fifth forms the speech tail; it stays inside the object bounds.
            cr.move_to(b.x, b.y);
            cr.line_to(b.right(), b.y);
            cr.line_to(b.right(), b.y + b.height * 0.78);
            cr.line_to(b.x + b.width * 0.45, b.y + b.height * 0.78);
            cr.line_to(b.x + b.width * 0.2, b.bottom());
            cr.line_to(b.x + b.width * 0.25, b.y + b.height * 0.78);
            cr.line_to(b.x, b.y + b.height * 0.78);
            cr.close_path();
            cr.fill()?;
            cr.set_source_rgb(1.0, 1.0, 1.0);
            text(
                cr,
                &a.text,
                Bounds {
                    x: b.x + 8.0,
                    y: b.y + 4.0,
                    width: (b.width - 16.0).max(1.0),
                    height: (b.height * 0.78 - 8.0).max(1.0),
                },
                a.style.font_size,
                true,
            )?;
        }
        Kind::Blur | Kind::Pixelate => {
            if let Some(base) = base {
                let limit = Bounds {
                    x: 0.0,
                    y: 0.0,
                    width: base.width() as f64,
                    height: base.height() as f64,
                };
                if let Some(region) = b.intersection(limit) {
                    let x = region.x.floor() as u32;
                    let y = region.y.floor() as u32;
                    let width = (region.right().ceil() as u32).min(base.width()) - x;
                    let height = (region.bottom().ceil() as u32).min(base.height()) - y;
                    let source = imageops::crop_imm(base, x, y, width, height).to_image();
                    let filtered = if a.kind == Kind::Blur {
                        imageops::blur(&source, (a.style.width * 2.0).clamp(2.0, 40.0) as f32)
                    } else {
                        let block = (a.style.width * 3.0).round().clamp(4.0, 128.0) as u32;
                        let small = imageops::resize(
                            &source,
                            width.div_ceil(block).max(1),
                            height.div_ceil(block).max(1),
                            imageops::FilterType::Triangle,
                        );
                        imageops::resize(&small, width, height, imageops::FilterType::Nearest)
                    };
                    let surface = image_surface(&filtered)?;
                    rect_path(cr, b);
                    cr.clip();
                    cr.set_source_surface(&surface, x as f64, y as f64)?;
                    cr.paint()?;
                }
            } else {
                cr.set_source_rgba(0.3, 0.6, 0.9, 0.35);
                rect_path(cr, b);
                cr.fill_preserve()?;
                cr.set_source_rgb(0.2, 0.5, 0.9);
                cr.stroke()?;
            }
        }
    }
    cr.restore()?;
    Ok(())
}

fn image_outline(cr: &Context, crop: Bounds, torn: bool) {
    if !torn {
        rect_path(cr, crop);
        return;
    }
    cr.move_to(crop.x, crop.y);
    cr.line_to(crop.right(), crop.y);
    cr.line_to(crop.right(), crop.bottom());
    let mut x = crop.right();
    let mut index = 0;
    while x > crop.x {
        x = (x - 8.0).max(crop.x);
        let depth = if index % 2 == 0 {
            6.0_f64.min(crop.height / 4.0)
        } else {
            0.0
        };
        cr.line_to(x, crop.bottom() - depth);
        index += 1;
    }
    cr.close_path();
}

pub struct Renderer {
    base: ImageSurface,
}
impl Renderer {
    pub fn new(doc: &Document) -> Result<Self> {
        Ok(Self {
            base: image_surface(&doc.base)?,
        })
    }
    pub fn render(&self, doc: &Document) -> Result<Rendered> {
        self.render_except(doc, None)
    }
    pub fn render_except(&self, doc: &Document, exclude: Option<u64>) -> Result<Rendered> {
        doc.content.validate(doc.image_bounds())?;
        let crop = doc.content.crop.unwrap_or(doc.image_bounds());
        let pad = if doc.content.effects.shadow { 20 } else { 0 };
        let width = crop.width as u32 + pad * 2;
        let height = crop.height as u32 + pad * 2;
        gsnag_core::validate_image_size(width, height)?;
        let mut surface = ImageSurface::create(Format::ARgb32, width as i32, height as i32)?;
        {
            let cr = Context::new(&surface)?;
            if doc.content.effects.shadow {
                let mut mask = RgbaImage::new(width, height);
                for y in pad + 5..(pad + 5 + crop.height as u32).min(height) {
                    for x in pad + 5..(pad + 5 + crop.width as u32).min(width) {
                        mask.put_pixel(x, y, image::Rgba([0, 0, 0, 100]));
                    }
                }
                let shadow = image_surface(&imageops::blur(&mask, 6.0))?;
                cr.set_source_surface(&shadow, 0.0, 0.0)?;
                cr.paint()?;
            }
            cr.translate(pad as f64 - crop.x, pad as f64 - crop.y);
            image_outline(&cr, crop, doc.content.effects.torn_edge);
            cr.clip();
            cr.set_source_surface(&self.base, 0.0, 0.0)?;
            cr.paint()?;
            for a in &doc.content.annotations {
                if Some(a.id) != exclude {
                    annotation(&cr, a, Some(&doc.base))?;
                }
            }
        }
        let image = to_rgba(&mut surface)?;
        Ok(Rendered {
            image,
            surface,
            origin: Point::new(crop.x - pad as f64, crop.y - pad as f64),
        })
    }
}

pub fn render(doc: &Document) -> Result<RgbaImage> {
    Ok(Renderer::new(doc)?.render(doc)?.image)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Style;
    #[test]
    fn export_crop_filters_and_annotations_leave_base_unchanged() {
        let base = RgbaImage::from_fn(80, 60, |x, y| {
            image::Rgba([((x % 2) * 255) as u8, ((y % 2) * 255) as u8, 20, 255])
        });
        let mut doc = Document::new(base.clone()).unwrap();
        let style = Style {
            filled: true,
            ..Style::default()
        };
        doc.add(Annotation::new(
            Kind::Rect,
            Point::new(5.0, 5.0),
            Point::new(20.0, 20.0),
            style.clone(),
        ))
        .unwrap();
        doc.add(Annotation::new(
            Kind::Pixelate,
            Point::new(30.0, 5.0),
            Point::new(60.0, 25.0),
            style.clone(),
        ))
        .unwrap();
        doc.add(Annotation::new(
            Kind::Blur,
            Point::new(30.0, 30.0),
            Point::new(60.0, 55.0),
            style,
        ))
        .unwrap();
        doc.change(|c| {
            c.crop = Some(Bounds {
                x: 5.0,
                y: 5.0,
                width: 60.0,
                height: 50.0,
            })
        })
        .unwrap();
        let result = render(&doc).unwrap();
        assert_eq!(result.dimensions(), (60, 50));
        assert!(result.get_pixel(5, 5).0[0] > 200);
        assert_ne!(result.get_pixel(30, 5), base.get_pixel(35, 10));
        assert_ne!(result.get_pixel(30, 30), base.get_pixel(35, 35));
        assert_eq!(*doc.base, base);
        doc.change(|c| c.effects.shadow = true).unwrap();
        assert_eq!(render(&doc).unwrap().dimensions(), (100, 90));
    }
    #[test]
    fn every_tool_renders_and_unicode_uses_pango() {
        let mut doc = Document::new(RgbaImage::from_pixel(
            300,
            300,
            image::Rgba([255, 255, 255, 255]),
        ))
        .unwrap();
        for kind in [
            Kind::Rect,
            Kind::Ellipse,
            Kind::Line,
            Kind::Arrow,
            Kind::Freehand,
            Kind::Text,
            Kind::Highlight,
            Kind::Blur,
            Kind::Pixelate,
            Kind::Step,
            Kind::Callout,
        ] {
            let mut a = Annotation::new(
                kind,
                Point::new(20.0, 20.0),
                Point::new(250.0, 150.0),
                Style::default(),
            );
            a.text = "Hej, världen! Åäö → العربية".into();
            doc.add(a).unwrap();
        }
        doc.change(|c| c.effects.torn_edge = true).unwrap();
        let result = render(&doc).unwrap();
        assert!(result.pixels().any(|p| p.0[0] < 240));
        assert!(result.pixels().any(|p| p.0[3] == 0));
    }
}
