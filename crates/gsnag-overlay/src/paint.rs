use anyhow::{Result, ensure};
use gsnag_core::Rect;
use gtk4::cairo::{Context, Filter, FontSlant, FontWeight, Format, ImageSurface};
use image::RgbaImage;

use crate::selection::Selection;

pub fn image_surface(image: &RgbaImage) -> Result<ImageSurface> {
    gsnag_core::validate_image_size(image.width(), image.height())?;
    let width = i32::try_from(image.width())?;
    let height = i32::try_from(image.height())?;
    let stride = Format::ARgb32.stride_for_width(image.width())?;
    ensure!(
        stride as u32 == image.width() * 4,
        "Unexpected Cairo image stride"
    );
    let mut data = Vec::with_capacity(image.as_raw().len());
    for pixel in image.pixels() {
        let [r, g, b, a] = pixel.0;
        let premultiply = |v| (u32::from(v) * u32::from(a) + 127) / 255;
        let argb =
            (u32::from(a) << 24) | (premultiply(r) << 16) | (premultiply(g) << 8) | premultiply(b);
        data.extend_from_slice(&argb.to_ne_bytes());
    }
    Ok(ImageSurface::create_for_data(
        data,
        Format::ARgb32,
        width,
        height,
        stride,
    )?)
}

fn rectangle(cr: &Context, rect: Rect) {
    cr.rectangle(
        rect.left as f64,
        rect.top as f64,
        rect.width() as f64,
        rect.height() as f64,
    );
}

fn label(cr: &Context, x: f64, y: f64, lines: &[String], available: f64) -> Result<()> {
    cr.save()?;
    cr.select_font_face("Sans", FontSlant::Normal, FontWeight::Normal);
    cr.set_font_size(14.0);
    let width = lines
        .iter()
        .map(|s| cr.text_extents(s).map(|e| e.x_advance()))
        .collect::<std::result::Result<Vec<_>, _>>()?
        .into_iter()
        .fold(0.0, f64::max)
        + 24.0;
    let width = width.min(available.max(1.0));
    cr.rectangle(x, y, width, lines.len() as f64 * 22.0 + 16.0);
    cr.set_source_rgba(0.06, 0.08, 0.12, 0.96);
    cr.fill_preserve()?;
    cr.clip();
    cr.set_source_rgb(0.96, 0.97, 1.0);
    for (index, line) in lines.iter().enumerate() {
        cr.move_to(x + 12.0, y + 23.0 + index as f64 * 22.0);
        cr.show_text(line)?;
    }
    cr.restore()?;
    Ok(())
}

pub fn draw(
    cr: &Context,
    frozen: &ImageSurface,
    selection: &Selection,
    output: Rect,
    message: &str,
) -> Result<()> {
    cr.save()?;
    cr.translate(-output.left as f64, -output.top as f64);
    cr.set_source_rgb(0.08, 0.08, 0.08);
    cr.paint()?;
    cr.set_source_surface(
        frozen,
        selection.bounds.left as f64,
        selection.bounds.top as f64,
    )?;
    cr.paint()?;
    // Dim four disjoint strips, leaving the selected frozen pixels untouched.
    cr.set_source_rgba(0.0, 0.0, 0.0, 0.48);
    if let Some(rect) = selection.rect.and_then(|r| r.intersection(output)) {
        for strip in [
            Rect {
                bottom: rect.top,
                ..output
            },
            Rect {
                top: rect.bottom,
                ..output
            },
            Rect {
                top: rect.top,
                bottom: rect.bottom,
                right: rect.left,
                ..output
            },
            Rect {
                top: rect.top,
                bottom: rect.bottom,
                left: rect.right,
                ..output
            },
        ] {
            rectangle(cr, strip);
        }
    } else {
        rectangle(cr, output);
    }
    cr.fill()?;
    if let Some(rect) = selection.rect {
        rectangle(cr, rect);
        cr.set_source_rgb(0.2, 0.75, 1.0);
        cr.set_line_width(2.0);
        cr.stroke()?;
        for (x, y) in [
            (rect.left, rect.top),
            (rect.right, rect.top),
            (rect.left, rect.bottom),
            (rect.right, rect.bottom),
        ] {
            cr.rectangle(x as f64 - 4.0, y as f64 - 4.0, 8.0, 8.0);
            cr.set_source_rgb(1.0, 1.0, 1.0);
            cr.fill_preserve()?;
            cr.set_source_rgb(0.1, 0.5, 0.9);
            cr.stroke()?;
        }
    }
    if let Some((px, py)) = selection.pointer.filter(|(x, y)| output.contains(*x, *y)) {
        cr.set_line_width(1.0);
        cr.set_source_rgba(1.0, 1.0, 1.0, 0.7);
        cr.move_to(output.left as f64, py as f64 + 0.5);
        cr.line_to(output.right as f64, py as f64 + 0.5);
        cr.move_to(px as f64 + 0.5, output.top as f64);
        cr.line_to(px as f64 + 0.5, output.bottom as f64);
        cr.stroke()?;
        if output.width() >= 200 && output.height() >= 260 {
            let size = 126.0;
            let x = if px + 160 < output.right {
                px as f64 + 28.0
            } else {
                px as f64 - size - 28.0
            }
            .clamp(output.left as f64 + 8.0, output.right as f64 - size - 8.0);
            let y = if py + 210 < output.bottom {
                py as f64 + 28.0
            } else {
                py as f64 - size - 62.0
            }
            .clamp(output.top as f64 + 8.0, output.bottom as f64 - size - 42.0);
            cr.save()?;
            cr.rectangle(x, y, size, size);
            cr.clip();
            cr.set_source_rgb(0.1, 0.1, 0.1);
            cr.paint()?;
            cr.translate(x + size / 2.0, y + size / 2.0);
            cr.scale(6.0, 6.0);
            cr.set_source_surface(
                frozen,
                (selection.bounds.left - px) as f64 - 0.5,
                (selection.bounds.top - py) as f64 - 0.5,
            )?;
            cr.source().set_filter(Filter::Nearest);
            cr.paint()?;
            cr.restore()?;
            cr.set_source_rgb(0.2, 0.75, 1.0);
            cr.rectangle(x, y, size, size);
            cr.rectangle(x + size / 2.0 - 3.0, y + size / 2.0 - 3.0, 6.0, 6.0);
            cr.stroke()?;
            label(cr, x, y + size, &[format!("{px}, {py}")], size)?;
        }
    }
    let mut hints = vec![
        "Drag to select · Enter to save · Esc / right-click to cancel".to_string(),
        "Drag inside to move · Corners resize · Shift-drag locks ratio".to_string(),
        "Arrows move · Ctrl+arrows resize · Shift+arrows: 10 px".to_string(),
    ];
    if let Some(rect) = selection.rect {
        hints.insert(
            0,
            format!(
                "{} × {} px   at {}, {}",
                rect.width(),
                rect.height(),
                rect.left,
                rect.top
            ),
        );
    }
    if !message.is_empty() {
        hints.push(message.to_string());
    }
    label(
        cr,
        output.left as f64 + 12.0,
        output.top as f64 + 12.0,
        &hints,
        output.width() as f64 - 24.0,
    )?;
    cr.restore()?;
    Ok(())
}
