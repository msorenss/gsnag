use anyhow::{Context, Result, ensure};
use clap::{Args, Subcommand, ValueEnum};
use gsnag_proto::{Backend, FrameSource, WaylandCapture};
use gsnag_record::{AudioMode, Control};
use std::{
    io::{Read, Write},
    os::unix::{
        fs::PermissionsExt,
        net::{UnixListener, UnixStream},
    },
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

#[derive(Args)]
pub struct Options {
    #[command(subcommand)]
    command: Option<Action>,
}
#[derive(Subcommand)]
enum Action {
    /// Record a display (or the complete desktop).
    Output {
        #[arg(required_unless_present = "all", conflicts_with = "all")]
        name: Option<String>,
        #[arg(long)]
        all: bool,
        #[command(flatten)]
        save: Save,
    },
    /// Select a region, then record it.
    Region(Save),
    Stop,
    Pause,
    Resume,
    Status,
}
#[derive(Clone, Copy, ValueEnum)]
pub enum Audio {
    None,
    System,
    Microphone,
    Both,
}
#[derive(Args)]
struct Save {
    #[arg(long)]
    out: PathBuf,
    #[arg(long, default_value_t=15, value_parser=clap::value_parser!(u32).range(1..=60))]
    fps: u32,
    #[arg(long, default_value_t=1280, value_parser=clap::value_parser!(u32).range(320..=3840))]
    max_width: u32,
    #[arg(long)]
    cursor: bool,
    #[arg(long, value_enum, default_value = "none")]
    audio: Audio,
    /// PipeWire node.name for the microphone; defaults to the desktop's input.
    #[arg(long)]
    microphone: Option<String>,
    /// PipeWire node.name for system audio; defaults to the desktop's output.
    #[arg(long)]
    system: Option<String>,
    #[arg(long, value_parser=clap::value_parser!(u64).range(1..=86400))]
    duration: Option<u64>,
    #[arg(long, value_enum, default_value = "auto")]
    backend: super::CaptureBackend,
    #[arg(long)]
    overwrite: bool,
}
pub fn prepare(
    name: Option<&str>,
    region: bool,
    backend: Backend,
    cursor: bool,
) -> Result<Option<(Vec<gsnag_core::Output>, Option<gsnag_core::Rect>)>> {
    let report = gsnag_proto::inspect()?;
    let outputs: Vec<_> = report
        .outputs
        .into_iter()
        .filter(|o| name.is_none_or(|n| n == o.name))
        .collect();
    ensure!(!outputs.is_empty(), "No matching display");
    let rect = if region {
        ensure!(
            report.capabilities.layer_shell && report.capabilities.xdg_output,
            "Region selection requires layer-shell and xdg-output"
        );
        let mut source = WaylandCapture { backend };
        let frames = outputs
            .iter()
            .map(|o| source.capture_output(&o.name, cursor))
            .collect::<Result<Vec<_>>>()?;
        let desktop = gsnag_core::compose(&frames)?;
        let Some(rect) = gsnag_overlay::select(&desktop, &outputs)? else {
            return Ok(None);
        };
        Some(rect)
    } else {
        None
    };
    Ok(Some((outputs, rect)))
}
fn socket() -> Result<PathBuf> {
    let dir = std::env::var_os("XDG_RUNTIME_DIR").context("XDG_RUNTIME_DIR is required")?;
    let dir = PathBuf::from(dir);
    ensure!(dir.is_absolute(), "XDG_RUNTIME_DIR must be absolute");
    Ok(dir.join("gsnag-record.sock"))
}
pub fn command(action: &str) -> Result<String> {
    let mut stream = UnixStream::connect(socket()?).context("No recording is running")?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    writeln!(stream, "{action}")?;
    stream.shutdown(std::net::Shutdown::Write)?;
    let mut reply = String::new();
    stream.take(4096).read_to_string(&mut reply)?;
    Ok(reply)
}
pub struct Server {
    path: PathBuf,
    _lock: std::fs::File,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Server {
    pub fn start(control: Arc<Control>) -> Result<Self> {
        use std::os::unix::fs::OpenOptionsExt;
        let path = socket()?;
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(path.with_extension("lock"))?;
        lock.try_lock().context("A recording is already running")?;
        if path.exists() {
            std::fs::remove_file(&path)?;
        }
        let listener = UnixListener::bind(&path)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        listener.set_nonblocking(true)?;
        let stop = Arc::new(AtomicBool::new(false));
        let shutdown = stop.clone();
        let thread = std::thread::spawn(move || {
            while !shutdown.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let _ = stream.set_read_timeout(Some(Duration::from_millis(200)));
                        let _ = stream.set_write_timeout(Some(Duration::from_millis(200)));
                        let mut request = String::new();
                        if (&mut stream)
                            .take(128)
                            .read_to_string(&mut request)
                            .is_err()
                        {
                            continue;
                        }
                        let response = match request.trim() {
                            "stop" => {
                                control.stop();
                                "stopping\n".to_owned()
                            }
                            "pause" => {
                                control.pause(true);
                                "paused\n".to_owned()
                            }
                            "resume" => {
                                control.pause(false);
                                "recording\n".to_owned()
                            }
                            "status" => {
                                let p = control.progress();
                                serde_json::json!({"paused":control.paused(),"stopping":control.stopped(),"seconds":p.seconds,"frames":p.frames,"repeated":p.repeated,"width":p.width,"height":p.height}).to_string()
                            }
                            _ => "unknown command\n".into(),
                        };
                        let _ = stream.write_all(response.as_bytes());
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(30))
                    }
                    Err(_) => break,
                }
            }
        });
        Ok(Self {
            path,
            _lock: lock,
            stop,
            thread: Some(thread),
        })
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        let _ = std::fs::remove_file(&self.path);
    }
}
pub fn run(options: Options) -> Result<()> {
    let Some(action) = options.command else {
        return super::record_ui::run();
    };
    let (name, region, save) = match action {
        Action::Output { name, save, .. } => (name, false, save),
        Action::Region(save) => (None, true, save),
        other => {
            let action = match other {
                Action::Stop => "stop",
                Action::Pause => "pause",
                Action::Resume => "resume",
                _ => "status",
            };
            println!("{}", command(action)?);
            return Ok(());
        }
    };
    let backend = match save.backend {
        super::CaptureBackend::Auto => Backend::Auto,
        super::CaptureBackend::Ext => Backend::Ext,
        super::CaptureBackend::Wlr => Backend::Wlr,
    };
    let control = Arc::new(Control::default());
    let _server = Server::start(control.clone())?;
    let stop = control.clone();
    ctrlc::set_handler(move || stop.stop())?;
    let Some((outputs, region)) = prepare(name.as_deref(), region, backend, save.cursor)? else {
        return Ok(());
    };
    let outcome = gsnag_record::record(
        gsnag_record::Options {
            output: save.out,
            outputs,
            region,
            fps: save.fps,
            max_width: save.max_width,
            cursor: save.cursor,
            backend,
            audio: match save.audio {
                Audio::None => AudioMode::None,
                Audio::System => AudioMode::System,
                Audio::Microphone => AudioMode::Microphone,
                Audio::Both => AudioMode::Both,
            },
            microphone: save.microphone,
            system: save.system,
            duration: save.duration.map(Duration::from_secs),
            overwrite: save.overwrite,
        },
        control,
    )?;
    println!(
        "{} ({:.2}s, {}×{}, {} frames)",
        outcome.path.display(),
        outcome.progress.seconds,
        outcome.progress.width,
        outcome.progress.height,
        outcome.progress.frames
    );
    if let Some(warning) = outcome.warning {
        anyhow::bail!("Recording saved, but stopped early: {warning}");
    }
    Ok(())
}
