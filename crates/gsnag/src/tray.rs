use gsnag_i18n::{format as tf, tr};
use std::{cell::Cell, process::Command, rc::Rc, time::Duration};

use anyhow::Result;
use async_channel::Sender;
use gtk4::glib;
use ksni::blocking::TrayMethods;
use libadwaita::prelude::*;

enum Event {
    Region,
    Desktop,
    Record,
    Control(&'static str),
    Language(&'static str),
    Quit,
    Finished(Result<(), String>),
    Ready(Result<ksni::blocking::Handle<Tray>, String>),
    Offline,
    Online,
}

struct Tray {
    tx: Sender<Event>,
    busy: bool,
    recording: bool,
}

impl Tray {
    fn send(&self, event: Event) {
        let _ = self.tx.try_send(event);
    }
}

impl ksni::Tray for Tray {
    fn id(&self) -> String {
        "gsnag".into()
    }
    fn title(&self) -> String {
        "gsnag".into()
    }
    fn icon_name(&self) -> String {
        "gsnag".into()
    }
    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        // Embedded fallback also works before the package icon is installed.
        let mut argb = Vec::with_capacity(32 * 32 * 4);
        for y in 0..32 {
            for x in 0..32 {
                let corner = ((8..=10).contains(&x) || (22..=24).contains(&x))
                    && ((8..=14).contains(&y) || (18..=24).contains(&y))
                    || ((8..=10).contains(&y) || (22..=24).contains(&y))
                        && ((8..=14).contains(&x) || (18..=24).contains(&x));
                argb.extend_from_slice(if corner {
                    &[255, 255, 255, 255]
                } else {
                    &[255, 53, 132, 228]
                });
            }
        }
        vec![ksni::Icon {
            width: 32,
            height: 32,
            data: argb,
        }]
    }
    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            title: "gsnag".into(),
            description: tr(if self.recording {
                "Recording controls are in the menu"
            } else if self.busy {
                "Capture or editor is open"
            } else {
                "Left-click: capture region. Right-click: menu."
            }),
            ..Default::default()
        }
    }
    fn activate(&mut self, _: i32, _: i32) {
        if !self.busy {
            self.send(Event::Region);
        }
    }
    fn watcher_offline(&self, _: ksni::OfflineReason) -> bool {
        self.send(Event::Offline);
        true // Re-register when the panel restarts.
    }
    fn watcher_online(&self) {
        self.send(Event::Online);
    }
    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::StandardItem;
        vec![
            StandardItem {
                label: tr("Capture region"),
                enabled: !self.busy,
                activate: Box::new(|t: &mut Self| t.send(Event::Region)),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: tr("Capture entire desktop"),
                enabled: !self.busy,
                activate: Box::new(|t: &mut Self| t.send(Event::Desktop)),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: tr("Record video…"),
                enabled: !self.busy,
                activate: Box::new(|t: &mut Self| t.send(Event::Record)),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: tr("Pause recording"),
                enabled: self.recording,
                activate: Box::new(|t: &mut Self| t.send(Event::Control("pause"))),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: tr("Resume recording"),
                enabled: self.recording,
                activate: Box::new(|t: &mut Self| t.send(Event::Control("resume"))),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: tr("Stop and save"),
                enabled: self.recording,
                activate: Box::new(|t: &mut Self| t.send(Event::Control("stop"))),
                ..Default::default()
            }
            .into(),
            ksni::menu::SubMenu {
                label: tr("Language"),
                submenu: std::iter::once(("auto", tr("System language")))
                    .chain(
                        gsnag_i18n::LANGUAGES
                            .iter()
                            .map(|(id, name)| (*id, (*name).to_owned())),
                    )
                    .map(|(id, name)| {
                        StandardItem {
                            label: name,
                            activate: Box::new(move |t: &mut Self| t.send(Event::Language(id))),
                            ..Default::default()
                        }
                        .into()
                    })
                    .collect(),
                ..Default::default()
            }
            .into(),
            ksni::MenuItem::Separator,
            StandardItem {
                label: tr("Quit gsnag"),
                activate: Box::new(|t: &mut Self| t.send(Event::Quit)),
                ..Default::default()
            }
            .into(),
        ]
    }
}

fn launcher(
    app: &libadwaita::Application,
    tx: &Sender<Event>,
) -> (libadwaita::ApplicationWindow, gtk4::Label) {
    let content = gtk4::Box::new(gtk4::Orientation::Vertical, 12);
    content.set_margin_top(24);
    content.set_margin_bottom(24);
    content.set_margin_start(24);
    content.set_margin_end(24);
    let status = gtk4::Label::new(None);
    status.set_wrap(true);
    content.append(&status);
    for (label, action) in [
        ("Capture region", 0),
        ("Capture entire desktop", 1),
        ("Record video…", 2),
    ] {
        let button = gtk4::Button::with_label(&tr(label));
        let tx = tx.clone();
        button.connect_clicked(move |_| {
            let _ = tx.try_send(match action {
                0 => Event::Region,
                1 => Event::Desktop,
                _ => Event::Record,
            });
        });
        content.append(&button);
    }
    let view = libadwaita::ToolbarView::new();
    view.add_top_bar(&libadwaita::HeaderBar::new());
    view.set_content(Some(&content));
    let window = libadwaita::ApplicationWindow::builder()
        .application(app)
        .title("gsnag")
        .default_width(380)
        .content(&view)
        .build();
    let tx = tx.clone();
    window.connect_close_request(move |_| {
        let _ = tx.try_send(Event::Quit);
        glib::Propagation::Proceed
    });
    (window, status)
}

pub fn run() -> Result<()> {
    let app = libadwaita::Application::builder()
        .application_id("se.gsnag.Tray")
        .build();
    let started = Rc::new(Cell::new(false));
    app.connect_activate(move |app| {
        if started.replace(true) {
            return;
        }
        let hold = app.hold();
        let (tx, rx) = async_channel::unbounded();
        let worker_tx = tx.clone();
        std::thread::spawn(move || {
            let tray = Tray {
                tx: worker_tx.clone(),
                busy: false,
                recording: false,
            };
            let result = tray
                .assume_sni_available(true)
                .spawn()
                .map_err(|e| e.to_string());
            let _ = worker_tx.send_blocking(Event::Ready(result));
        });
        let app = app.clone();
        glib::MainContext::default().spawn_local(async move {
            let _hold = hold;
            let mut handle: Option<ksni::blocking::Handle<Tray>> = None;
            let mut busy = false;
            let mut online = true;
            let (window, status) = launcher(&app, &tx);
            while let Ok(event) = rx.recv().await {
                match event {
                    Event::Ready(Ok(h)) => {
                        handle = Some(h);
                    }
                    Event::Ready(Err(error)) => {
                        online = false;
                        status.set_text(&tf("Tray could not start: {error}", &[("error", error)]));
                        window.present();
                    }
                    Event::Offline => {
                        online = false;
                        status.set_text(&tr(
                            "The system tray is unavailable. You can capture from this window.",
                        ));
                        window.present();
                    }
                    Event::Online => {
                        online = true;
                        window.set_visible(false);
                    }
                    Event::Language(code) => match gsnag_i18n::save_language(code) {
                        Ok(()) => {
                            if let Some(h) = &handle {
                                h.update(|_| {});
                            }
                        }
                        Err(e) => {
                            status.set_text(&tf("Error: {error}", &[("error", e.to_string())]));
                            window.present();
                        }
                    },
                    Event::Control(action) => {
                        if let Err(e) = super::recording::command(action) {
                            status.set_text(&tf("Error: {error}", &[("error", e.to_string())]));
                            window.present();
                        }
                    }
                    Event::Quit => break,
                    Event::Finished(result) => {
                        busy = false;
                        if let Some(h) = &handle {
                            h.update(|t| {
                                t.busy = false;
                                t.recording = false;
                            });
                        }
                        if let Err(error) = result {
                            status.set_text(&tf("Error: {error}", &[("error", error)]));
                            window.present();
                        } else if !online {
                            status.set_text(&tr(
                                "The system tray is unavailable. You can capture from this window.",
                            ));
                            window.present();
                        }
                    }
                    Event::Region | Event::Desktop | Event::Record => {
                        if busy {
                            continue;
                        }
                        busy = true;
                        let recording = matches!(event, Event::Record);
                        if let Some(h) = &handle {
                            h.update(|t| {
                                t.busy = true;
                                t.recording = recording;
                            });
                        }
                        window.set_visible(false);
                        let desktop = matches!(event, Event::Desktop);
                        let tx = tx.clone();
                        std::thread::spawn(move || {
                            // Let the panel dismiss its menu before freezing the desktop.
                            std::thread::sleep(Duration::from_millis(250));
                            let result = (|| -> Result<()> {
                                let exe = std::env::current_exe()?;
                                let args = if recording {
                                    &["record"][..]
                                } else if desktop {
                                    &["capture", "output", "--all", "--edit"][..]
                                } else {
                                    &["capture", "region", "--edit"][..]
                                };
                                let output = Command::new(exe)
                                    .env("GSNAG_LANGUAGE", gsnag_i18n::language())
                                    .args(args)
                                    .output()?;
                                anyhow::ensure!(
                                    output.status.success(),
                                    "{}",
                                    String::from_utf8_lossy(&output.stderr).trim()
                                );
                                Ok(())
                            })()
                            .map_err(|e| format!("{e:#}"));
                            let _ = tx.send_blocking(Event::Finished(result));
                        });
                    }
                }
            }
            if let Some(h) = handle {
                h.shutdown();
            }
            window.destroy();
            app.quit();
        });
    });
    let code = app.run_with_args(&["gsnag"]);
    anyhow::ensure!(
        code == glib::ExitCode::SUCCESS,
        "Tray application exited with {code:?}"
    );
    Ok(())
}
