//! Frozen-screen region selection on one GTK4 layer-shell surface per output.

mod paint;
mod selection;

use std::{cell::RefCell, rc::Rc};

use anyhow::{Context, Result, anyhow, ensure};
use gsnag_core::{Output, Rect, desktop_bounds};
use gtk4::{gdk, glib, prelude::*};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use image::RgbaImage;
use selection::Selection;

struct Session {
    selection: RefCell<Selection>,
    message: RefCell<String>,
    windows: RefCell<Vec<glib::WeakRef<gtk4::Window>>>,
    main_loop: glib::MainLoop,
    result: RefCell<Option<Result<Option<Rect>>>>,
}

impl Session {
    fn redraw(&self) {
        for window in self
            .windows
            .borrow()
            .iter()
            .filter_map(glib::WeakRef::upgrade)
        {
            if let Some(child) = window.child() {
                child.queue_draw();
            }
        }
    }

    fn finish(&self, result: Result<Option<Rect>>) {
        if self.result.borrow().is_none() {
            *self.result.borrow_mut() = Some(result);
            self.main_loop.quit();
        }
    }
}

/// Returns a logical desktop rectangle, or `None` when cancelled. Must run on
/// the main thread. All outputs must be captured before any overlay is mapped.
pub fn select(desktop: &RgbaImage, outputs: &[Output]) -> Result<Option<Rect>> {
    let bounds = desktop_bounds(outputs)?;
    ensure!(
        desktop.dimensions() == (bounds.width() as u32, bounds.height() as u32),
        "Frozen image does not match the output layout"
    );
    // Restrict backend selection without changing process environment after threads start.
    gdk::set_allowed_backends("wayland");
    gtk4::init().context("Cannot initialize GTK4; run region capture from the Wayland desktop")?;
    ensure!(
        gtk4_layer_shell::is_supported(),
        "Region selection requires a Wayland compositor with layer-shell support"
    );
    let display = gdk::Display::default().context("No GDK display")?;
    let monitors = display.monitors();
    ensure!(
        monitors.n_items() as usize == outputs.len(),
        "Display layout changed during capture; try again"
    );
    let mut matched = Vec::new();
    for output in outputs {
        let monitor = (0..monitors.n_items())
            .filter_map(|i| monitors.item(i))
            .filter_map(|o| o.downcast::<gdk::Monitor>().ok())
            .find(|m| m.connector().as_deref() == Some(output.name.as_str()))
            .with_context(|| {
                format!(
                    "Captured output {} is no longer available to GTK; try again",
                    output.name
                )
            })?;
        let geo = monitor.geometry();
        ensure!(
            geo.x() == output.x
                && geo.y() == output.y
                && geo.width() as u32 == output.logical_width
                && geo.height() as u32 == output.logical_height,
            "Output {} changed geometry during capture; try again",
            output.name
        );
        matched.push(monitor);
    }
    let surface = paint::image_surface(desktop)?;
    let session = Rc::new(Session {
        selection: RefCell::new(Selection::new(
            bounds,
            outputs.iter().map(Rect::from_output).collect(),
        )),
        message: RefCell::new(String::new()),
        windows: RefCell::new(Vec::new()),
        main_loop: glib::MainLoop::new(None, false),
        result: RefCell::new(None),
    });
    let mut windows = Vec::new();
    let mut monitor_handlers = Vec::new();
    for (output, monitor) in outputs.iter().zip(&matched) {
        let window = gtk4::Window::builder()
            .title(gsnag_i18n::tr("gsnag — select region"))
            .decorated(false)
            .default_width(output.logical_width as i32)
            .default_height(output.logical_height as i32)
            .build();
        window.init_layer_shell();
        window.set_namespace(Some("gsnag-region"));
        window.set_layer(Layer::Overlay);
        window.set_monitor(Some(monitor));
        window.set_keyboard_mode(KeyboardMode::Exclusive);
        window.set_exclusive_zone(-1); // Cover panels too, without reserving desktop space.
        for edge in [Edge::Left, Edge::Right, Edge::Top, Edge::Bottom] {
            window.set_anchor(edge, true);
        }
        let area = gtk4::DrawingArea::builder()
            .hexpand(true)
            .vexpand(true)
            .focusable(true)
            .build();
        area.set_cursor_from_name(Some("crosshair"));
        let rect = Rect::from_output(output);
        let draw_session = session.clone();
        let frozen = surface.clone();
        area.set_draw_func(move |_, cr, width, height| {
            if width as i64 != rect.width() || height as i64 != rect.height() {
                draw_session.finish(Err(anyhow!(
                    "Display size changed during selection: expected {}x{}, got {width}x{height}; try again",
                    rect.width(), rect.height()
                )));
                return;
            }
            if let Err(error) = paint::draw(
                cr,
                &frozen,
                &draw_session.selection.borrow(),
                rect,
                &draw_session.message.borrow(),
            ) {
                draw_session.finish(Err(error));
            }
        });
        // Legacy events retain Wayland's implicit pointer grab: while a button is
        // held, coordinates may extend beyond the originating output's surface.
        let pointer = gtk4::EventControllerLegacy::new();
        pointer.set_propagation_phase(gtk4::PropagationPhase::Capture);
        let input_session = session.clone();
        pointer.connect_event(move |_, event| {
            let Some((x, y)) = event.position() else {
                return glib::Propagation::Proceed;
            };
            let point = (rect.left + x.round() as i64, rect.top + y.round() as i64);
            let shift = event
                .modifier_state()
                .contains(gdk::ModifierType::SHIFT_MASK);
            match event.event_type() {
                gdk::EventType::ButtonPress => {
                    let Some(button) = event.downcast_ref::<gdk::ButtonEvent>() else {
                        return glib::Propagation::Proceed;
                    };
                    match button.button() {
                        1 => input_session.selection.borrow_mut().begin(point),
                        3 => input_session.finish(Ok(None)),
                        _ => return glib::Propagation::Proceed,
                    }
                }
                gdk::EventType::ButtonRelease => {
                    if event
                        .downcast_ref::<gdk::ButtonEvent>()
                        .is_some_and(|b| b.button() == 1)
                    {
                        input_session.selection.borrow_mut().end(point, shift);
                    }
                }
                gdk::EventType::MotionNotify => {
                    input_session.selection.borrow_mut().motion(point, shift)
                }
                _ => return glib::Propagation::Proceed,
            }
            input_session.message.borrow_mut().clear();
            input_session.redraw();
            glib::Propagation::Stop
        });
        window.add_controller(pointer);
        let keys = gtk4::EventControllerKey::new();
        keys.set_propagation_phase(gtk4::PropagationPhase::Capture);
        let key_session = session.clone();
        keys.connect_key_pressed(move |_, key, _, modifiers| {
            let step = if modifiers.contains(gdk::ModifierType::SHIFT_MASK) {
                10
            } else {
                1
            };
            let resize = modifiers.contains(gdk::ModifierType::CONTROL_MASK);
            match key {
                gdk::Key::Escape => key_session.finish(Ok(None)),
                gdk::Key::Return | gdk::Key::KP_Enter => {
                    let rect = key_session.selection.borrow().accepted();
                    if let Some(rect) = rect {
                        key_session.finish(Ok(Some(rect)));
                    } else {
                        *key_session.message.borrow_mut() =
                            gsnag_i18n::tr("Draw a non-empty region on a display first");
                    }
                }
                gdk::Key::Left => key_session.selection.borrow_mut().nudge(-step, 0, resize),
                gdk::Key::Right => key_session.selection.borrow_mut().nudge(step, 0, resize),
                gdk::Key::Up => key_session.selection.borrow_mut().nudge(0, -step, resize),
                gdk::Key::Down => key_session.selection.borrow_mut().nudge(0, step, resize),
                gdk::Key::Tab | gdk::Key::ISO_Left_Tab => {
                    *key_session.message.borrow_mut() = gsnag_i18n::tr(
                        "Window selection is unavailable in this version; drag a region",
                    );
                }
                gdk::Key::Shift_L | gdk::Key::Shift_R => {
                    let mut selection = key_session.selection.borrow_mut();
                    if let Some(point) = selection.pointer {
                        selection.motion(point, true);
                    }
                }
                _ => return glib::Propagation::Proceed,
            }
            key_session.redraw();
            glib::Propagation::Stop
        });
        let release_session = session.clone();
        keys.connect_key_released(move |_, key, _, _| {
            if key == gdk::Key::Shift_L || key == gdk::Key::Shift_R {
                let mut selection = release_session.selection.borrow_mut();
                if let Some(point) = selection.pointer {
                    selection.motion(point, false);
                }
                drop(selection);
                release_session.redraw();
            }
        });
        window.add_controller(keys);
        let close_session = session.clone();
        window.connect_close_request(move |_| {
            close_session.finish(Ok(None));
            glib::Propagation::Stop
        });
        let changed_session = session.clone();
        let handler = monitor.connect_notify_local(None, move |_, property| {
            if matches!(
                property.name(),
                "geometry" | "scale" | "scale-factor" | "valid"
            ) {
                changed_session.finish(Err(anyhow!(
                    "Display layout changed during selection; try again"
                )));
            }
        });
        monitor_handlers.push(handler);
        window.set_child(Some(&area));
        session.windows.borrow_mut().push(window.downgrade());
        windows.push(window);
    }
    let changed_session = session.clone();
    let list_handler = monitors.connect_items_changed(move |_, _, _, _| {
        changed_session.finish(Err(anyhow!(
            "Displays connected or disconnected during selection; try again"
        )));
    });
    for window in &windows {
        window.present();
    }
    if session.result.borrow().is_none() {
        session.main_loop.run();
    }
    for window in windows {
        window.destroy();
    }
    monitors.disconnect(list_handler);
    for (monitor, handler) in matched.iter().zip(monitor_handlers) {
        monitor.disconnect(handler);
    }
    session.result.borrow_mut().take().unwrap_or(Ok(None))
}
