//! Serializable document objects and reversible edits, independent of the UI.
use std::sync::Arc;

use anyhow::{Result, ensure};
use image::RgbaImage;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}
impl Point {
    pub fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }
    pub fn distance(self, other: Self) -> f64 {
        (self.x - other.x).hypot(self.y - other.y)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Bounds {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}
impl Bounds {
    pub fn between(a: Point, b: Point) -> Self {
        Self {
            x: a.x.min(b.x),
            y: a.y.min(b.y),
            width: (a.x - b.x).abs().max(1.0),
            height: (a.y - b.y).abs().max(1.0),
        }
    }
    pub fn right(self) -> f64 {
        self.x + self.width
    }
    pub fn bottom(self) -> f64 {
        self.y + self.height
    }
    pub fn contains(self, p: Point, tolerance: f64) -> bool {
        p.x >= self.x - tolerance
            && p.x <= self.right() + tolerance
            && p.y >= self.y - tolerance
            && p.y <= self.bottom() + tolerance
    }
    pub fn corners(self) -> [Point; 4] {
        [
            Point::new(self.x, self.y),
            Point::new(self.right(), self.y),
            Point::new(self.right(), self.bottom()),
            Point::new(self.x, self.bottom()),
        ]
    }
    pub fn intersection(self, other: Self) -> Option<Self> {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let width = self.right().min(other.right()) - x;
        let height = self.bottom().min(other.bottom()) - y;
        (width >= 1.0 && height >= 1.0).then_some(Self {
            x,
            y,
            width,
            height,
        })
    }
    fn valid(self) -> bool {
        [self.x, self.y, self.width, self.height]
            .iter()
            .all(|v| v.is_finite() && v.abs() <= 1_000_000.0)
            && self.width >= 1.0
            && self.height >= 1.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    Rect,
    Ellipse,
    Line,
    Arrow,
    Freehand,
    Text,
    Highlight,
    Blur,
    Pixelate,
    Step,
    Callout,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Style {
    pub color: [f64; 4],
    pub width: f64,
    pub font_size: f64,
    pub filled: bool,
}
impl Default for Style {
    fn default() -> Self {
        Self {
            color: [0.92, 0.20, 0.18, 1.0],
            width: 4.0,
            font_size: 28.0,
            filled: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Annotation {
    pub id: u64,
    pub kind: Kind,
    pub bounds: Bounds,
    pub points: Vec<Point>,
    pub style: Style,
    pub text: String,
    pub number: u32,
}

impl Annotation {
    pub fn new(kind: Kind, start: Point, end: Point, style: Style) -> Self {
        let points = match kind {
            Kind::Line | Kind::Arrow | Kind::Freehand => vec![start, end],
            _ => Vec::new(),
        };
        Self {
            id: 0,
            kind,
            bounds: Bounds::between(start, end),
            points,
            style,
            text: "Text".into(),
            number: 1,
        }
    }
    pub fn translated(&self, dx: f64, dy: f64) -> Self {
        let mut a = self.clone();
        a.bounds.x += dx;
        a.bounds.y += dy;
        for p in &mut a.points {
            p.x += dx;
            p.y += dy;
        }
        a
    }
    pub fn resized(&self, bounds: Bounds) -> Self {
        let mut a = self.clone();
        for p in &mut a.points {
            p.x = bounds.x + (p.x - self.bounds.x) * bounds.width / self.bounds.width;
            p.y = bounds.y + (p.y - self.bounds.y) * bounds.height / self.bounds.height;
        }
        a.bounds = bounds;
        a
    }
    pub fn handles(&self) -> Vec<Point> {
        if matches!(self.kind, Kind::Line | Kind::Arrow) {
            self.points.clone()
        } else {
            self.bounds.corners().to_vec()
        }
    }
    pub fn resize_handle(&self, index: usize, point: Point, proportional: bool) -> Self {
        if matches!(self.kind, Kind::Line | Kind::Arrow) {
            let mut result = self.clone();
            if let Some(p) = result.points.get_mut(index) {
                *p = point;
            }
            result.bounds = Bounds::between(result.points[0], result.points[1]);
            return result;
        }
        let anchor = self.bounds.corners()[(index + 2) % 4];
        let end = if proportional {
            let ratio = self.bounds.width / self.bounds.height;
            let dx = point.x - anchor.x;
            let dy = point.y - anchor.y;
            let width = dx.abs().max(dy.abs() * ratio);
            Point::new(
                anchor.x + if dx < 0.0 { -width } else { width },
                anchor.y
                    + if dy < 0.0 {
                        -width / ratio
                    } else {
                        width / ratio
                    },
            )
        } else {
            point
        };
        self.resized(Bounds::between(anchor, end))
    }
    pub fn hit_test(&self, p: Point, tolerance: f64) -> bool {
        let tolerance = tolerance + self.style.width / 2.0;
        match self.kind {
            Kind::Line | Kind::Arrow | Kind::Freehand => {
                self.points
                    .windows(2)
                    .any(|s| segment_distance(p, s[0], s[1]) <= tolerance)
                    || (self.kind == Kind::Arrow
                        && self.arrow_head().is_some_and(|[a, b, c]| {
                            segment_distance(p, a, b) <= tolerance
                                || segment_distance(p, b, c) <= tolerance
                                || segment_distance(p, c, a) <= tolerance
                        }))
            }
            Kind::Ellipse => {
                let b = self.bounds;
                let rx = b.width / 2.0;
                let ry = b.height / 2.0;
                let r = ((p.x - b.x - rx) / rx).hypot((p.y - b.y - ry) / ry);
                if self.style.filled {
                    r <= 1.0 + tolerance / rx.min(ry)
                } else {
                    (r - 1.0).abs() <= tolerance / rx.min(ry)
                }
            }
            Kind::Rect if !self.style.filled => {
                let corners = self.bounds.corners();
                (0..4).any(|i| segment_distance(p, corners[i], corners[(i + 1) % 4]) <= tolerance)
            }
            _ => self.bounds.contains(p, tolerance),
        }
    }
    pub fn arrow_head(&self) -> Option<[Point; 3]> {
        let [start, end] = self.points.as_slice() else {
            return None;
        };
        let angle = (end.y - start.y).atan2(end.x - start.x);
        let length = (self.style.width * 4.0).max(12.0).min(start.distance(*end));
        Some([
            *end,
            Point::new(
                end.x - length * (angle - 0.45).cos(),
                end.y - length * (angle - 0.45).sin(),
            ),
            Point::new(
                end.x - length * (angle + 0.45).cos(),
                end.y - length * (angle + 0.45).sin(),
            ),
        ])
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(self.bounds.valid(), "Invalid annotation bounds");
        ensure!(
            self.points.len() <= 100_000 && self.text.len() <= 100_000,
            "Annotation is too large"
        );
        ensure!(
            self.points.iter().all(|p| p.x.is_finite()
                && p.y.is_finite()
                && p.x.abs() <= 1_000_000.0
                && p.y.abs() <= 1_000_000.0),
            "Invalid annotation point"
        );
        ensure!(
            self.style
                .color
                .iter()
                .all(|v| v.is_finite() && (0.0..=1.0).contains(v)),
            "Invalid annotation color"
        );
        ensure!(
            (0.5..=100.0).contains(&self.style.width)
                && (4.0..=300.0).contains(&self.style.font_size),
            "Invalid stroke or font size"
        );
        ensure!(
            !matches!(self.kind, Kind::Line | Kind::Arrow) || self.points.len() == 2,
            "Line/arrow must have two endpoints"
        );
        ensure!(
            self.kind != Kind::Freehand || self.points.len() >= 2,
            "Freehand needs at least two points"
        );
        Ok(())
    }
}

fn segment_distance(p: Point, a: Point, b: Point) -> f64 {
    let dx = b.x - a.x;
    let dy = b.y - a.y;
    let length2 = dx * dx + dy * dy;
    if length2 <= f64::EPSILON {
        return p.distance(a);
    }
    let t = ((p.x - a.x) * dx + (p.y - a.y) * dy) / length2;
    p.distance(Point::new(
        a.x + t.clamp(0.0, 1.0) * dx,
        a.y + t.clamp(0.0, 1.0) * dy,
    ))
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Effects {
    pub shadow: bool,
    pub torn_edge: bool,
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Content {
    pub annotations: Vec<Annotation>,
    pub crop: Option<Bounds>,
    pub effects: Effects,
}
impl Content {
    pub fn validate(&self, image_bounds: Bounds) -> Result<()> {
        ensure!(self.annotations.len() <= 10_000, "Too many annotations");
        let mut ids = std::collections::HashSet::new();
        let mut total_points = 0;
        for a in &self.annotations {
            ensure!(ids.insert(a.id), "Duplicate annotation ID");
            a.validate()?;
            total_points += a.points.len();
        }
        ensure!(total_points <= 1_000_000, "Too many freehand points");
        if let Some(crop) = self.crop {
            ensure!(
                crop.valid() && image_bounds.intersection(crop) == Some(crop),
                "Crop is outside the base image"
            );
            ensure!(
                [crop.x, crop.y, crop.width, crop.height]
                    .iter()
                    .all(|v| v.fract() == 0.0),
                "Crop coordinates must be whole pixels"
            );
        }
        let crop = self.crop.unwrap_or(image_bounds);
        let padding = if self.effects.shadow { 40.0 } else { 0.0 };
        ensure!(
            crop.width + padding <= 32_767.0 && crop.height + padding <= 32_767.0,
            "Image effects exceed the editor's 32767-pixel dimension limit"
        );
        gsnag_core::validate_image_size(
            (crop.width + padding) as u32,
            (crop.height + padding) as u32,
        )?;
        Ok(())
    }
}

// Commands own only annotation metadata. The immutable base pixels are never
// cloned into undo history. One completed gesture produces one command.
#[derive(Debug, Clone)]
struct Edit {
    before: Content,
    after: Content,
}
trait Command {
    fn apply(&self, content: &mut Content);
    fn undo(&self, content: &mut Content);
}
impl Command for Edit {
    fn apply(&self, content: &mut Content) {
        *content = self.after.clone();
    }
    fn undo(&self, content: &mut Content) {
        *content = self.before.clone();
    }
}

#[derive(Debug)]
pub struct Document {
    pub base: Arc<RgbaImage>,
    pub content: Content,
    undo: Vec<Edit>,
    redo: Vec<Edit>,
    saved: Option<Content>,
}
impl Document {
    pub fn new(base: RgbaImage) -> Result<Self> {
        gsnag_core::validate_image_size(base.width(), base.height())?;
        ensure!(
            base.width() <= 32_767 && base.height() <= 32_767,
            "Editor supports at most 32767 pixels per dimension"
        );
        Ok(Self {
            base: Arc::new(base),
            content: Content::default(),
            undo: vec![],
            redo: vec![],
            saved: None,
        })
    }
    pub fn image_bounds(&self) -> Bounds {
        Bounds {
            x: 0.0,
            y: 0.0,
            width: self.base.width() as f64,
            height: self.base.height() as f64,
        }
    }
    pub fn dirty(&self) -> bool {
        self.saved.as_ref() != Some(&self.content)
    }
    pub fn mark_saved(&mut self) {
        self.saved = Some(self.content.clone());
    }
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }
    pub fn change(&mut self, update: impl FnOnce(&mut Content)) -> Result<()> {
        let mut next = self.content.clone();
        update(&mut next);
        next.validate(self.image_bounds())?;
        if next != self.content {
            let command = Edit {
                before: self.content.clone(),
                after: next,
            };
            command.apply(&mut self.content);
            self.undo.push(command);
            self.redo.clear();
            if self.undo.len() > 100 {
                self.undo.remove(0);
            }
        }
        Ok(())
    }
    pub fn add(&mut self, mut annotation: Annotation) -> Result<u64> {
        annotation.id = self
            .content
            .annotations
            .iter()
            .map(|a| a.id)
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("Annotation IDs exhausted"))?;
        let id = annotation.id;
        self.change(|c| c.annotations.push(annotation))?;
        Ok(id)
    }
    pub fn replace(&mut self, annotation: Annotation) -> Result<()> {
        self.change(|c| {
            if let Some(a) = c.annotations.iter_mut().find(|a| a.id == annotation.id) {
                *a = annotation;
            }
        })
    }
    pub fn remove(&mut self, id: u64) -> Result<()> {
        self.change(|c| c.annotations.retain(|a| a.id != id))
    }
    pub fn reorder(&mut self, id: u64, forward: bool) -> Result<()> {
        self.change(|c| {
            if let Some(i) = c.annotations.iter().position(|a| a.id == id) {
                let j = if forward {
                    (i + 1).min(c.annotations.len() - 1)
                } else {
                    i.saturating_sub(1)
                };
                c.annotations.swap(i, j);
            }
        })
    }
    pub fn undo(&mut self) {
        if let Some(e) = self.undo.pop() {
            e.undo(&mut self.content);
            self.redo.push(e);
        }
    }
    pub fn redo(&mut self) {
        if let Some(e) = self.redo.pop() {
            e.apply(&mut self.content);
            self.undo.push(e);
        }
    }
    pub fn hit_test(&self, p: Point, tolerance: f64) -> Option<u64> {
        self.content
            .annotations
            .iter()
            .rev()
            .find(|a| a.hit_test(p, tolerance))
            .map(|a| a.id)
    }
    pub fn annotation(&self, id: u64) -> Option<&Annotation> {
        self.content.annotations.iter().find(|a| a.id == id)
    }
    pub fn next_step(&self) -> u32 {
        self.content
            .annotations
            .iter()
            .filter(|a| a.kind == Kind::Step)
            .map(|a| a.number)
            .max()
            .unwrap_or(0)
            .saturating_add(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn rect() -> Annotation {
        Annotation::new(
            Kind::Rect,
            Point::new(10.0, 10.0),
            Point::new(100.0, 80.0),
            Style::default(),
        )
    }
    #[test]
    fn hit_testing_respects_outlines_line_distance_and_z_order() {
        let mut doc = Document::new(RgbaImage::new(200, 200)).unwrap();
        let a = doc.add(rect()).unwrap();
        assert_eq!(doc.hit_test(Point::new(50.0, 40.0), 2.0), None);
        assert_eq!(doc.hit_test(Point::new(10.0, 40.0), 2.0), Some(a));
        let mut filled = rect();
        filled.style.filled = true;
        let b = doc.add(filled).unwrap();
        assert_eq!(doc.hit_test(Point::new(10.0, 40.0), 2.0), Some(b));
        doc.reorder(b, false).unwrap();
        assert_eq!(doc.hit_test(Point::new(10.0, 40.0), 2.0), Some(a));
        let line = Annotation::new(
            Kind::Line,
            Point::new(0.0, 0.0),
            Point::new(100.0, 100.0),
            Style::default(),
        );
        assert!(line.hit_test(Point::new(50.0, 50.0), 2.0));
        assert!(!line.hit_test(Point::new(80.0, 20.0), 2.0));
    }
    #[test]
    fn undo_redo_preserves_base_and_save_state_across_branches() {
        let mut doc = Document::new(RgbaImage::new(200, 200)).unwrap();
        let base = doc.base.clone();
        doc.mark_saved();
        let id = doc.add(rect()).unwrap();
        doc.mark_saved();
        doc.replace(doc.annotation(id).unwrap().translated(20.0, 10.0))
            .unwrap();
        assert!(doc.dirty());
        doc.undo();
        assert!(!doc.dirty());
        doc.undo();
        assert!(doc.content.annotations.is_empty());
        doc.redo();
        doc.redo();
        assert_eq!(doc.annotation(id).unwrap().bounds.x, 30.0);
        doc.undo();
        doc.remove(id).unwrap();
        assert!(!doc.can_redo());
        assert!(Arc::ptr_eq(&base, &doc.base));
    }
    #[test]
    fn resize_preserves_arrow_direction_and_validates_loaded_content() {
        let arrow = Annotation::new(
            Kind::Arrow,
            Point::new(100.0, 100.0),
            Point::new(10.0, 10.0),
            Style::default(),
        );
        let resized = arrow.resize_handle(1, Point::new(25.0, 80.0), false);
        assert_eq!(resized.points[0], Point::new(100.0, 100.0));
        assert_eq!(resized.points[1], Point::new(25.0, 80.0));
        let mut bad = rect();
        bad.style.width = f64::NAN;
        assert!(bad.validate().is_err());
        let mut doc = Document::new(RgbaImage::new(200, 200)).unwrap();
        assert!(
            doc.change(|c| c.crop = Some(Bounds {
                x: -1.0,
                y: 0.0,
                width: 20.0,
                height: 20.0
            }))
            .is_err()
        );
        assert!(!doc.can_undo());
    }
}
