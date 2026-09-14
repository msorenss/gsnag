//! Native Wayland recording, software H.264/AAC encoding and PipeWire audio.
mod audio;
mod encoder;
use anyhow::{Context, Result, ensure};
use gsnag_core::{Output, Rect};
use gsnag_proto::{Backend, OutputStream};
use image::{RgbaImage, imageops};
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioMode {
    None,
    System,
    Microphone,
    Both,
}
#[derive(Clone)]
pub struct Options {
    pub output: PathBuf,
    pub outputs: Vec<Output>,
    pub region: Option<Rect>,
    pub fps: u32,
    pub max_width: u32,
    pub cursor: bool,
    pub backend: Backend,
    pub audio: AudioMode,
    pub microphone: Option<String>,
    pub system: Option<String>,
    pub duration: Option<Duration>,
    pub overwrite: bool,
}
#[derive(Default, Clone, Debug)]
pub struct Progress {
    pub seconds: f64,
    pub frames: u64,
    pub repeated: u64,
    pub width: u32,
    pub height: u32,
}
#[derive(Default)]
struct Clock {
    start: Option<Instant>,
    pause_start: Option<Instant>,
    paused: Duration,
}
#[derive(Default)]
pub struct Control {
    stop: AtomicBool,
    clock: Mutex<Clock>,
    progress: Mutex<Progress>,
}
impl Control {
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
    pub fn started(&self) -> bool {
        self.clock.lock().unwrap().start.is_some()
    }
    pub fn stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }
    pub fn paused(&self) -> bool {
        self.clock.lock().unwrap().pause_start.is_some()
    }
    pub fn pause(&self, paused: bool) {
        let mut c = self.clock.lock().unwrap();
        if paused && c.pause_start.is_none() {
            c.pause_start = Some(Instant::now());
        } else if !paused && let Some(start) = c.pause_start.take() {
            c.paused += start.elapsed();
        }
    }
    pub fn elapsed(&self) -> Duration {
        let c = self.clock.lock().unwrap();
        c.start
            .map(|start| {
                c.pause_start
                    .unwrap_or_else(Instant::now)
                    .saturating_duration_since(start)
                    .saturating_sub(c.paused)
            })
            .unwrap_or_default()
    }
    pub fn progress(&self) -> Progress {
        self.progress.lock().unwrap().clone()
    }
    fn start(&self) {
        *self.clock.lock().unwrap() = Clock {
            start: Some(Instant::now()),
            ..Default::default()
        };
    }
}
#[derive(Debug)]
pub struct Outcome {
    pub path: PathBuf,
    pub progress: Progress,
    pub warning: Option<String>,
}

pub fn dimensions(width: u32, height: u32, max_width: u32) -> Result<(u32, u32)> {
    ensure!(
        width >= 2 && height >= 2,
        "Recording region must be at least 2×2 pixels"
    );
    ensure!(
        (320..=3840).contains(&max_width),
        "Maximum video width must be between 320 and 3840"
    );
    let scale = (max_width as f64 / width as f64)
        .min(1.0)
        .min(2160.0 / height as f64);
    Ok((
        ((width as f64 * scale) as u32 / 2 * 2).max(2),
        ((height as f64 * scale) as u32 / 2 * 2).max(2),
    ))
}

struct CaptureWorker {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Drop for CaptureWorker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
fn capture(
    options: Options,
    control: Arc<Control>,
    tx: mpsc::SyncSender<Result<(RgbaImage, Duration)>>,
    stop: Arc<AtomicBool>,
) {
    let result = (|| -> Result<()> {
        let mut streams: Vec<_> = options
            .outputs
            .iter()
            .map(|o| OutputStream::connect(&o.name, options.cursor, options.backend))
            .collect::<Result<_>>()?;
        let bounds = gsnag_core::desktop_bounds(&options.outputs)?;
        let mut last_time = Duration::ZERO;
        while !stop.load(Ordering::Relaxed) && !control.stopped() {
            if control.paused() {
                std::thread::sleep(Duration::from_millis(30));
                continue;
            }
            let frame_started = Instant::now();
            let mut frames = Vec::new();
            for stream in &mut streams {
                let (frame, stamp) = stream.next_frame(&stop)?;
                frames.push(frame);
                last_time = last_time.max(stamp);
            }
            let mut image = if frames.len() == 1 && options.region.is_none() {
                frames.pop().unwrap().image
            } else {
                gsnag_core::compose(&frames)?
            };
            if let Some(region) = options.region {
                image = gsnag_core::crop_region(&image, bounds, region)?;
            }
            match tx.try_send(Ok((image, last_time))) {
                Ok(()) | Err(mpsc::TrySendError::Full(_)) => (),
                Err(mpsc::TrySendError::Disconnected(_)) => break,
            }
            std::thread::sleep(
                Duration::from_secs_f64(1.0 / options.fps as f64)
                    .saturating_sub(frame_started.elapsed()),
            );
        }
        Ok(())
    })();
    if let Err(e) = result {
        let _ = tx.try_send(Err(e));
    }
}

pub fn record(options: Options, control: Arc<Control>) -> Result<Outcome> {
    ensure!(
        (1..=60).contains(&options.fps),
        "FPS must be between 1 and 60"
    );
    ensure!(!options.outputs.is_empty(), "No display selected");
    if let Some(duration) = options.duration {
        ensure!(
            !duration.is_zero() && duration <= Duration::from_secs(86_400),
            "Duration must be between 0 and 86400 seconds"
        );
    }
    let container = match options
        .output
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("mp4") => "mp4",
        Some("mkv") => "matroska",
        _ => anyhow::bail!("Recording destination must end in .mp4 or .mkv"),
    };
    ensure!(
        options.overwrite || !options.output.exists(),
        "Destination exists; use --overwrite to replace it"
    );
    let parent = options
        .output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."));
    let temp = tempfile::Builder::new()
        .prefix(".gsnag-record-")
        .tempfile_in(parent)?;
    let (tx, rx) = mpsc::sync_channel(2);
    let stop = Arc::new(AtomicBool::new(false));
    let worker_stop = stop.clone();
    let worker_options = options.clone();
    let worker_control = control.clone();
    let worker = CaptureWorker {
        stop,
        thread: Some(std::thread::spawn(move || {
            capture(worker_options, worker_control, tx, worker_stop)
        })),
    };
    let (mut latest, mut stamp) = rx
        .recv_timeout(Duration::from_secs(12))
        .context("Timed out waiting for first recording frame")??;
    let size = dimensions(latest.width(), latest.height(), options.max_width)?;
    let mut enc = encoder::Encoder::new(
        temp.path(),
        container,
        size.0,
        size.1,
        options.fps,
        options.audio != AudioMode::None,
    )?;
    let mut audio = Vec::new();
    if matches!(options.audio, AudioMode::System | AudioMode::Both) {
        audio.push(audio::AudioCapture::start(
            true,
            options.system.clone(),
            control.clone(),
        ));
    }
    if matches!(options.audio, AudioMode::Microphone | AudioMode::Both) {
        audio.push(audio::AudioCapture::start(
            false,
            options.microphone.clone(),
            control.clone(),
        ));
    }
    // Negotiate devices before starting the timeline, avoiding silent startup gaps.
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        ensure!(
            !control.stopped(),
            "Recording cancelled before the first frame"
        );
        let mut ready = true;
        for source in &audio {
            let buffer = source.samples.lock().unwrap();
            if let Some(error) = &buffer.error {
                anyhow::bail!("{error}");
            }
            ready &= buffer.received;
        }
        while let Ok(frame) = rx.try_recv() {
            let (image, t) = frame?;
            latest = image;
            stamp = t;
        }
        if ready {
            break;
        }
        ensure!(
            Instant::now() < deadline,
            "Requested audio source produced no samples; check the default PipeWire source/sink"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    control.start();
    let mut progress = Progress {
        width: size.0,
        height: size.1,
        ..Default::default()
    };
    let mut audio_pts = 0;
    let mut warning = None;
    let mut prev_stamp = None;
    while !control.stopped() {
        if control.paused() {
            std::thread::sleep(Duration::from_millis(20));
            continue;
        }
        let elapsed = control.elapsed();
        if options.duration.is_some_and(|duration| elapsed >= duration) {
            break;
        }
        if progress.frames as f64 / options.fps as f64 > elapsed.as_secs_f64() {
            std::thread::sleep(Duration::from_millis(2));
            continue;
        }
        loop {
            let frame = match rx.try_recv() {
                Ok(frame) => frame,
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    warning = Some("Screen capture ended unexpectedly".into());
                    control.stop();
                    break;
                }
            };
            match frame {
                Ok((image, t)) => {
                    latest = image;
                    stamp = t;
                }
                Err(e) => {
                    warning = Some(format!("{e:#}"));
                    control.stop();
                }
            }
        }
        if control.stopped() {
            break;
        }
        ensure!(
            dimensions(latest.width(), latest.height(), options.max_width)? == size,
            "Recording dimensions changed"
        );
        if prev_stamp == Some(stamp) {
            progress.repeated += 1;
        }
        prev_stamp = Some(stamp);
        let image = if latest.dimensions() == size {
            std::borrow::Cow::Borrowed(&latest)
        } else {
            std::borrow::Cow::Owned(imageops::resize(
                &latest,
                size.0,
                size.1,
                imageops::FilterType::Triangle,
            ))
        };
        enc.video(&image, progress.frames as i64)?;
        progress.frames += 1;
        progress.seconds = progress.frames as f64 / options.fps as f64;
        let end = ((progress.seconds - 1.0 / options.fps as f64 - 0.15).max(0.0) * 48_000.0) as u64;
        while !audio.is_empty() && audio_pts + enc.audio_frame_size as u64 <= end {
            let mut samples = vec![[0.0; 2]; enc.audio_frame_size];
            for source in &audio {
                let mut buffer = source.samples.lock().unwrap();
                if let Some(e) = &buffer.error {
                    warning = Some(e.clone());
                    control.stop();
                }
                if progress.seconds > 1.0
                    && progress.seconds * 48_000.0 > buffer.last_end as f64 + 48_000.0
                {
                    warning=Some("Requested audio source produced no samples; check the default PipeWire source/sink".into());
                    control.stop();
                }
                buffer.mix(audio_pts, &mut samples, 1.0 / audio.len() as f32);
            }
            enc.audio(&samples, audio_pts as i64)?;
            audio_pts += samples.len() as u64;
        }
        *control.progress.lock().unwrap() = progress.clone();
        // Stop and finalize a usable clip if constant-rate encoding falls behind.
        let slot = (control.elapsed().as_secs_f64() * options.fps as f64) as u64;
        if slot > progress.frames + options.fps as u64 {
            warning = Some("Encoding cannot keep up; lower resolution or FPS".into());
            control.stop();
        }
    }
    if !audio.is_empty() {
        std::thread::sleep(Duration::from_millis(100));
        let end = (progress.seconds * 48_000.0) as u64;
        while audio_pts < end {
            let mut samples = vec![[0.0; 2]; enc.audio_frame_size];
            let valid = (end - audio_pts).min(samples.len() as u64) as usize;
            for source in &audio {
                source.samples.lock().unwrap().mix(
                    audio_pts,
                    &mut samples[..valid],
                    1.0 / audio.len() as f32,
                );
            }
            enc.audio(&samples, audio_pts as i64)?;
            audio_pts += samples.len() as u64;
        }
    }
    drop(audio);
    drop(rx);
    drop(worker);
    ensure!(
        progress.frames > 0,
        "Recording cancelled before the first frame"
    );
    enc.finish()?;
    temp.as_file().sync_all()?;
    if options.overwrite {
        temp.persist(&options.output).map_err(|e| e.error)?;
    } else {
        temp.persist_noclobber(&options.output)
            .map_err(|e| e.error)?;
    }
    Ok(Outcome {
        path: options.output,
        progress,
        warning,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dimensions_are_even_and_bounded() {
        assert_eq!(dimensions(1921, 1081, 1920).unwrap(), (1920, 1080));
        assert_eq!(dimensions(3840, 2160, 1920).unwrap(), (1920, 1080));
        assert!(dimensions(1, 30, 1920).is_err());
    }
    #[test]
    fn clock_excludes_pause() {
        let c = Control::default();
        c.start();
        c.pause(true);
        let before = c.elapsed();
        std::thread::sleep(Duration::from_millis(10));
        assert_eq!(c.elapsed(), before);
        c.pause(false);
        assert!(!c.paused());
    }
    #[test]
    fn encodes_a_playable_test_clip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.mp4");
        let mut enc = encoder::Encoder::new(&path, "mp4", 64, 48, 10, true).unwrap();
        let image = RgbaImage::from_pixel(64, 48, image::Rgba([200, 30, 40, 255]));
        let mut audio = 0;
        for n in 0..10 {
            enc.video(&image, n).unwrap();
            while audio + 1024 <= (n + 1) * 4800 {
                enc.audio(&vec![[0.0; 2]; 1024], audio).unwrap();
                audio += 1024;
            }
        }
        enc.finish().unwrap();
        assert!(std::fs::metadata(path).unwrap().len() > 1000);
    }
}
