//! Native Wayland discovery and shared-memory screen capture.

mod capture;

use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use gsnag_core::{CapturedOutput, Output};
use rustix::event::{PollFd, PollFlags, Timespec, poll};
use serde::Serialize;
use wayland_client::protocol::{
    wl_buffer, wl_callback, wl_output, wl_registry, wl_shm, wl_shm_pool,
};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum, delegate_noop};
use wayland_protocols::ext::image_capture_source::v1::client::{
    ext_image_capture_source_v1, ext_output_image_capture_source_manager_v1,
};
use wayland_protocols::ext::image_copy_capture::v1::client::ext_image_copy_capture_manager_v1;
use wayland_protocols::xdg::xdg_output::zv1::client::{zxdg_output_manager_v1, zxdg_output_v1};
use wayland_protocols_wlr::screencopy::v1::client::zwlr_screencopy_manager_v1;

#[derive(Debug, Clone, Serialize)]
pub struct Global {
    pub id: u32,
    pub interface: String,
    pub version: u32,
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub outputs: Vec<Output>,
    pub capabilities: Capabilities,
    pub globals: Vec<Global>,
}

#[derive(Debug, Serialize)]
pub struct Capabilities {
    pub ext_output_capture: bool,
    pub wlr_screencopy: bool,
    pub layer_shell: bool,
    pub xdg_output: bool,
    pub foreign_toplevel_list: bool,
    pub foreign_toplevel_capture: bool,
}

#[derive(Debug, Clone, Copy, Default)]
pub enum Backend {
    #[default]
    Auto,
    Ext,
    Wlr,
}

pub trait FrameSource {
    fn capture_output(&mut self, name: &str, cursor: bool) -> Result<CapturedOutput>;
}

pub struct WaylandCapture {
    pub backend: Backend,
}

impl FrameSource for WaylandCapture {
    fn capture_output(&mut self, name: &str, cursor: bool) -> Result<CapturedOutput> {
        // Each still owns a connection so all protocol objects are also released on errors.
        Client::connect()?.capture(name, cursor, self.backend)
    }
}

pub fn inspect() -> Result<Report> {
    Ok(Client::connect()?.report())
}

#[derive(Default)]
struct State {
    globals: Vec<Global>,
    outputs: Vec<(wl_output::WlOutput, Output)>,
    synced: bool,
    capture: capture::CaptureState,
}

struct Client {
    connection: Connection,
    queue: EventQueue<State>,
    registry: wl_registry::WlRegistry,
    state: State,
}

impl Client {
    fn connect() -> Result<Self> {
        let connection = Connection::connect_to_env().context("Cannot connect to Wayland. Run gsnag inside the labwc desktop session (WAYLAND_DISPLAY and XDG_RUNTIME_DIR must be set)")?;
        let queue = connection.new_event_queue();
        let registry = connection.display().get_registry(&queue.handle(), ());
        let mut client = Self {
            connection,
            queue,
            registry,
            state: State::default(),
        };
        client.sync()?;
        client.sync()?;
        if let Some(global) = client.global("zxdg_output_manager_v1") {
            let qh = client.queue.handle();
            // Version 2 uses its own done event and works with old wl_output versions.
            let manager: zxdg_output_manager_v1::ZxdgOutputManagerV1 =
                client
                    .registry
                    .bind(global.id, global.version.min(2), &qh, ());
            for (proxy, output) in &client.state.outputs {
                manager.get_xdg_output(proxy, &qh, output.id);
            }
            client.sync()?;
            manager.destroy();
        }
        for (_, o) in &mut client.state.outputs {
            if o.logical_width == 0 || o.logical_height == 0 {
                let (w, h) = if o.transform % 2 == 1 {
                    (o.pixel_height, o.pixel_width)
                } else {
                    (o.pixel_width, o.pixel_height)
                };
                o.logical_width = w / o.scale.max(1) as u32;
                o.logical_height = h / o.scale.max(1) as u32;
            }
        }
        Ok(client)
    }

    fn global(&self, interface: &str) -> Option<&Global> {
        self.state.globals.iter().find(|g| g.interface == interface)
    }

    fn report(&self) -> Report {
        let has = |name| self.global(name).is_some();
        let ext = has("ext_image_copy_capture_manager_v1");
        Report {
            outputs: self.state.outputs.iter().map(|(_, o)| o.clone()).collect(),
            capabilities: Capabilities {
                ext_output_capture: ext
                    && has("ext_output_image_capture_source_manager_v1")
                    && has("wl_shm"),
                wlr_screencopy: has("zwlr_screencopy_manager_v1") && has("wl_shm"),
                layer_shell: has("zwlr_layer_shell_v1"),
                xdg_output: has("zxdg_output_manager_v1"),
                foreign_toplevel_list: has("ext_foreign_toplevel_list_v1"),
                foreign_toplevel_capture: ext
                    && has("ext_foreign_toplevel_list_v1")
                    && has("ext_foreign_toplevel_image_capture_source_manager_v1"),
            },
            globals: self.state.globals.clone(),
        }
    }

    fn sync(&mut self) -> Result<()> {
        self.state.synced = false;
        self.connection.display().sync(&self.queue.handle(), ());
        self.wait_until(|s| s.synced)
    }

    fn wait_until(&mut self, done: impl Fn(&State) -> bool) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            self.queue.dispatch_pending(&mut self.state)?;
            if let Some(error) = &self.state.capture.error {
                bail!("{error}");
            }
            if done(&self.state) {
                return Ok(());
            }
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .context("Wayland operation timed out after 10 seconds")?;
            self.queue.flush()?;
            if let Some(guard) = self.queue.prepare_read() {
                let mut fds = [PollFd::new(&self.connection, PollFlags::IN)];
                let timeout = Timespec::try_from(remaining)?;
                match poll(&mut fds, Some(&timeout)) {
                    Ok(0) => bail!("Wayland operation timed out after 10 seconds"),
                    Ok(_) => {
                        guard.read()?;
                    }
                    Err(rustix::io::Errno::INTR) => continue,
                    Err(error) => return Err(error.into()),
                }
            }
        }
    }
}

impl Dispatch<wl_registry::WlRegistry, ()> for State {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            wl_registry::Event::Global {
                name,
                interface,
                version,
            } => {
                if interface == "wl_output" {
                    let proxy = registry.bind(name, version.min(4), qh, name);
                    state.outputs.push((
                        proxy,
                        Output {
                            id: name,
                            name: format!("output-{name}"),
                            scale: 1,
                            ..Output::default()
                        },
                    ));
                }
                state.globals.push(Global {
                    id: name,
                    interface,
                    version,
                });
            }
            wl_registry::Event::GlobalRemove { name } => {
                state.globals.retain(|g| g.id != name);
                state.outputs.retain(|(_, o)| o.id != name);
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_output::WlOutput, u32> for State {
    fn event(
        state: &mut Self,
        _: &wl_output::WlOutput,
        event: wl_output::Event,
        id: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some((_, o)) = state.outputs.iter_mut().find(|(_, o)| o.id == *id) else {
            return;
        };
        match event {
            wl_output::Event::Name { name } => o.name = name,
            wl_output::Event::Description { description } => o.description = description,
            wl_output::Event::Geometry {
                x, y, transform, ..
            } => {
                o.x = x;
                o.y = y;
                o.transform = match transform {
                    WEnum::Value(t) => t as u32,
                    WEnum::Unknown(t) => t,
                };
            }
            wl_output::Event::Mode {
                flags: WEnum::Value(flags),
                width,
                height,
                ..
            } if flags.contains(wl_output::Mode::Current) => {
                o.pixel_width = width.max(0) as u32;
                o.pixel_height = height.max(0) as u32;
            }
            wl_output::Event::Scale { factor } => o.scale = factor.max(1),
            _ => {}
        }
    }
}

impl Dispatch<zxdg_output_v1::ZxdgOutputV1, u32> for State {
    fn event(
        state: &mut Self,
        _: &zxdg_output_v1::ZxdgOutputV1,
        event: zxdg_output_v1::Event,
        id: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some((_, o)) = state.outputs.iter_mut().find(|(_, o)| o.id == *id) else {
            return;
        };
        match event {
            zxdg_output_v1::Event::Name { name } => o.name = name,
            zxdg_output_v1::Event::Description { description } => o.description = description,
            zxdg_output_v1::Event::LogicalPosition { x, y } => {
                o.x = x;
                o.y = y;
            }
            zxdg_output_v1::Event::LogicalSize { width, height } => {
                o.logical_width = width.max(0) as u32;
                o.logical_height = height.max(0) as u32;
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_callback::WlCallback, ()> for State {
    fn event(
        state: &mut Self,
        _: &wl_callback::WlCallback,
        _: wl_callback::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        state.synced = true;
    }
}

delegate_noop!(State: ignore wl_shm::WlShm);
delegate_noop!(State: ignore wl_shm_pool::WlShmPool);
delegate_noop!(State: ignore wl_buffer::WlBuffer);
delegate_noop!(State: ignore zxdg_output_manager_v1::ZxdgOutputManagerV1);
delegate_noop!(State: ignore ext_output_image_capture_source_manager_v1::ExtOutputImageCaptureSourceManagerV1);
delegate_noop!(State: ignore ext_image_capture_source_v1::ExtImageCaptureSourceV1);
delegate_noop!(State: ignore ext_image_copy_capture_manager_v1::ExtImageCopyCaptureManagerV1);
delegate_noop!(State: ignore zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1);
