use std::{path::PathBuf, process::ExitCode};

use anyhow::{Context, Result, ensure};
use clap::{Args, Parser, Subcommand, ValueEnum};
use gsnag_proto::{Backend, FrameSource, WaylandCapture};

mod shortcuts;
mod tray;

#[derive(Parser)]
#[command(version, about = "Native Wayland screen capture")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Install, remove or inspect labwc keyboard shortcuts.
    Shortcuts(shortcuts::Options),
    /// Keep a capture shortcut in the desktop system tray.
    Tray,
    /// Report compositor protocols and capture capabilities.
    Doctor,
    /// Open an image or an editable .gsnag project.
    Edit {
        input: PathBuf,
        /// Suggested filename in the export dialog.
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Render a .gsnag project to PNG, JPEG or WebP without opening the editor.
    Export {
        input: PathBuf,
        #[arg(long)]
        out: PathBuf,
        #[arg(long)]
        overwrite: bool,
    },
    /// List connected displays and their logical geometry.
    Outputs {
        #[arg(long)]
        json: bool,
    },
    /// Capture a still image.
    Capture {
        #[command(subcommand)]
        target: CaptureTarget,
    },
}

#[derive(Subcommand)]
enum CaptureTarget {
    /// Capture a named output at native resolution, or the logical desktop.
    Output(OutputArgs),
    /// Select a region on the frozen desktop, then press Enter to save.
    Region(SaveArgs),
}

#[derive(Args)]
struct OutputArgs {
    #[arg(required_unless_present = "all", conflicts_with = "all")]
    name: Option<String>,
    #[arg(long)]
    all: bool,
    #[command(flatten)]
    save: SaveArgs,
}

#[derive(Args)]
struct SaveArgs {
    #[arg(long, value_name = "FILE.png", required_unless_present = "edit")]
    out: Option<PathBuf>,
    /// Open the captured image in the annotation editor. --out suggests an export filename.
    #[arg(long)]
    edit: bool,
    /// Include the pointer in the captured image.
    #[arg(long)]
    cursor: bool,
    #[arg(long, value_enum, default_value = "auto")]
    backend: CaptureBackend,
    /// Replace an existing destination file.
    #[arg(long)]
    overwrite: bool,
}

#[derive(Clone, Copy, ValueEnum)]
enum CaptureBackend {
    Auto,
    Ext,
    Wlr,
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("gsnag: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Command::Shortcuts(options) => shortcuts::run(options)?,
        Command::Tray => tray::run()?,
        Command::Edit { input, out } => {
            let doc = gsnag_editor::storage::load(&input)
                .with_context(|| format!("Cannot open {}", input.display()))?;
            let project = gsnag_editor::storage::is_project(&input).then_some(input);
            gsnag_editor::open(doc, project, out)?;
        }
        Command::Export {
            input,
            out,
            overwrite,
        } => {
            let doc = gsnag_editor::storage::load(&input)?;
            let image = gsnag_editor::render::render(&doc)?;
            gsnag_editor::storage::export_image(&image, &out, overwrite)?;
            println!("{} ({}x{})", out.display(), image.width(), image.height());
        }
        Command::Doctor => println!(
            "{}",
            serde_json::to_string_pretty(&gsnag_proto::inspect()?)?
        ),
        Command::Outputs { json } => {
            let report = gsnag_proto::inspect()?;
            if json {
                println!("{}", serde_json::to_string_pretty(&report.outputs)?);
            } else {
                for o in report.outputs {
                    println!(
                        "{}\t{}x{} at {},{}\tscale {}\t{}",
                        o.name, o.logical_width, o.logical_height, o.x, o.y, o.scale, o.description
                    );
                }
            }
        }
        Command::Capture { target } => {
            let save = match &target {
                CaptureTarget::Output(args) => &args.save,
                CaptureTarget::Region(args) => args,
            };
            if !save.edit {
                let out = save.out.as_ref().context("Provide --out or --edit")?;
                ensure!(
                    out.extension()
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("png")),
                    "Direct capture exports PNG; use an .png destination or --edit"
                );
                ensure!(
                    save.overwrite || !out.try_exists()?,
                    "Destination already exists; use --overwrite to replace it"
                );
            }
            let mut source = WaylandCapture {
                backend: match save.backend {
                    CaptureBackend::Auto => Backend::Auto,
                    CaptureBackend::Ext => Backend::Ext,
                    CaptureBackend::Wlr => Backend::Wlr,
                },
            };
            let image = if let CaptureTarget::Output(OutputArgs {
                name: Some(name), ..
            }) = &target
            {
                source.capture_output(name, save.cursor)?.image
            } else {
                let report = gsnag_proto::inspect()?;
                ensure!(
                    report.capabilities.xdg_output,
                    "Desktop and region capture require xdg-output logical geometry"
                );
                let region = matches!(target, CaptureTarget::Region(_));
                ensure!(
                    !region || report.capabilities.layer_shell,
                    "Region selection requires layer-shell support"
                );
                let frames = report
                    .outputs
                    .iter()
                    .map(|o| source.capture_output(&o.name, save.cursor))
                    .collect::<Result<Vec<_>>>()?;
                let desktop = gsnag_core::compose(&frames)?;
                if region {
                    let outputs = frames.iter().map(|f| f.output.clone()).collect::<Vec<_>>();
                    let bounds = gsnag_core::desktop_bounds(&outputs)?;
                    drop(frames);
                    eprintln!("Drag to select; Enter saves, Esc or right-click cancels.");
                    let Some(rect) = gsnag_overlay::select(&desktop, &outputs)? else {
                        eprintln!("Selection cancelled");
                        return Ok(());
                    };
                    gsnag_core::crop_region(&desktop, bounds, rect)?
                } else {
                    desktop
                }
            };
            if save.edit {
                let doc = gsnag_editor::model::Document::new(image)?;
                gsnag_editor::open(doc, None, save.out.clone())?;
            } else {
                let out = save.out.as_ref().context("Provide --out or --edit")?;
                gsnag_editor::storage::export_image(&image, out, save.overwrite)?;
                println!("{} ({}x{})", out.display(), image.width(), image.height());
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editor_commands_and_capture_handoff_parse_without_a_destination() {
        for args in [
            vec!["gsnag", "tray"],
            vec!["gsnag", "capture", "region", "--edit"],
            vec!["gsnag", "capture", "output", "HDMI-A-1", "--edit"],
            vec!["gsnag", "capture", "output", "--all", "--edit"],
            vec!["gsnag", "edit", "example.gsnag"],
            vec!["gsnag", "export", "example.gsnag", "--out", "image.webp"],
        ] {
            assert!(Cli::try_parse_from(args).is_ok());
        }
        assert!(Cli::try_parse_from(["gsnag", "edit"]).is_err());
        assert!(Cli::try_parse_from(["gsnag", "export", "example.gsnag"]).is_err());
    }

    #[test]
    fn region_requires_destination_and_accepts_capture_options() {
        assert!(Cli::try_parse_from(["gsnag", "capture", "region"]).is_err());
        assert!(
            Cli::try_parse_from([
                "gsnag",
                "capture",
                "region",
                "--out",
                "region.png",
                "--cursor",
                "--backend",
                "wlr",
                "--overwrite"
            ])
            .is_ok()
        );
        assert!(
            Cli::try_parse_from([
                "gsnag",
                "capture",
                "region",
                "HDMI-A-1",
                "--out",
                "region.png"
            ])
            .is_err()
        );
    }

    #[test]
    fn output_requires_exactly_one_selector_and_destination() {
        assert!(
            Cli::try_parse_from([
                "gsnag", "capture", "output", "HDMI-A-1", "--out", "test.png"
            ])
            .is_ok()
        );
        assert!(
            Cli::try_parse_from(["gsnag", "capture", "output", "--all", "--out", "test.png"])
                .is_ok()
        );
        assert!(Cli::try_parse_from(["gsnag", "capture", "output", "--out", "test.png"]).is_err());
        assert!(
            Cli::try_parse_from([
                "gsnag", "capture", "output", "HDMI-A-1", "--all", "--out", "test.png"
            ])
            .is_err()
        );
        assert!(Cli::try_parse_from(["gsnag", "capture", "output", "HDMI-A-1"]).is_err());
    }
}
