use gsnag_i18n::{format as tf, tr};
use std::{
    cell::{Cell, RefCell},
    io::Cursor,
    path::{Path, PathBuf},
    rc::Rc,
};

use anyhow::{Context, Result, anyhow};
use gtk4::{gdk, gio, glib, prelude::*};
use libadwaita::{self as adw, prelude::*};
use tempfile::NamedTempFile;

use crate::{
    canvas::{self, Drag, Viewport},
    model::{Annotation, Bounds, Document, Kind, Point, Style},
    render::{Rendered, Renderer},
    storage,
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tool {
    Select,
    Draw(Kind),
    Crop,
    Pan,
}
impl Tool {
    fn name(self) -> &'static str {
        match self {
            Self::Select => "Select",
            Self::Pan => "Pan",
            Self::Crop => "Crop",
            Self::Draw(kind) => match kind {
                Kind::Rect => "Rectangle",
                Kind::Ellipse => "Ellipse",
                Kind::Line => "Line",
                Kind::Arrow => "Arrow",
                Kind::Freehand => "Freehand",
                Kind::Text => "Text",
                Kind::Highlight => "Highlight",
                Kind::Blur => "Blur",
                Kind::Pixelate => "Pixelate",
                Kind::Step => "Step",
                Kind::Callout => "Callout",
            },
        }
    }
}
pub(crate) struct State {
    pub doc: Document,
    pub renderer: Renderer,
    pub rendered: Rendered,
    pub view: Viewport,
    pub tool: Tool,
    pub selected: Option<u64>,
    pub drag: Option<Drag>,
    pub preview: Option<Annotation>,
    pub preview_base: Option<Rendered>,
    pub crop_preview: Option<Bounds>,
    project_path: Option<PathBuf>,
    export_path: Option<PathBuf>,
    drag_file: Option<NamedTempFile>,
}
pub(crate) struct Editor {
    pub state: RefCell<State>,
    pub area: gtk4::DrawingArea,
    window: adw::Window,
    title: adw::WindowTitle,
    status: gtk4::Label,
    color: gtk4::ColorDialogButton,
    width: gtk4::SpinButton,
    font: gtk4::SpinButton,
    filled: gtk4::CheckButton,
    text: gtk4::TextBuffer,
    number: gtk4::SpinButton,
    undo: gtk4::Button,
    redo: gtk4::Button,
    shadow: gtk4::CheckButton,
    torn: gtk4::CheckButton,
    tools: Vec<(Tool, gtk4::ToggleButton)>,
    syncing: Cell<bool>,
    dialog_open: Cell<bool>,
    main_loop: glib::MainLoop,
}

/// Open the editor on the GTK main thread. `suggested_export` only pre-fills the
/// export dialog; no file is written until the user chooses Save/Export.
pub fn open(
    doc: Document,
    project_path: Option<PathBuf>,
    suggested_export: Option<PathBuf>,
) -> Result<()> {
    gdk::set_allowed_backends("wayland");
    adw::init()
        .context("Cannot initialize the editor; run gsnag in the Wayland desktop session")?;
    let renderer = Renderer::new(&doc)?;
    let rendered = renderer.render(&doc)?;
    let window = adw::Window::builder()
        .title(tr("gsnag — Editor"))
        .default_width(1120)
        .default_height(760)
        .build();
    let root = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    let header = adw::HeaderBar::new();
    let title = adw::WindowTitle::new("gsnag", &tr("Annotation editor"));
    header.set_title_widget(Some(&title));
    let undo = button("Undo", "Undo (Ctrl+Z)");
    let redo = button("Redo", "Redo (Ctrl+Shift+Z)");
    header.pack_start(&undo);
    header.pack_start(&redo);
    let save = button("Save project", "Save editable .gsnag project (Ctrl+S)");
    let export = button("Export…", "Export PNG, JPEG or WebP (Ctrl+E)");
    export.add_css_class("suggested-action");
    let copy = button("Copy", "Copy the rendered image (Ctrl+C)");
    header.pack_end(&export);
    header.pack_end(&copy);
    header.pack_end(&save);
    root.append(&header);
    let body = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
    body.set_vexpand(true);
    let sidebar = gtk4::Box::new(gtk4::Orientation::Vertical, 8);
    sidebar.set_margin_start(12);
    sidebar.set_margin_end(12);
    sidebar.set_margin_top(12);
    sidebar.set_margin_bottom(12);
    sidebar.set_size_request(212, -1);
    let grid = gtk4::Grid::builder()
        .row_spacing(4)
        .column_spacing(4)
        .column_homogeneous(true)
        .build();
    let specs = [
        (Tool::Select, "Select · V"),
        (Tool::Draw(Kind::Rect), "Rect · R"),
        (Tool::Draw(Kind::Ellipse), "Ellipse · E"),
        (Tool::Draw(Kind::Line), "Line · L"),
        (Tool::Draw(Kind::Arrow), "Arrow · A"),
        (Tool::Draw(Kind::Freehand), "Pen · F"),
        (Tool::Draw(Kind::Highlight), "Highlight · H"),
        (Tool::Draw(Kind::Text), "Text · T"),
        (Tool::Draw(Kind::Blur), "Blur · B"),
        (Tool::Draw(Kind::Pixelate), "Pixelate · P"),
        (Tool::Draw(Kind::Step), "Step · N"),
        (Tool::Draw(Kind::Callout), "Callout · C"),
        (Tool::Crop, "Crop · X"),
        (Tool::Pan, "Pan · Space"),
    ];
    let mut tools = Vec::new();
    let mut group: Option<gtk4::ToggleButton> = None;
    for (i, (tool, label)) in specs.into_iter().enumerate() {
        let button = gtk4::ToggleButton::with_label(&tr(label));
        button.set_tooltip_text(Some(&tr(tool.name())));
        button.set_group(group.as_ref());
        if group.is_none() {
            group = Some(button.clone());
            button.set_active(true);
        }
        grid.attach(&button, (i % 2) as i32, (i / 2) as i32, 1, 1);
        tools.push((tool, button));
    }
    sidebar.append(&grid);
    sidebar.append(&gtk4::Separator::new(gtk4::Orientation::Horizontal));
    let color =
        gtk4::ColorDialogButton::new(Some(gtk4::ColorDialog::builder().with_alpha(true).build()));
    color.set_rgba(&gdk::RGBA::new(0.92, 0.20, 0.18, 1.0));
    color.set_tooltip_text(Some(&tr("Annotation color")));
    sidebar.append(&row("Color", &color));
    let swatches = gtk4::Box::new(gtk4::Orientation::Horizontal, 4);
    for rgba in [
        [0.92, 0.20, 0.18, 1.0],
        [1.0, 0.82, 0.0, 1.0],
        [0.15, 0.65, 0.35, 1.0],
        [0.15, 0.50, 0.95, 1.0],
        [1.0, 1.0, 1.0, 1.0],
        [0.0, 0.0, 0.0, 1.0],
    ] {
        let swatch = gtk4::Button::new();
        let area = gtk4::DrawingArea::builder()
            .content_width(14)
            .content_height(14)
            .build();
        area.set_draw_func(move |_, cr, _, _| {
            cr.set_source_rgb(rgba[0], rgba[1], rgba[2]);
            let _ = cr.paint();
        });
        swatch.set_child(Some(&area));
        let target = color.clone();
        swatch.connect_clicked(move |_| {
            target.set_rgba(&gdk::RGBA::new(
                rgba[0] as f32,
                rgba[1] as f32,
                rgba[2] as f32,
                1.0,
            ))
        });
        swatches.append(&swatch);
    }
    sidebar.append(&swatches);
    let width = gtk4::SpinButton::with_range(1.0, 64.0, 1.0);
    width.set_value(4.0);
    sidebar.append(&row("Stroke / filter", &width));
    let font = gtk4::SpinButton::with_range(8.0, 144.0, 1.0);
    font.set_value(28.0);
    sidebar.append(&row("Text size", &font));
    let filled = gtk4::CheckButton::with_label(&tr("Fill shapes"));
    sidebar.append(&filled);
    let text_view = gtk4::TextView::new();
    text_view.set_wrap_mode(gtk4::WrapMode::WordChar);
    text_view.set_left_margin(6);
    text_view.set_top_margin(6);
    let text = text_view.buffer();
    text.set_text(&tr("Text"));
    let text_scroll = gtk4::ScrolledWindow::builder()
        .min_content_height(64)
        .max_content_height(96)
        .child(&text_view)
        .build();
    sidebar.append(
        &gtk4::Label::builder()
            .label(tr("Text / callout"))
            .xalign(0.0)
            .build(),
    );
    sidebar.append(&text_scroll);
    let number = gtk4::SpinButton::with_range(1.0, 99999.0, 1.0);
    sidebar.append(&row("Selected step", &number));
    let apply = button(
        "Apply to selection",
        "Apply color, stroke and text (Ctrl+Enter)",
    );
    sidebar.append(&apply);
    let layers = gtk4::Box::new(gtk4::Orientation::Horizontal, 4);
    let lower = button("Lower", "Lower selected object (Page Down)");
    let raise = button("Raise", "Raise selected object (Page Up)");
    let delete = button("Delete", "Delete selected object");
    layers.append(&lower);
    layers.append(&raise);
    layers.append(&delete);
    sidebar.append(&layers);
    let reset_crop = button("Reset crop", "Restore the full original canvas");
    sidebar.append(&reset_crop);
    let shadow = gtk4::CheckButton::with_label(&tr("Drop shadow"));
    let torn = gtk4::CheckButton::with_label(&tr("Torn bottom edge"));
    sidebar.append(&shadow);
    sidebar.append(&torn);
    let note = gtk4::Label::builder()
        .label(tr(
            "Projects keep the original image. Share a flattened export when hiding content.",
        ))
        .wrap(true)
        .xalign(0.0)
        .max_width_chars(26)
        .build();
    note.add_css_class("dim-label");
    sidebar.append(&note);
    let side_scroll = gtk4::ScrolledWindow::builder()
        .hexpand(false)
        .width_request(280)
        .hscrollbar_policy(gtk4::PolicyType::Never)
        .child(&sidebar)
        .build();
    body.append(&side_scroll);
    let area = gtk4::DrawingArea::builder()
        .hexpand(true)
        .vexpand(true)
        .focusable(true)
        .build();
    area.set_cursor_from_name(Some("default"));
    body.append(&area);
    root.append(&body);
    let footer = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    footer.set_margin_start(12);
    footer.set_margin_end(12);
    footer.set_margin_top(6);
    footer.set_margin_bottom(6);
    let status = gtk4::Label::builder()
        .xalign(0.0)
        .hexpand(true)
        .ellipsize(gtk4::pango::EllipsizeMode::End)
        .build();
    footer.append(&status);
    let drag = button(
        "Drag image",
        "Drag the exported image to another application",
    );
    footer.append(&drag);
    let save_as = button("Save as…", "Save a new .gsnag project (Ctrl+Shift+S)");
    footer.append(&save_as);
    let fit = button("Fit", "Fit image (Ctrl+0)");
    let actual = button("100%", "Actual pixels (Ctrl+1)");
    let minus = button("−", "Zoom out");
    let plus = button("+", "Zoom in");
    for b in [&fit, &actual, &minus, &plus] {
        footer.append(b);
    }
    root.append(&footer);
    window.set_content(Some(&root));
    let editor = Rc::new(Editor {
        state: RefCell::new(State {
            doc,
            renderer,
            rendered,
            view: Viewport::default(),
            tool: Tool::Select,
            selected: None,
            drag: None,
            preview: None,
            preview_base: None,
            crop_preview: None,
            project_path,
            export_path: suggested_export,
            drag_file: None,
        }),
        area,
        window,
        title,
        status,
        color,
        width,
        font,
        filled,
        text,
        number,
        undo,
        redo,
        shadow,
        torn,
        tools,
        syncing: Cell::new(false),
        dialog_open: Cell::new(false),
        main_loop: glib::MainLoop::new(None, false),
    });
    canvas::attach(&editor);
    bind(&editor, &editor.undo, |e| {
        e.cancel_drag();
        e.state.borrow_mut().doc.undo();
        e.refresh();
    });
    bind(&editor, &editor.redo, |e| {
        e.cancel_drag();
        e.state.borrow_mut().doc.redo();
        e.refresh();
    });
    bind(&editor, &apply, |e| e.apply_properties());
    bind(&editor, &delete, |e| e.delete());
    bind(&editor, &raise, |e| e.reorder(true));
    bind(&editor, &lower, |e| e.reorder(false));
    bind(&editor, &reset_crop, |e| {
        e.cancel_drag();
        let result = e.state.borrow_mut().doc.change(|c| c.crop = None);
        e.finish_edit(result);
        e.fit();
    });
    bind(&editor, &fit, |e| e.fit());
    bind(&editor, &actual, |e| e.zoom(1.0));
    bind(&editor, &minus, |e| {
        let zoom = e.state.borrow().view.zoom / 1.25;
        e.zoom(zoom);
    });
    bind(&editor, &plus, |e| {
        let zoom = e.state.borrow().view.zoom * 1.25;
        e.zoom(zoom);
    });
    bind(&editor, &copy, |e| e.copy());
    bind(&editor, &save, |e| e.start_save(false));
    bind(&editor, &save_as, |e| e.start_save(true));
    bind(&editor, &export, |e| e.start_export());
    for (tool, button) in &editor.tools {
        let weak = Rc::downgrade(&editor);
        let tool = *tool;
        button.connect_toggled(move |button| {
            if button.is_active()
                && let Some(e) = weak.upgrade()
            {
                e.cancel_drag();
                e.state.borrow_mut().tool = tool;
                e.area.set_cursor_from_name(Some(if tool == Tool::Pan {
                    "grab"
                } else if tool == Tool::Select {
                    "default"
                } else {
                    "crosshair"
                }));
                e.area.grab_focus();
                e.update_status();
            }
        });
    }
    for button in [&editor.shadow, &editor.torn] {
        let weak = Rc::downgrade(&editor);
        button.connect_toggled(move |_| {
            if let Some(e) = weak.upgrade()
                && !e.syncing.get()
            {
                e.cancel_drag();
                let shadow = e.shadow.is_active();
                let torn = e.torn.is_active();
                let result = e.state.borrow_mut().doc.change(|c| {
                    c.effects.shadow = shadow;
                    c.effects.torn_edge = torn;
                });
                e.finish_edit(result);
            }
        });
    }
    setup_keys(&editor);
    setup_drag(&editor, &drag);
    let weak = Rc::downgrade(&editor);
    editor.window.connect_close_request(move |_| {
        if let Some(e) = weak.upgrade() {
            e.request_close();
        }
        glib::Propagation::Stop
    });
    editor.refresh();
    editor.window.maximize();
    editor.window.present();
    editor.area.grab_focus();
    editor.main_loop.run();
    editor.window.destroy();
    Ok(())
}
fn button(label: &str, tip: &str) -> gtk4::Button {
    gtk4::Button::builder()
        .label(tr(label))
        .tooltip_text(tr(tip))
        .build()
}
fn row(label: &str, widget: &impl IsA<gtk4::Widget>) -> gtk4::Box {
    let row = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    row.append(
        &gtk4::Label::builder()
            .label(tr(label))
            .xalign(0.0)
            .hexpand(true)
            .build(),
    );
    row.append(widget);
    row
}
fn bind(e: &Rc<Editor>, button: &gtk4::Button, action: impl Fn(&Rc<Editor>) + 'static) {
    let weak = Rc::downgrade(e);
    button.connect_clicked(move |_| {
        if let Some(e) = weak.upgrade() {
            action(&e);
        }
    });
}

impl Editor {
    pub fn style(&self) -> Style {
        let c = self.color.rgba();
        Style {
            color: [
                c.red() as f64,
                c.green() as f64,
                c.blue() as f64,
                c.alpha() as f64,
            ],
            width: self.width.value(),
            font_size: self.font.value(),
            filled: self.filled.is_active(),
        }
    }
    pub fn text(&self) -> String {
        self.text
            .text(&self.text.start_iter(), &self.text.end_iter(), false)
            .to_string()
    }
    pub fn report_error(&self, error: anyhow::Error) {
        self.status
            .set_text(&tf("Error: {error}", &[("error", format!("{error:#}"))]));
        eprintln!("gsnag editor: {error:#}");
    }
    pub fn update_status(&self) {
        let s = self.state.borrow();
        self.status.set_text(&tf("{tool} · {width} × {height} px · {count} objects · {zoom}% · Ctrl+wheel zoom · Middle-drag pan", &[
            ("tool",tr(s.tool.name())), ("width",s.rendered.image.width().to_string()), ("height",s.rendered.image.height().to_string()),
            ("count",s.doc.content.annotations.len().to_string()), ("zoom",format!("{:.0}",s.view.zoom*100.0)),
        ]));
    }
    pub fn sync_selection(&self) {
        self.syncing.set(true);
        let s = self.state.borrow();
        if let Some(a) = s.selected.and_then(|id| s.doc.annotation(id)) {
            self.color.set_rgba(&gdk::RGBA::new(
                a.style.color[0] as f32,
                a.style.color[1] as f32,
                a.style.color[2] as f32,
                a.style.color[3] as f32,
            ));
            self.width.set_value(a.style.width);
            self.font.set_value(a.style.font_size);
            self.filled.set_active(a.style.filled);
            self.text.set_text(&a.text);
            self.number.set_value(a.number as f64);
        }
        self.shadow.set_active(s.doc.content.effects.shadow);
        self.torn.set_active(s.doc.content.effects.torn_edge);
        drop(s);
        self.syncing.set(false);
    }
    pub fn refresh(&self) {
        let mut s = self.state.borrow_mut();
        match s.renderer.render(&s.doc) {
            Ok(rendered) => s.rendered = rendered,
            Err(error) => {
                drop(s);
                self.report_error(error);
                return;
            }
        }
        if s.selected.is_some_and(|id| s.doc.annotation(id).is_none()) {
            s.selected = None;
        }
        self.undo.set_sensitive(s.doc.can_undo());
        self.redo.set_sensitive(s.doc.can_redo());
        let name = s
            .project_path
            .as_ref()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .map(str::to_owned)
            .unwrap_or_else(|| tr("Untitled"));
        self.title.set_title(&format!(
            "{}{}",
            if s.doc.dirty() { "● " } else { "" },
            name
        ));
        self.window.set_title(Some(&format!("gsnag — {name}")));
        drop(s);
        self.sync_selection();
        self.update_status();
        self.area.queue_draw();
    }
    pub fn cancel_drag(&self) {
        let mut s = self.state.borrow_mut();
        s.drag = None;
        s.preview = None;
        s.preview_base = None;
        s.crop_preview = None;
        drop(s);
        self.area.queue_draw();
    }
    fn finish_edit(&self, result: Result<()>) {
        match result {
            Ok(()) => self.refresh(),
            Err(error) => self.report_error(error),
        }
    }
    fn apply_properties(&self) {
        self.cancel_drag();
        let mut s = self.state.borrow_mut();
        if let Some(mut a) = s.selected.and_then(|id| s.doc.annotation(id).cloned()) {
            a.style = self.style();
            a.text = self.text();
            a.number = self.number.value_as_int() as u32;
            let result = s.doc.replace(a);
            drop(s);
            self.finish_edit(result);
        } else {
            drop(s);
            self.status.set_text(&tr(
                "Choose an object with Select first; these properties also apply to new objects",
            ));
        }
    }
    fn delete(&self) {
        self.cancel_drag();
        let mut s = self.state.borrow_mut();
        if let Some(id) = s.selected.take() {
            let result = s.doc.remove(id);
            drop(s);
            self.finish_edit(result);
        }
    }
    fn reorder(&self, forward: bool) {
        self.cancel_drag();
        let mut s = self.state.borrow_mut();
        if let Some(id) = s.selected {
            let result = s.doc.reorder(id, forward);
            drop(s);
            self.finish_edit(result);
        }
    }
    fn fit(&self) {
        self.state.borrow_mut().view.fit = true;
        self.area.queue_draw();
    }
    fn zoom(&self, zoom: f64) {
        self.state.borrow_mut().view.zoom_at(
            zoom,
            Point::new(
                self.area.width() as f64 / 2.0,
                self.area.height() as f64 / 2.0,
            ),
        );
        self.area.queue_draw();
        self.update_status();
    }
    fn tool(&self, tool: Tool) {
        if let Some((_, button)) = self.tools.iter().find(|(t, _)| *t == tool) {
            button.set_active(true);
        }
    }
    fn texture(&self) -> gdk::Texture {
        let s = self.state.borrow();
        let image = &s.rendered.image;
        let bytes = glib::Bytes::from_owned(image.as_raw().clone());
        gdk::MemoryTexture::new(
            image.width() as i32,
            image.height() as i32,
            gdk::MemoryFormat::R8g8b8a8,
            &bytes,
            image.width() as usize * 4,
        )
        .upcast()
    }
    fn copy(&self) {
        self.cancel_drag();
        self.area.clipboard().set_texture(&self.texture());
        self.status.set_text(&tr(
            "Copied image — keep the editor open until pasted if no clipboard manager is running",
        ));
    }
    fn start_save(self: &Rc<Self>, save_as: bool) {
        if self.dialog_open.replace(true) {
            return;
        }
        self.cancel_drag();
        let e = self.clone();
        glib::MainContext::default().spawn_local(async move {
            let _ = e.save(save_as).await;
            e.dialog_open.set(false);
        });
    }
    async fn save(self: &Rc<Self>, save_as: bool) -> bool {
        let existing = if save_as {
            None
        } else {
            self.state.borrow().project_path.clone()
        };
        let path = if let Some(path) = existing {
            path
        } else {
            let initial = self.state.borrow().project_path.clone().or_else(|| {
                self.state
                    .borrow()
                    .export_path
                    .as_ref()
                    .map(|p| p.with_extension("gsnag"))
            });
            match self.choose_path(true, initial.as_deref()).await {
                Ok(Some(p)) => p,
                Ok(None) => return false,
                Err(error) => {
                    self.report_error(error);
                    return false;
                }
            }
        };
        let result = storage::save_project(&self.state.borrow().doc, &path, true);
        match result {
            Ok(()) => {
                let mut s = self.state.borrow_mut();
                s.project_path = Some(path.clone());
                s.doc.mark_saved();
                drop(s);
                self.refresh();
                self.status.set_text(&tf(
                    "Project saved: {path}",
                    &[("path", path.display().to_string())],
                ));
                true
            }
            Err(error) => {
                self.report_error(error);
                false
            }
        }
    }
    async fn choose_path(&self, project: bool, initial: Option<&Path>) -> Result<Option<PathBuf>> {
        let dialog = gtk4::FileDialog::builder()
            .title(tr(if project {
                "Save editable project"
            } else {
                "Export image"
            }))
            .accept_label(tr(if project { "Save" } else { "Export" }))
            .modal(true)
            .build();
        let filter = gtk4::FileFilter::new();
        filter.set_name(Some(&tr(if project {
            "gsnag project (*.gsnag)"
        } else {
            "PNG, JPEG or WebP"
        })));
        for ext in if project {
            &["gsnag"][..]
        } else {
            &["png", "jpg", "jpeg", "webp"][..]
        } {
            filter.add_suffix(ext);
        }
        let filters = gio::ListStore::new::<gtk4::FileFilter>();
        filters.append(&filter);
        dialog.set_filters(Some(&filters));
        if let Some(initial) = initial {
            let absolute = if initial.is_absolute() {
                initial.to_path_buf()
            } else {
                std::env::current_dir()?.join(initial)
            };
            dialog.set_initial_file(Some(&gio::File::for_path(absolute)));
        } else {
            dialog.set_initial_name(Some(if project {
                "Untitled.gsnag"
            } else {
                "capture.png"
            }));
        }
        match dialog.save_future(Some(&self.window)).await {
            Ok(file) => Ok(Some(
                file.path().context("Choose a local file destination")?,
            )),
            Err(error)
                if error.matches(gtk4::DialogError::Dismissed)
                    || error.matches(gtk4::DialogError::Cancelled) =>
            {
                Ok(None)
            }
            Err(error) => Err(error.into()),
        }
    }
    fn start_export(self: &Rc<Self>) {
        if self.dialog_open.replace(true) {
            return;
        }
        self.cancel_drag();
        let e = self.clone();
        glib::MainContext::default().spawn_local(async move {
            let initial = e.state.borrow().export_path.clone();
            match e.choose_path(false, initial.as_deref()).await {
                Ok(Some(path)) => {
                    let result =
                        storage::export_image(&e.state.borrow().rendered.image, &path, true);
                    match result {
                        Ok(()) => {
                            e.state.borrow_mut().export_path = Some(path.clone());
                            e.status.set_text(&tf(
                                "Exported {path}",
                                &[("path", path.display().to_string())],
                            ));
                        }
                        Err(error) => e.report_error(error),
                    }
                }
                Ok(None) => {}
                Err(error) => e.report_error(error),
            }
            e.dialog_open.set(false);
        });
    }
    fn request_close(self: &Rc<Self>) {
        if self.dialog_open.replace(true) {
            return;
        }
        self.cancel_drag();
        if !self.state.borrow().doc.dirty() {
            self.main_loop.quit();
            return;
        }
        let dialog = adw::AlertDialog::new(
            Some(&tr("Save the editable project?")),
            Some(&tr(
                "Your annotations have unsaved changes. Image exports do not preserve editable objects.",
            )),
        );
        dialog.add_responses(&[
            ("cancel", &tr("Keep editing")),
            ("discard", &tr("Discard changes")),
            ("save", &tr("Save project")),
        ]);
        dialog.set_response_appearance("discard", adw::ResponseAppearance::Destructive);
        dialog.set_response_appearance("save", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("save"));
        dialog.set_close_response("cancel");
        let e = self.clone();
        glib::MainContext::default().spawn_local(async move {
            let response = dialog.choose_future(Some(&e.window)).await;
            let close = match response.as_str() {
                "discard" => true,
                "save" => e.save(false).await,
                _ => false,
            };
            e.dialog_open.set(false);
            if close {
                e.main_loop.quit();
            }
        });
    }
}
fn setup_keys(e: &Rc<Editor>) {
    let keys = gtk4::EventControllerKey::new();
    keys.set_propagation_phase(gtk4::PropagationPhase::Capture);
    let weak = Rc::downgrade(e);
    keys.connect_key_pressed(move |_, key, _, modifiers| {
        let Some(e) = weak.upgrade() else {
            return glib::Propagation::Proceed;
        };
        if e.dialog_open.get() {
            return glib::Propagation::Proceed;
        }
        if key == gdk::Key::Escape {
            e.cancel_drag();
            e.tool(Tool::Select);
            e.area.grab_focus();
            return glib::Propagation::Stop;
        }
        let ctrl = modifiers.contains(gdk::ModifierType::CONTROL_MASK);
        let shift = modifiers.contains(gdk::ModifierType::SHIFT_MASK);
        let typing = GtkWindowExt::focus(&e.window).is_some_and(|w| {
            w.is::<gtk4::TextView>()
                || w.is::<gtk4::Text>()
                || w.is::<gtk4::Entry>()
                || w.is::<gtk4::SpinButton>()
        });
        if ctrl {
            match key.to_lower() {
                gdk::Key::s => e.start_save(shift),
                gdk::Key::e => e.start_export(),
                gdk::Key::Return => e.apply_properties(),
                gdk::Key::c if !typing => e.copy(),
                gdk::Key::z if !typing => {
                    e.cancel_drag();
                    if shift {
                        e.state.borrow_mut().doc.redo();
                    } else {
                        e.state.borrow_mut().doc.undo();
                    }
                    e.refresh();
                }
                gdk::Key::y if !typing => {
                    e.cancel_drag();
                    e.state.borrow_mut().doc.redo();
                    e.refresh();
                }
                gdk::Key::_0 => e.fit(),
                gdk::Key::_1 => e.zoom(1.0),
                gdk::Key::plus | gdk::Key::equal => {
                    let zoom = e.state.borrow().view.zoom * 1.25;
                    e.zoom(zoom);
                }
                gdk::Key::minus => {
                    let zoom = e.state.borrow().view.zoom / 1.25;
                    e.zoom(zoom);
                }
                gdk::Key::q => e.request_close(),
                _ => return glib::Propagation::Proceed,
            }
        } else if !typing {
            match key.to_lower() {
                gdk::Key::Escape => {
                    e.cancel_drag();
                    e.tool(Tool::Select);
                }
                gdk::Key::Delete | gdk::Key::BackSpace => e.delete(),
                gdk::Key::Page_Up => e.reorder(true),
                gdk::Key::Page_Down => e.reorder(false),
                gdk::Key::Left | gdk::Key::Right | gdk::Key::Up | gdk::Key::Down => {
                    let step = if shift { 10.0 } else { 1.0 };
                    let (dx, dy) = match key {
                        gdk::Key::Left => (-step, 0.0),
                        gdk::Key::Right => (step, 0.0),
                        gdk::Key::Up => (0.0, -step),
                        _ => (0.0, step),
                    };
                    let mut s = e.state.borrow_mut();
                    if let Some(a) = s.selected.and_then(|id| s.doc.annotation(id).cloned()) {
                        let result = s.doc.replace(a.translated(dx, dy));
                        drop(s);
                        e.finish_edit(result);
                    }
                }
                gdk::Key::v => e.tool(Tool::Select),
                gdk::Key::r => e.tool(Tool::Draw(Kind::Rect)),
                gdk::Key::e => e.tool(Tool::Draw(Kind::Ellipse)),
                gdk::Key::l => e.tool(Tool::Draw(Kind::Line)),
                gdk::Key::a => e.tool(Tool::Draw(Kind::Arrow)),
                gdk::Key::f => e.tool(Tool::Draw(Kind::Freehand)),
                gdk::Key::h => e.tool(Tool::Draw(Kind::Highlight)),
                gdk::Key::t => e.tool(Tool::Draw(Kind::Text)),
                gdk::Key::b => e.tool(Tool::Draw(Kind::Blur)),
                gdk::Key::p => e.tool(Tool::Draw(Kind::Pixelate)),
                gdk::Key::n => e.tool(Tool::Draw(Kind::Step)),
                gdk::Key::c => e.tool(Tool::Draw(Kind::Callout)),
                gdk::Key::x => e.tool(Tool::Crop),
                gdk::Key::space => e.tool(Tool::Pan),
                _ => return glib::Propagation::Proceed,
            }
        } else {
            return glib::Propagation::Proceed;
        }
        glib::Propagation::Stop
    });
    e.window.add_controller(keys);
}
fn setup_drag(e: &Rc<Editor>, button: &gtk4::Button) {
    let source = gtk4::DragSource::builder()
        .actions(gdk::DragAction::COPY)
        .build();
    let weak = Rc::downgrade(e);
    source.connect_prepare(move |_, _, _| {
        let e = weak.upgrade()?;
        e.cancel_drag();
        let result = (|| -> Result<gdk::ContentProvider> {
            let mut png = Cursor::new(Vec::new());
            e.state
                .borrow()
                .rendered
                .image
                .write_to(&mut png, image::ImageFormat::Png)?;
            let bytes = glib::Bytes::from_owned(png.into_inner());
            let temp = tempfile::Builder::new()
                .prefix("gsnag-")
                .suffix(".png")
                .tempfile()?;
            std::fs::write(temp.path(), bytes.as_ref())?;
            let files = gdk::FileList::from_array(&[gio::File::for_path(temp.path())]);
            let provider = gdk::ContentProvider::new_union(&[
                gdk::ContentProvider::for_value(&files.to_value()),
                gdk::ContentProvider::for_value(&e.texture().to_value()),
                gdk::ContentProvider::for_bytes("image/png", &bytes),
            ]);
            e.state.borrow_mut().drag_file = Some(temp);
            Ok(provider)
        })();
        match result {
            Ok(p) => Some(p),
            Err(error) => {
                e.report_error(anyhow!("Cannot drag image: {error:#}"));
                None
            }
        }
    });
    button.add_controller(source);
}
