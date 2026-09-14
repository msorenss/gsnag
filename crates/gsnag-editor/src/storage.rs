//! Bounded project loading and atomic exports. Projects retain unredacted pixels.
use std::{
    fs::File,
    io::{BufReader, Cursor, Read, Write},
    path::Path,
};

use anyhow::{Context, Result, bail, ensure};
use image::{DynamicImage, ImageFormat, ImageReader, Limits, RgbImage, RgbaImage};
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;
use zip::{ZipArchive, ZipWriter, write::SimpleFileOptions};

use crate::model::{Content, Document};

const MAX_JSON: u64 = 16 * 1024 * 1024;
const MAX_PNG: u64 = 410_000_000;
#[derive(Serialize, Deserialize)]
struct Project {
    version: u32,
    document: Content,
}

pub fn atomic_write(
    path: &Path,
    overwrite: bool,
    write: impl FnOnce(&mut File) -> Result<()>,
) -> Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut temp = NamedTempFile::new_in(parent)
        .with_context(|| format!("Cannot create a temporary file in {}", parent.display()))?;
    write(temp.as_file_mut())?;
    temp.as_file_mut().flush()?;
    temp.as_file().sync_all()?;
    if overwrite {
        temp.persist(path).map_err(|e| e.error)?;
    } else {
        temp.persist_noclobber(path)
            .map_err(|e| e.error)
            .with_context(|| {
                format!(
                    "Cannot save {}; the destination may already exist",
                    path.display()
                )
            })?;
    }
    Ok(())
}

fn decode(
    reader: impl std::io::BufRead + std::io::Seek,
    format: Option<ImageFormat>,
) -> Result<RgbaImage> {
    let mut reader = ImageReader::new(reader);
    if let Some(format) = format {
        reader.set_format(format);
    } else {
        reader = reader.with_guessed_format()?;
    }
    ensure!(
        matches!(
            reader.format(),
            Some(ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::WebP)
        ),
        "Open a PNG, JPEG, WebP or .gsnag project"
    );
    let mut limits = Limits::default();
    limits.max_image_width = Some(32_767);
    limits.max_image_height = Some(32_767);
    limits.max_alloc = Some(400_000_000);
    reader.limits(limits);
    let image = reader.decode()?.into_rgba8();
    gsnag_core::validate_image_size(image.width(), image.height())?;
    Ok(image)
}

pub fn is_project(path: &Path) -> bool {
    path.extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("gsnag"))
}
pub fn load(path: &Path) -> Result<Document> {
    let mut doc = if is_project(path) {
        let mut zip = ZipArchive::new(File::open(path)?)?;
        ensure!(
            zip.len() == 2,
            "Project must contain document.json and base.png"
        );
        let json = read_entry(&mut zip, "document.json", MAX_JSON)?;
        let project: Project = serde_json::from_slice(&json)?;
        ensure!(
            project.version == 1,
            "Unsupported .gsnag project version {}",
            project.version
        );
        let png = read_entry(&mut zip, "base.png", MAX_PNG)?;
        let mut doc = Document::new(decode(Cursor::new(png), Some(ImageFormat::Png))?)?;
        project.document.validate(doc.image_bounds())?;
        doc.content = project.document;
        doc
    } else {
        Document::new(decode(BufReader::new(File::open(path)?), None)?)?
    };
    // An unchanged imported image is already safely stored at its source path.
    doc.mark_saved();
    Ok(doc)
}
fn read_entry(zip: &mut ZipArchive<File>, name: &str, limit: u64) -> Result<Vec<u8>> {
    let file = zip.by_name(name)?;
    ensure!(file.size() <= limit, "Project entry {name} is too large");
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= limit,
        "Project entry {name} exceeded its size limit"
    );
    Ok(bytes)
}

pub fn save_project(doc: &Document, path: &Path, overwrite: bool) -> Result<()> {
    ensure!(is_project(path), "Project filenames must end in .gsnag");
    doc.content.validate(doc.image_bounds())?;
    let json = serde_json::to_vec_pretty(&Project {
        version: 1,
        document: doc.content.clone(),
    })?;
    ensure!(
        json.len() as u64 <= MAX_JSON,
        "Project metadata exceeds 16 MiB"
    );
    atomic_write(path, overwrite, |file| {
        let mut zip = ZipWriter::new(file);
        zip.start_file(
            "document.json",
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated),
        )?;
        zip.write_all(&json)?;
        zip.start_file(
            "base.png",
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored),
        )?;
        let mut png = Cursor::new(Vec::new());
        doc.base.write_to(&mut png, ImageFormat::Png)?;
        zip.write_all(png.get_ref())?;
        zip.finish()?;
        Ok(())
    })
}

pub fn export_image(image: &RgbaImage, path: &Path, overwrite: bool) -> Result<()> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    ensure!(
        matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "webp"),
        "Export filename must end in .png, .jpg, .jpeg or .webp"
    );
    atomic_write(path, overwrite, |file| {
        match ext.as_str() {
            "png" => image.write_to(file, ImageFormat::Png)?,
            "webp" => image.write_to(file, ImageFormat::WebP)?,
            "jpg" | "jpeg" => {
                let flattened = RgbImage::from_fn(image.width(), image.height(), |x, y| {
                    let [r, g, b, a] = image.get_pixel(x, y).0;
                    let blend = |v| {
                        ((u32::from(v) * u32::from(a) + 255 * (255 - u32::from(a)) + 127) / 255)
                            as u8
                    };
                    image::Rgb([blend(r), blend(g), blend(b)])
                });
                image::codecs::jpeg::JpegEncoder::new_with_quality(file, 90)
                    .encode_image(&DynamicImage::ImageRgb8(flattened))?;
            }
            _ => bail!("Unsupported export format"),
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        model::{Annotation, Kind, Point, Style},
        render,
    };
    #[test]
    fn project_roundtrip_keeps_editable_objects_pixels_and_export() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("roundtrip.gsnag");
        let mut doc = Document::new(RgbaImage::from_pixel(
            80,
            60,
            image::Rgba([20, 40, 60, 255]),
        ))
        .unwrap();
        for kind in [
            Kind::Arrow,
            Kind::Text,
            Kind::Blur,
            Kind::Step,
            Kind::Callout,
        ] {
            doc.add(Annotation::new(
                kind,
                Point::new(10.0, 10.0),
                Point::new(60.0, 40.0),
                Style::default(),
            ))
            .unwrap();
        }
        save_project(&doc, &path, false).unwrap();
        let loaded = load(&path).unwrap();
        assert_eq!(doc.content, loaded.content);
        assert_eq!(doc.base, loaded.base);
        assert!(!loaded.dirty());
        assert_eq!(
            render::render(&doc).unwrap(),
            render::render(&loaded).unwrap()
        );
        assert!(save_project(&doc, &path, false).is_err());
    }
    #[test]
    fn failed_atomic_writes_preserve_existing_files_and_remove_temporary_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keep.png");
        std::fs::write(&path, b"original").unwrap();
        assert!(
            atomic_write(&path, true, |f| {
                f.write_all(b"partial")?;
                bail!("simulated encoder failure")
            })
            .is_err()
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"original");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }
    #[test]
    fn exports_all_formats_and_flattens_jpeg_alpha_on_white() {
        let dir = tempfile::tempdir().unwrap();
        let image = RgbaImage::new(10, 10);
        for ext in ["png", "jpg", "webp"] {
            let path = dir.path().join(format!("test.{ext}"));
            export_image(&image, &path, false).unwrap();
            let loaded = load(&path).unwrap();
            assert_eq!(loaded.base.dimensions(), (10, 10));
            if ext == "jpg" {
                assert!(loaded.base.get_pixel(0, 0).0[0] > 245);
            } else {
                assert_eq!(loaded.base.get_pixel(0, 0).0[3], 0);
            }
        }
    }
    #[test]
    fn rejects_unknown_project_versions_and_invalid_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.gsnag");
        let file = File::create(&path).unwrap();
        let mut zip = ZipWriter::new(file);
        zip.start_file("document.json", SimpleFileOptions::default())
            .unwrap();
        zip.write_all(br#"{"version":99,"document":{"annotations":[],"crop":null,"effects":{"shadow":false,"torn_edge":false}}}"#).unwrap();
        zip.start_file("base.png", SimpleFileOptions::default())
            .unwrap();
        zip.finish().unwrap();
        assert!(load(&path).unwrap_err().to_string().contains("version"));
    }
}
