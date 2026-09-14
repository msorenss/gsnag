use gtk4::{cairo::Context, gdk, glib, prelude::*};

use crate::{
    model::{Annotation, Bounds, Kind, Point},
    render,
    ui::{Editor, Tool},
};
use std::rc::Rc;

#[derive(Debug)]
pub(crate) struct Viewport {
    pub zoom: f64,
    pub x: f64,
    pub y: f64,
    pub fit: bool,
    pub pointer: Point,
}
impl Default for Viewport {
    fn default() -> Self {
        Self {
            zoom: 1.0,
            x: 0.0,
            y: 0.0,
            fit: true,
            pointer: Point::default(),
        }
    }
}
impl Viewport {
    pub fn fit(&mut self, width: f64, height: f64, image_width: f64, image_height: f64) {
        self.zoom = ((width - 48.0) / image_width)
            .min((height - 48.0) / image_height)
            .clamp(0.02, 8.0);
        self.x = (width - image_width * self.zoom) / 2.0;
        self.y = (height - image_height * self.zoom) / 2.0;
    }
    pub fn point(&self, p: Point, origin: Point) -> Point {
        Point::new(
            (p.x - self.x) / self.zoom + origin.x,
            (p.y - self.y) / self.zoom + origin.y,
        )
    }
    pub fn zoom_at(&mut self, zoom: f64, p: Point) {
        let next = zoom.clamp(0.02, 16.0);
        let factor = next / self.zoom;
        self.x = p.x - (p.x - self.x) * factor;
        self.y = p.y - (p.y - self.y) * factor;
        self.zoom = next;
        self.fit = false;
        if next.fract() == 0.0 {
            self.x = self.x.round();
            self.y = self.y.round();
        }
    }
}

pub(crate) enum Drag {
    Draw { start: Point, kind: Kind },
    Move { start: Point, original: Annotation },
    Resize { index: usize, original: Annotation },
    Crop { start: Point },
    Pan { x: f64, y: f64 },
}

pub(crate) fn attach(editor: &Rc<Editor>) {
    let weak = Rc::downgrade(editor);
    editor.area.set_draw_func(move |_, cr, width, height| {
        if let Some(editor) = weak.upgrade()
            && let Err(error) = draw(&editor, cr, width, height)
        {
            editor.report_error(error);
        }
    });
    let motion = gtk4::EventControllerMotion::new();
    let weak = Rc::downgrade(editor);
    motion.connect_motion(move |_, x, y| {
        if let Some(e) = weak.upgrade() {
            e.state.borrow_mut().view.pointer = Point::new(x, y);
        }
    });
    editor.area.add_controller(motion);
    for button in [1, 2] {
        let gesture = gtk4::GestureDrag::new();
        gesture.set_button(button);
        let weak = Rc::downgrade(editor);
        gesture.connect_drag_begin(move |gesture, x, y| {
            if let Some(e) = weak.upgrade() {
                e.area.grab_focus();
                begin(&e, Point::new(x, y), button == 2);
                gesture.set_state(gtk4::EventSequenceState::Claimed);
            }
        });
        let weak = Rc::downgrade(editor);
        gesture.connect_drag_update(move |gesture, dx, dy| {
            if let Some(e) = weak.upgrade()
                && let Some((x, y)) = gesture.start_point()
            {
                update(
                    &e,
                    Point::new(x + dx, y + dy),
                    Point::new(dx, dy),
                    gesture
                        .current_event_state()
                        .contains(gdk::ModifierType::SHIFT_MASK),
                );
            }
        });
        let weak = Rc::downgrade(editor);
        gesture.connect_drag_end(move |gesture, dx, dy| {
            if let Some(e) = weak.upgrade() {
                if let Some((x, y)) = gesture.start_point() {
                    update(
                        &e,
                        Point::new(x + dx, y + dy),
                        Point::new(dx, dy),
                        gesture
                            .current_event_state()
                            .contains(gdk::ModifierType::SHIFT_MASK),
                    );
                }
                end(&e);
            }
        });
        let weak = Rc::downgrade(editor);
        gesture.connect_cancel(move |_, _| {
            if let Some(e) = weak.upgrade() {
                e.cancel_drag();
            }
        });
        editor.area.add_controller(gesture);
    }
    let scroll = gtk4::EventControllerScroll::new(gtk4::EventControllerScrollFlags::BOTH_AXES);
    let weak = Rc::downgrade(editor);
    scroll.connect_scroll(move |controller, dx, dy| {
        if let Some(e) = weak.upgrade() {
            let mut s = e.state.borrow_mut();
            if controller
                .current_event_state()
                .contains(gdk::ModifierType::CONTROL_MASK)
            {
                let pointer = s.view.pointer;
                let zoom = s.view.zoom * (-dy * 0.15).exp();
                s.view.zoom_at(zoom, pointer);
            } else {
                s.view.fit = false;
                s.view.x -= dx * 35.0;
                s.view.y -= dy * 35.0;
            }
            drop(s);
            e.area.queue_draw();
            e.update_status();
        }
        glib::Propagation::Stop
    });
    editor.area.add_controller(scroll);
}

fn clamp(point: Point, bounds: Bounds) -> Point {
    Point::new(
        point.x.clamp(bounds.x, bounds.right()),
        point.y.clamp(bounds.y, bounds.bottom()),
    )
}
fn begin(e: &Rc<Editor>, point: Point, pan: bool) {
    e.cancel_drag();
    let mut s = e.state.borrow_mut();
    let source = s.view.point(point, s.rendered.origin);
    let source = clamp(source, s.doc.image_bounds());
    let tool = if pan { Tool::Pan } else { s.tool };
    match tool {
        Tool::Pan => {
            s.drag = Some(Drag::Pan {
                x: s.view.x,
                y: s.view.y,
            });
            s.view.fit = false;
        }
        Tool::Select => {
            let selected = s.selected.and_then(|id| s.doc.annotation(id).cloned());
            if let Some(a) = selected
                && let Some(index) = a
                    .handles()
                    .iter()
                    .position(|p| p.distance(source) <= 8.0 / s.view.zoom)
            {
                s.drag = Some(Drag::Resize {
                    index,
                    original: a.clone(),
                });
                s.preview = Some(a);
            } else {
                s.selected = s.doc.hit_test(source, 5.0 / s.view.zoom);
                if let Some(a) = s.selected.and_then(|id| s.doc.annotation(id).cloned()) {
                    s.drag = Some(Drag::Move {
                        start: source,
                        original: a.clone(),
                    });
                    s.preview = Some(a);
                }
            }
            if s.preview.is_some() {
                match s.renderer.render_except(&s.doc, s.selected) {
                    Ok(rendered) => s.preview_base = Some(rendered),
                    Err(error) => {
                        drop(s);
                        e.report_error(error);
                        return;
                    }
                }
            }
        }
        Tool::Crop => {
            s.drag = Some(Drag::Crop { start: source });
        }
        Tool::Draw(kind) => {
            let mut a = Annotation::new(kind, source, source, e.style());
            a.text = e.text();
            a.number = s.doc.next_step();
            if kind == Kind::Step {
                a.bounds = Bounds {
                    x: source.x - 22.0,
                    y: source.y - 22.0,
                    width: 44.0,
                    height: 44.0,
                };
            }
            s.preview = Some(a);
            s.drag = Some(Drag::Draw {
                start: source,
                kind,
            });
        }
    }
    drop(s);
    e.sync_selection();
    e.area.queue_draw();
}
fn update(e: &Rc<Editor>, point: Point, delta: Point, shift: bool) {
    let mut s = e.state.borrow_mut();
    let source = clamp(s.view.point(point, s.rendered.origin), s.doc.image_bounds());
    let Some(drag) = s.drag.take() else {
        return;
    };
    match &drag {
        Drag::Pan { x, y } => {
            s.view.x = x + delta.x;
            s.view.y = y + delta.y;
        }
        Drag::Move { start, original } => {
            s.preview = Some(original.translated(source.x - start.x, source.y - start.y));
        }
        Drag::Resize { index, original } => {
            s.preview = Some(original.resize_handle(*index, source, shift));
        }
        Drag::Crop { start } => {
            s.crop_preview = Some(Bounds::between(*start, source));
        }
        Drag::Draw { start, kind } => {
            if let Some(a) = s.preview.as_mut() {
                if *kind == Kind::Freehand {
                    if a.points.last().is_none_or(|p| p.distance(source) >= 0.5)
                        && a.points.len() < 100_000
                    {
                        a.points.push(source);
                    }
                    let left = a.points.iter().map(|p| p.x).fold(f64::INFINITY, f64::min);
                    let top = a.points.iter().map(|p| p.y).fold(f64::INFINITY, f64::min);
                    let right = a
                        .points
                        .iter()
                        .map(|p| p.x)
                        .fold(f64::NEG_INFINITY, f64::max);
                    let bottom = a
                        .points
                        .iter()
                        .map(|p| p.y)
                        .fold(f64::NEG_INFINITY, f64::max);
                    a.bounds = Bounds::between(Point::new(left, top), Point::new(right, bottom));
                } else if *kind != Kind::Step {
                    let mut end = source;
                    if shift {
                        let dx = end.x - start.x;
                        let dy = end.y - start.y;
                        if matches!(kind, Kind::Line | Kind::Arrow) {
                            let angle = (dy.atan2(dx) / std::f64::consts::FRAC_PI_4).round()
                                * std::f64::consts::FRAC_PI_4;
                            end = Point::new(
                                start.x + dx.hypot(dy) * angle.cos(),
                                start.y + dx.hypot(dy) * angle.sin(),
                            );
                        } else {
                            let size = dx.abs().max(dy.abs());
                            end = Point::new(
                                start.x + if dx < 0.0 { -size } else { size },
                                start.y + if dy < 0.0 { -size } else { size },
                            );
                        }
                    }
                    a.bounds = Bounds::between(*start, end);
                    if matches!(kind, Kind::Line | Kind::Arrow) {
                        a.points = vec![*start, end];
                    }
                }
            }
        }
    }
    s.drag = Some(drag);
    drop(s);
    e.area.queue_draw();
}
fn end(e: &Rc<Editor>) {
    let mut s = e.state.borrow_mut();
    let drag = s.drag.take();
    let preview = s.preview.take();
    s.preview_base = None;
    let result = match drag {
        Some(Drag::Draw { kind, .. }) => {
            if let Some(mut a) = preview {
                if matches!(kind, Kind::Text | Kind::Callout)
                    && a.bounds.width < 4.0
                    && a.bounds.height < 4.0
                {
                    a.bounds.width = 240.0_f64
                        .min(s.doc.image_bounds().right() - a.bounds.x)
                        .max(1.0);
                    a.bounds.height = if kind == Kind::Callout { 110.0 } else { 80.0 };
                }
                if kind != Kind::Step
                    && !matches!(kind, Kind::Text | Kind::Callout)
                    && a.bounds.width <= 1.0
                    && a.bounds.height <= 1.0
                {
                    Ok(())
                } else {
                    s.doc.add(a).map(|id| {
                        s.selected = Some(id);
                    })
                }
            } else {
                Ok(())
            }
        }
        Some(Drag::Move { .. } | Drag::Resize { .. }) => {
            if let Some(a) = preview {
                s.doc.replace(a)
            } else {
                Ok(())
            }
        }
        Some(Drag::Crop { .. }) => {
            let crop = s
                .crop_preview
                .take()
                .and_then(|b| b.intersection(s.doc.content.crop.unwrap_or(s.doc.image_bounds())));
            if let Some(b) = crop {
                let crop = Bounds {
                    x: b.x.floor(),
                    y: b.y.floor(),
                    width: b.right().ceil() - b.x.floor(),
                    height: b.bottom().ceil() - b.y.floor(),
                };
                s.view.fit = true;
                s.doc.change(|c| c.crop = Some(crop))
            } else {
                Ok(())
            }
        }
        _ => Ok(()),
    };
    drop(s);
    if let Err(error) = result {
        e.report_error(error);
    } else {
        e.refresh();
    }
}

fn draw(e: &Editor, cr: &Context, width: i32, height: i32) -> anyhow::Result<()> {
    let mut s = e.state.borrow_mut();
    let (iw, ih) = (
        s.rendered.image.width() as f64,
        s.rendered.image.height() as f64,
    );
    if s.view.fit {
        s.view.fit(width as f64, height as f64, iw, ih);
    }
    cr.set_source_rgb(0.14, 0.15, 0.17);
    cr.paint()?;
    cr.save()?;
    cr.translate(s.view.x, s.view.y);
    cr.scale(s.view.zoom, s.view.zoom);
    // Checkerboard makes transparent capture gaps, torn edges and shadows visible.
    cr.rectangle(0.0, 0.0, iw, ih);
    cr.clip();
    cr.set_source_rgb(0.85, 0.85, 0.85);
    cr.paint()?;
    let tile = gtk4::cairo::ImageSurface::create(gtk4::cairo::Format::Rgb24, 32, 32)?;
    let tile_cr = Context::new(&tile)?;
    tile_cr.set_source_rgb(0.85, 0.85, 0.85);
    tile_cr.paint()?;
    tile_cr.rectangle(0.0, 0.0, 16.0, 16.0);
    tile_cr.rectangle(16.0, 16.0, 16.0, 16.0);
    tile_cr.set_source_rgb(0.95, 0.95, 0.95);
    tile_cr.fill()?;
    let pattern = gtk4::cairo::SurfacePattern::create(&tile);
    pattern.set_extend(gtk4::cairo::Extend::Repeat);
    pattern.set_filter(gtk4::cairo::Filter::Nearest);
    cr.set_source(&pattern)?;
    cr.paint()?;
    let rendered = s.preview_base.as_ref().unwrap_or(&s.rendered);
    cr.set_source_surface(&rendered.surface, 0.0, 0.0)?;
    cr.paint()?;
    cr.restore()?;
    cr.save()?;
    cr.translate(s.view.x, s.view.y);
    cr.scale(s.view.zoom, s.view.zoom);
    cr.translate(-s.rendered.origin.x, -s.rendered.origin.y);
    if let Some(preview) = &s.preview {
        render::annotation(cr, preview, None)?;
    }
    if let Some(b) = s.crop_preview {
        cr.set_source_rgba(0.1, 0.6, 1.0, 0.25);
        cr.rectangle(b.x, b.y, b.width, b.height);
        cr.fill_preserve()?;
        cr.set_source_rgb(0.1, 0.6, 1.0);
        cr.set_line_width(1.5 / s.view.zoom);
        cr.stroke()?;
    }
    let selected = s
        .preview
        .as_ref()
        .or_else(|| s.selected.and_then(|id| s.doc.annotation(id)));
    if let Some(a) = selected {
        let b = a.bounds;
        cr.set_source_rgb(0.1, 0.6, 1.0);
        cr.set_line_width(1.0 / s.view.zoom);
        cr.set_dash(&[4.0 / s.view.zoom, 3.0 / s.view.zoom], 0.0);
        cr.rectangle(b.x, b.y, b.width, b.height);
        cr.stroke()?;
        cr.set_dash(&[], 0.0);
        for p in a.handles() {
            let size = 7.0 / s.view.zoom;
            cr.rectangle(p.x - size / 2.0, p.y - size / 2.0, size, size);
            cr.set_source_rgb(1.0, 1.0, 1.0);
            cr.fill_preserve()?;
            cr.set_source_rgb(0.1, 0.6, 1.0);
            cr.stroke()?;
        }
    }
    cr.restore()?;
    Ok(())
}
