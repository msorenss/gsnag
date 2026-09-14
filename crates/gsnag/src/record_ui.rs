use anyhow::Result;
use gsnag_i18n::{format as tf, tr};
use gsnag_record::{AudioMode, Control};
use gtk4::{glib, prelude::*};
use libadwaita::prelude::*;
use std::{cell::Cell, rc::Rc, sync::Arc, time::Duration};

fn row(content: &gtk4::Box, label: &str, widget: &impl IsA<gtk4::Widget>) {
    let row = gtk4::Box::new(gtk4::Orientation::Horizontal, 12);
    let text = gtk4::Label::new(Some(&tr(label)));
    text.set_xalign(0.0);
    text.set_hexpand(true);
    row.append(&text);
    row.append(widget);
    content.append(&row);
}
fn dropdown(values: &[&str]) -> gtk4::DropDown {
    gtk4::DropDown::from_strings(
        &values
            .iter()
            .map(|v| tr(v))
            .collect::<Vec<_>>()
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
    )
}
pub fn run() -> Result<()> {
    libadwaita::init()?;
    let main_loop = glib::MainLoop::new(None, false);
    let window = libadwaita::Window::builder()
        .title(tr("gsnag — Recording"))
        .default_width(480)
        .build();
    let view = libadwaita::ToolbarView::new();
    view.add_top_bar(&libadwaita::HeaderBar::new());
    let content = gtk4::Box::new(gtk4::Orientation::Vertical, 12);
    for set in [
        WidgetExt::set_margin_top,
        WidgetExt::set_margin_bottom,
        WidgetExt::set_margin_start,
        WidgetExt::set_margin_end,
    ] {
        set(content.upcast_ref::<gtk4::Widget>(), 24);
    }
    let settings = gtk4::Box::new(gtk4::Orientation::Vertical, 12);
    let report = gsnag_proto::inspect()?;
    let names: Vec<_> = report.outputs.iter().map(|o| o.name.clone()).collect();
    let mut targets = vec![tr("Select region"), tr("Entire desktop")];
    targets.extend(names.clone());
    let target =
        gtk4::DropDown::from_strings(&targets.iter().map(String::as_str).collect::<Vec<_>>());
    row(&settings, "Capture", &target);
    let audio = dropdown(&[
        "No audio",
        "System audio",
        "Microphone",
        "System audio and microphone",
    ]);
    row(&settings, "Audio", &audio);
    let fps = gtk4::SpinButton::with_range(1.0, 60.0, 1.0);
    fps.set_value(15.0);
    row(&settings, "Frames per second", &fps);
    let size = dropdown(&["1280 px", "1920 px", "3840 px"]);
    row(&settings, "Maximum width", &size);
    let format = dropdown(&["MP4", "MKV"]);
    row(&settings, "Format", &format);
    let cursor = gtk4::Switch::new();
    row(&settings, "Include pointer", &cursor);
    let note = gtk4::Label::new(Some(&tr(
        "Audio uses the desktop's default devices. Choose devices in your system sound settings.",
    )));
    note.set_wrap(true);
    note.set_xalign(0.0);
    note.add_css_class("dim-label");
    settings.append(&note);
    let note = gtk4::Label::new(Some(&tr(
        "Keep this window outside the recorded area or minimize it during recording.",
    )));
    note.set_wrap(true);
    note.set_xalign(0.0);
    settings.append(&note);
    content.append(&settings);
    let status = gtk4::Label::new(Some(&tr("Ready to record")));
    status.set_wrap(true);
    status.set_selectable(true);
    content.append(&status);
    let start = gtk4::Button::with_label(&tr("Record…"));
    start.add_css_class("suggested-action");
    content.append(&start);
    let controls = gtk4::Box::new(gtk4::Orientation::Horizontal, 12);
    let pause = gtk4::Button::with_label(&tr("Pause"));
    pause.set_hexpand(true);
    controls.append(&pause);
    let stop = gtk4::Button::with_label(&tr("Stop and save"));
    stop.set_hexpand(true);
    stop.add_css_class("destructive-action");
    controls.append(&stop);
    controls.set_visible(false);
    content.append(&controls);
    view.set_content(Some(&content));
    window.set_content(Some(&view));
    let active = Rc::new(Cell::new(false));
    let control = Rc::new(std::cell::RefCell::new(Arc::new(Control::default())));
    let c = control.clone();
    pause.connect_clicked(move |b| {
        let c = c.borrow();
        c.pause(!c.paused());
        b.set_label(&tr(if c.paused() { "Resume" } else { "Pause" }));
    });
    let c = control.clone();
    stop.connect_clicked(move |b| {
        c.borrow().stop();
        b.set_sensitive(false);
    });
    let a = active.clone();
    let c = control.clone();
    let ml = main_loop.clone();
    let st = status.clone();
    window.connect_close_request(move |_| {
        if a.get() {
            c.borrow().stop();
            st.set_text(&tr("Finishing recording…"));
            glib::Propagation::Stop
        } else {
            ml.quit();
            glib::Propagation::Proceed
        }
    });
    let a = active.clone();
    let c = control.clone();
    let st = status.clone();
    let pb = pause.clone();
    let timer = glib::timeout_add_local(Duration::from_millis(250), move || {
        if a.get() {
            let c = c.borrow();
            let p = c.progress();
            let state = tr(if c.stopped() {
                "Finishing recording…"
            } else if c.paused() {
                "Paused"
            } else {
                "Recording"
            });
            st.set_text(&tf(
                "{state} · {seconds} s · {width} × {height}",
                &[
                    ("state", state),
                    ("seconds", format!("{:.1}", p.seconds)),
                    ("width", p.width.to_string()),
                    ("height", p.height.to_string()),
                ],
            ));
            pb.set_label(&tr(if c.paused() { "Resume" } else { "Pause" }));
        }
        glib::ControlFlow::Continue
    });
    let win = window.clone();
    let st = status.clone();
    let ml = main_loop.clone();
    start.connect_clicked(move |start| {
        if active.get() {
            return;
        }
        let ext = if format.selected() == 0 { "mp4" } else { "mkv" };
        let dialog = gtk4::FileDialog::builder()
            .title(tr("Save recording"))
            .accept_label(tr("Record"))
            .initial_name(format!(
                "gsnag-{}.{}",
                glib::DateTime::now_local()
                    .and_then(|d| d.format("%Y%m%d-%H%M%S"))
                    .map(|s| s.to_string())
                    .unwrap_or_else(|_| "video".into()),
                ext
            ))
            .build();
        let filter = gtk4::FileFilter::new();
        filter.set_name(Some(ext));
        filter.add_pattern(&format!("*.{ext}"));
        dialog.set_default_filter(Some(&filter));
        let filters = gtk4::gio::ListStore::new::<gtk4::FileFilter>();
        filters.append(&filter);
        dialog.set_filters(Some(&filters));
        let win = win.clone();
        let st = st.clone();
        let start = start.clone();
        let active = active.clone();
        let control = control.clone();
        let settings = settings.clone();
        let controls = controls.clone();
        let stop = stop.clone();
        let target = target.selected();
        let audio = audio.selected();
        let fps = fps.value_as_int() as u32;
        let max_width = [1280, 1920, 3840][size.selected() as usize];
        let cursor = cursor.is_active();
        let names = names.clone();
        let _ml = ml.clone();
        dialog.save(
            Some(&win.clone()),
            None::<&gtk4::gio::Cancellable>,
            move |result| {
                let file = match result {
                    Ok(file) => file,
                    Err(_) => return,
                };
                let Some(mut path) = file.path() else {
                    st.set_text(&tr("Choose a local file"));
                    return;
                };
                let explicit_extension = path.extension().is_some();
                if path.extension().is_none() {
                    path.set_extension(ext);
                }
                // FileDialog has already confirmed any existing destination.
                let control_new = Arc::new(Control::default());
                let server = match super::recording::Server::start(control_new.clone()) {
                    Ok(s) => s,
                    Err(e) => {
                        st.set_text(&tf("Error: {error}", &[("error", format!("{e:#}"))]));
                        return;
                    }
                };
                *control.borrow_mut() = control_new.clone();
                active.set(true);
                start.set_sensitive(false);
                win.set_visible(false);
                glib::timeout_add_local_once(Duration::from_millis(300), move || {
                    let name = if target >= 2 {
                        names.get((target - 2) as usize).map(String::as_str)
                    } else {
                        None
                    };
                    let prepared = super::recording::prepare(
                        name,
                        target == 0,
                        gsnag_proto::Backend::Auto,
                        cursor,
                    );
                    win.present();
                    let (outputs, region) = match prepared {
                        Ok(Some(v)) => v,
                        other => {
                            active.set(false);
                            start.set_sensitive(true);
                            st.set_text(&match other {
                                Err(e) => tf("Error: {error}", &[("error", format!("{e:#}"))]),
                                _ => tr("Selection cancelled"),
                            });
                            return;
                        }
                    };
                    settings.set_visible(false);
                    start.set_visible(false);
                    controls.set_visible(true);
                    stop.set_sensitive(true);
                    st.set_text(&tr("Starting recording…"));
                    let options = gsnag_record::Options {
                        output: path,
                        outputs,
                        region,
                        fps,
                        max_width,
                        cursor,
                        backend: gsnag_proto::Backend::Auto,
                        audio: [
                            AudioMode::None,
                            AudioMode::System,
                            AudioMode::Microphone,
                            AudioMode::Both,
                        ][audio as usize],
                        microphone: None,
                        system: None,
                        duration: None,
                        overwrite: explicit_extension,
                    };
                    let (tx, rx) = async_channel::bounded(1);
                    std::thread::spawn(move || {
                        let result = gsnag_record::record(options, control_new);
                        drop(server);
                        let _ = tx.send_blocking(result);
                    });
                    glib::MainContext::default().spawn_local(async move {
                        let result = rx.recv().await;
                        active.set(false);
                        controls.set_visible(false);
                        settings.set_visible(true);
                        start.set_visible(true);
                        start.set_sensitive(true);
                        st.set_text(&match result {
                            Ok(Ok(out)) => {
                                let mut text = tf(
                                    "Recording saved: {path}",
                                    &[("path", out.path.display().to_string())],
                                );
                                if let Some(w) = out.warning {
                                    text.push_str(&format!(
                                        "\n{}",
                                        tf("Stopped early: {reason}", &[("reason", w)])
                                    ));
                                }
                                text
                            }
                            Ok(Err(e)) => tf("Error: {error}", &[("error", format!("{e:#}"))]),
                            Err(e) => tf("Error: {error}", &[("error", e.to_string())]),
                        });
                        win.present();
                    });
                });
            },
        );
    });
    window.present();
    main_loop.run();
    timer.remove();
    window.destroy();
    Ok(())
}
