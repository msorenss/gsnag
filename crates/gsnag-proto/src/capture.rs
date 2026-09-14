use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    os::fd::AsFd,
};

use image::{Rgba, RgbaImage, imageops};
use rustix::fs::{MemfdFlags, memfd_create};
use wayland_protocols::ext::image_copy_capture::v1::client::{
    ext_image_copy_capture_frame_v1 as ext_frame, ext_image_copy_capture_session_v1 as ext_session,
};
use wayland_protocols_wlr::screencopy::v1::client::zwlr_screencopy_frame_v1 as wlr_frame;

use super::*;

#[derive(Default)]
pub(super) struct CaptureState {
    width: u32,
    height: u32,
    stride: u32,
    format: Option<wl_shm::Format>,
    constraints_done: bool,
    ready: bool,
    y_invert: bool,
    transform: u32,
    pub error: Option<String>,
    presented: Option<Duration>,
}

impl Client {
    pub(super) fn capture(
        &mut self,
        name: &str,
        cursor: bool,
        backend: Backend,
    ) -> Result<CapturedOutput> {
        self.state.capture = CaptureState::default();
        let (output_proxy, output) = self
            .state
            .outputs
            .iter()
            .find(|(_, o)| o.name == name)
            .cloned()
            .with_context(|| format!("Output {name:?} was not found; run gsnag outputs"))?;
        let caps = self.report().capabilities;
        let use_ext = match backend {
            Backend::Auto => caps.ext_output_capture,
            Backend::Ext => {
                ensure!(
                    caps.ext_output_capture,
                    "ext-image-copy-capture output capture is unavailable"
                );
                true
            }
            Backend::Wlr => false,
        };
        ensure!(
            use_ext || caps.wlr_screencopy,
            "No supported native output capture protocol (ext-image-copy-capture or wlr-screencopy)"
        );
        let qh = self.queue.handle();
        let shm_global = self.global("wl_shm").context("wl_shm is unavailable")?;
        let shm: wl_shm::WlShm = self.registry.bind(shm_global.id, 1, &qh, ());

        let (ext, wlr) = if use_ext {
            let g = self
                .global("ext_output_image_capture_source_manager_v1")
                .unwrap();
            let source_manager: ext_output_image_capture_source_manager_v1::ExtOutputImageCaptureSourceManagerV1 = self.registry.bind(g.id, 1, &qh, ());
            let source = source_manager.create_source(&output_proxy, &qh, ());
            let g = self.global("ext_image_copy_capture_manager_v1").unwrap();
            let manager: ext_image_copy_capture_manager_v1::ExtImageCopyCaptureManagerV1 =
                self.registry.bind(g.id, 1, &qh, ());
            let options = if cursor {
                ext_image_copy_capture_manager_v1::Options::PaintCursors
            } else {
                ext_image_copy_capture_manager_v1::Options::empty()
            };
            let session = manager.create_session(&source, options, &qh, ());
            source.destroy();
            source_manager.destroy();
            manager.destroy();
            (Some(session), None)
        } else {
            let g = self.global("zwlr_screencopy_manager_v1").unwrap();
            let manager: zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1 =
                self.registry.bind(g.id, g.version.min(3), &qh, ());
            let frame = manager.capture_output(i32::from(cursor), &output_proxy, &qh, ());
            manager.destroy();
            (None, Some(frame))
        };
        self.wait_until(|s| s.capture.constraints_done)?;
        let c = &self.state.capture;
        gsnag_core::validate_image_size(c.width, c.height)?;
        let format = c
            .format
            .context("Compositor did not offer a supported 32-bit wl_shm format")?;
        let stride = if use_ext {
            c.width.checked_mul(4).context("Stride overflow")?
        } else {
            c.stride
        };
        ensure!(
            u64::from(stride) >= u64::from(c.width) * 4,
            "Invalid buffer stride"
        );
        let size = u64::from(stride) * u64::from(c.height);
        ensure!(size <= 400_000_000, "Capture buffer exceeds 400 MB");
        let mut file = File::from(memfd_create(c"gsnag-frame", MemfdFlags::CLOEXEC)?);
        file.set_len(size)?;
        let pool = shm.create_pool(file.as_fd(), i32::try_from(size)?, &qh, ());
        let buffer = pool.create_buffer(
            0,
            i32::try_from(c.width)?,
            i32::try_from(c.height)?,
            i32::try_from(stride)?,
            format,
            &qh,
            (),
        );
        pool.destroy();
        let ext_frame = ext.as_ref().map(|session| {
            let frame = session.create_frame(&qh, ());
            frame.attach_buffer(&buffer);
            frame.damage_buffer(0, 0, c.width as i32, c.height as i32);
            frame.capture();
            frame
        });
        if let Some(frame) = &wlr {
            frame.copy(&buffer);
        }
        self.wait_until(|s| s.capture.ready)?;
        let mut bytes = vec![0; size as usize];
        file.read_exact(&mut bytes)?;
        let c = &self.state.capture;
        let mut image = decode(&bytes, c.width, c.height, stride, format, c.y_invert)?;
        // Undo the buffer transform; legacy screencopy uses wl_output metadata.
        image = orient(
            image,
            if use_ext {
                c.transform
            } else {
                output.transform
            },
        )?;
        if let Some(frame) = ext_frame {
            frame.destroy();
        }
        if let Some(session) = ext {
            session.destroy();
        }
        if let Some(frame) = wlr {
            frame.destroy();
        }
        buffer.destroy();
        self.queue.flush()?;
        Ok(CapturedOutput { output, image })
    }
}

fn supported(format: wl_shm::Format) -> bool {
    matches!(
        format,
        wl_shm::Format::Argb8888
            | wl_shm::Format::Xrgb8888
            | wl_shm::Format::Abgr8888
            | wl_shm::Format::Xbgr8888
    )
}

fn decode(
    bytes: &[u8],
    width: u32,
    height: u32,
    stride: u32,
    format: wl_shm::Format,
    inverted: bool,
) -> Result<RgbaImage> {
    ensure!(supported(format), "Unsupported pixel format: {format:?}");
    ensure!(
        u64::from(stride) >= u64::from(width) * 4
            && bytes.len() as u64 >= u64::from(stride) * u64::from(height),
        "Truncated capture buffer"
    );
    gsnag_core::validate_image_size(width, height)?;
    let alpha = matches!(format, wl_shm::Format::Argb8888 | wl_shm::Format::Abgr8888);
    let bgr = matches!(format, wl_shm::Format::Abgr8888 | wl_shm::Format::Xbgr8888);
    Ok(RgbaImage::from_fn(width, height, |x, y| {
        let y = if inverted { height - 1 - y } else { y };
        let offset = y as usize * stride as usize + x as usize * 4;
        let pixel = u32::from_ne_bytes(bytes[offset..offset + 4].try_into().unwrap());
        let a = if alpha { (pixel >> 24) as u8 } else { 255 };
        let (r, g, b) = if bgr {
            (pixel as u8, (pixel >> 8) as u8, (pixel >> 16) as u8)
        } else {
            ((pixel >> 16) as u8, (pixel >> 8) as u8, pixel as u8)
        };
        let straight = |v: u8| {
            if a == 0 {
                0
            } else {
                (u32::from(v) * 255 / u32::from(a)).min(255) as u8
            }
        };
        Rgba([straight(r), straight(g), straight(b), a])
    }))
}

fn orient(image: RgbaImage, transform: u32) -> Result<RgbaImage> {
    // Invert the compositor's counter-clockwise transform to recover display orientation.
    Ok(match transform {
        0 => image,
        1 => imageops::rotate90(&image),
        2 => imageops::rotate180(&image),
        3 => imageops::rotate270(&image),
        4 => imageops::flip_horizontal(&image),
        5 => imageops::rotate270(&imageops::flip_horizontal(&image)),
        6 => imageops::rotate180(&imageops::flip_horizontal(&image)),
        7 => imageops::rotate90(&imageops::flip_horizontal(&image)),
        _ => bail!("Unknown Wayland buffer transform: {transform}"),
    })
}

impl Dispatch<ext_session::ExtImageCopyCaptureSessionV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &ext_session::ExtImageCopyCaptureSessionV1,
        event: ext_session::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let c = &mut state.capture;
        match event {
            ext_session::Event::BufferSize { width, height } => {
                c.width = width;
                c.height = height;
            }
            ext_session::Event::ShmFormat {
                format: WEnum::Value(format),
            } if supported(format) => c.format = Some(format),
            ext_session::Event::Done => c.constraints_done = true,
            ext_session::Event::Stopped => c.error = Some("Capture session stopped".into()),
            _ => {}
        }
    }
}

impl Dispatch<ext_frame::ExtImageCopyCaptureFrameV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &ext_frame::ExtImageCopyCaptureFrameV1,
        event: ext_frame::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            ext_frame::Event::Ready => state.capture.ready = true,
            ext_frame::Event::PresentationTime {
                tv_sec_hi,
                tv_sec_lo,
                tv_nsec,
            } => {
                state.capture.presented = Some(Duration::new(
                    (u64::from(tv_sec_hi) << 32) | u64::from(tv_sec_lo),
                    tv_nsec.min(999_999_999),
                ));
            }
            ext_frame::Event::Failed { reason } => {
                state.capture.error = Some(format!("Frame capture failed: {reason:?}"))
            }
            ext_frame::Event::Transform { transform } => {
                state.capture.transform = match transform {
                    WEnum::Value(t) => t as u32,
                    WEnum::Unknown(t) => t,
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<wlr_frame::ZwlrScreencopyFrameV1, ()> for State {
    fn event(
        state: &mut Self,
        frame: &wlr_frame::ZwlrScreencopyFrameV1,
        event: wlr_frame::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let c = &mut state.capture;
        match event {
            wlr_frame::Event::Buffer {
                format,
                width,
                height,
                stride,
            } => {
                c.width = width;
                c.height = height;
                c.stride = stride;
                if let WEnum::Value(format) = format {
                    c.format = supported(format).then_some(format);
                }
                if frame.version() < 3 {
                    c.constraints_done = true;
                }
            }
            wlr_frame::Event::BufferDone => c.constraints_done = true,
            wlr_frame::Event::Flags {
                flags: WEnum::Value(flags),
            } => c.y_invert = flags.contains(wlr_frame::Flags::YInvert),
            wlr_frame::Event::Ready {
                tv_sec_hi,
                tv_sec_lo,
                tv_nsec,
            } => {
                c.ready = true;
                c.presented = Some(Duration::new(
                    (u64::from(tv_sec_hi) << 32) | u64::from(tv_sec_lo),
                    tv_nsec.min(999_999_999),
                ));
            }
            wlr_frame::Event::Failed => {
                c.error = Some("Compositor refused or failed screencopy".into())
            }
            _ => {}
        }
    }
}

/// A continuous native capture session with a reusable wl_shm buffer.
/// Construct and use it on the capture worker thread.
pub struct OutputStream {
    client: Client,
    output: Output,
    proxy: wl_output::WlOutput,
    ext: Option<ext_session::ExtImageCopyCaptureSessionV1>,
    wlr: Option<zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1>,
    shm: wl_shm::WlShm,
    buffer: Option<wl_buffer::WlBuffer>,
    file: Option<File>,
    bytes: Vec<u8>,
    layout: Option<(u32, u32, u32, wl_shm::Format)>,
    cursor: bool,
}
impl OutputStream {
    pub fn connect(name: &str, cursor: bool, backend: Backend) -> Result<Self> {
        let mut client = Client::connect()?;
        let (proxy, output) = client
            .state
            .outputs
            .iter()
            .find(|(_, o)| o.name == name)
            .cloned()
            .context("Recording output disappeared")?;
        let caps = client.report().capabilities;
        let use_ext = match backend {
            Backend::Auto => caps.ext_output_capture,
            Backend::Ext => true,
            Backend::Wlr => false,
        };
        ensure!(
            if use_ext {
                caps.ext_output_capture
            } else {
                caps.wlr_screencopy
            },
            "Native recording protocol unavailable"
        );
        let qh = client.queue.handle();
        let g = client
            .global("wl_shm")
            .context("No shared memory support")?;
        let shm = client.registry.bind(g.id, 1, &qh, ());
        let (ext, wlr) = if use_ext {
            let g = client
                .global("ext_output_image_capture_source_manager_v1")
                .unwrap();
            let manager: ext_output_image_capture_source_manager_v1::ExtOutputImageCaptureSourceManagerV1 = client.registry.bind(g.id, 1, &qh, ());
            let source = manager.create_source(&proxy, &qh, ());
            let g = client.global("ext_image_copy_capture_manager_v1").unwrap();
            let capture: ext_image_copy_capture_manager_v1::ExtImageCopyCaptureManagerV1 =
                client.registry.bind(g.id, 1, &qh, ());
            let flags = if cursor {
                ext_image_copy_capture_manager_v1::Options::PaintCursors
            } else {
                ext_image_copy_capture_manager_v1::Options::empty()
            };
            let session = capture.create_session(&source, flags, &qh, ());
            capture.destroy();
            source.destroy();
            manager.destroy();
            client.wait_until(|s| s.capture.constraints_done)?;
            (Some(session), None)
        } else {
            let g = client.global("zwlr_screencopy_manager_v1").unwrap();
            (
                None,
                Some(client.registry.bind(g.id, g.version.min(3), &qh, ())),
            )
        };
        Ok(Self {
            client,
            output,
            proxy,
            ext,
            wlr,
            shm,
            buffer: None,
            file: None,
            bytes: Vec::new(),
            layout: None,
            cursor,
        })
    }
    pub fn next_frame(
        &mut self,
        stop: &std::sync::atomic::AtomicBool,
    ) -> Result<(CapturedOutput, Duration)> {
        let qh = self.client.queue.handle();
        let c = &mut self.client.state.capture;
        c.ready = false;
        c.presented = None;
        let legacy = self.wlr.as_ref().map(|manager| {
            c.constraints_done = false;
            manager.capture_output(i32::from(self.cursor), &self.proxy, &qh, ())
        });
        self.client
            .wait_until_cancel(|s| s.capture.constraints_done, stop)?;
        let c = &self.client.state.capture;
        let stride = if self.ext.is_some() {
            c.width.checked_mul(4).context("Stride overflow")?
        } else {
            c.stride
        };
        let format = c.format.context("No usable capture pixel format")?;
        let layout = (c.width, c.height, stride, format);
        if let Some(expected) = self.layout {
            ensure!(
                expected == layout,
                "Output resolution changed during recording"
            );
        }
        if self.buffer.is_none() {
            gsnag_core::validate_image_size(c.width, c.height)?;
            let size = u64::from(stride) * u64::from(c.height);
            ensure!(
                size <= 400_000_000 && u64::from(stride) >= u64::from(c.width) * 4,
                "Invalid recording buffer size"
            );
            let file = File::from(memfd_create(c"gsnag-record", MemfdFlags::CLOEXEC)?);
            file.set_len(size)?;
            let pool = self
                .shm
                .create_pool(file.as_fd(), i32::try_from(size)?, &qh, ());
            self.buffer = Some(pool.create_buffer(
                0,
                c.width as i32,
                c.height as i32,
                stride as i32,
                format,
                &qh,
                (),
            ));
            pool.destroy();
            self.file = Some(file);
            self.bytes.resize(size as usize, 0);
            self.layout = Some(layout);
        }
        let buffer = self.buffer.as_ref().unwrap();
        let frame = self.ext.as_ref().map(|session| {
            let frame = session.create_frame(&qh, ());
            frame.attach_buffer(buffer);
            frame.damage_buffer(0, 0, c.width as i32, c.height as i32);
            frame.capture();
            frame
        });
        if let Some(frame) = &legacy {
            frame.copy(buffer);
        }
        self.client.wait_until_cancel(|s| s.capture.ready, stop)?;
        let file = self.file.as_mut().unwrap();
        file.seek(SeekFrom::Start(0))?;
        file.read_exact(&mut self.bytes)?;
        let c = &self.client.state.capture;
        let image = orient(
            decode(&self.bytes, c.width, c.height, stride, format, c.y_invert)?,
            if self.ext.is_some() {
                c.transform
            } else {
                self.output.transform
            },
        )?;
        let timestamp = c
            .presented
            .context("Missing compositor presentation timestamp")?;
        if let Some(f) = frame {
            f.destroy();
        }
        if let Some(f) = legacy {
            f.destroy();
        }
        self.client.queue.flush()?;
        ensure!(
            self.client
                .state
                .outputs
                .iter()
                .any(|(_, o)| o == &self.output),
            "Output layout changed during recording"
        );
        Ok((
            CapturedOutput {
                output: self.output.clone(),
                image,
            },
            timestamp,
        ))
    }
}
impl Drop for OutputStream {
    fn drop(&mut self) {
        if let Some(b) = &self.buffer {
            b.destroy();
        }
        if let Some(s) = &self.ext {
            s.destroy();
        }
        if let Some(m) = &self.wlr {
            m.destroy();
        }
        let _ = self.client.queue.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_native_endian_padding_and_y_inversion() {
        let data: Vec<u8> = [0x00ff0000_u32, 0, 0x000000ff, 0]
            .into_iter()
            .flat_map(u32::to_ne_bytes)
            .collect();
        let image = decode(&data, 1, 2, 8, wl_shm::Format::Xrgb8888, true).unwrap();
        assert_eq!(image.get_pixel(0, 0).0, [0, 0, 255, 255]);
        assert_eq!(image.get_pixel(0, 1).0, [255, 0, 0, 255]);
        assert!(decode(&data[..4], 1, 2, 8, wl_shm::Format::Xrgb8888, false).is_err());
    }

    #[test]
    fn unpremultiplies_alpha_and_handles_abgr() {
        let data = 0x80004080_u32.to_ne_bytes();
        let image = decode(&data, 1, 1, 4, wl_shm::Format::Abgr8888, false).unwrap();
        assert_eq!(image.get_pixel(0, 0).0, [255, 127, 0, 128]);
    }

    #[test]
    fn all_transforms_preserve_pixels_and_expected_dimensions() {
        let image = RgbaImage::from_fn(3, 2, |x, y| Rgba([(y * 3 + x) as u8, 0, 0, 255]));
        for t in 0..8 {
            let transformed = orient(image.clone(), t).unwrap();
            assert_eq!(
                transformed.dimensions(),
                if t % 2 == 0 { (3, 2) } else { (2, 3) }
            );
            let mut pixels: Vec<_> = transformed.pixels().map(|p| p[0]).collect();
            pixels.sort();
            assert_eq!(pixels, [0, 1, 2, 3, 4, 5]);
        }
        assert_eq!(
            orient(image, 1)
                .unwrap()
                .pixels()
                .map(|p| p[0])
                .collect::<Vec<_>>(),
            [3, 0, 4, 1, 5, 2]
        );
    }
}
