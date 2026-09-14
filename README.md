# gsnag

Native Wayland screen capture for Raspberry Pi OS Trixie / labwc, written in Rust.
Still-image capture includes output capture and an interactive region selector
with a frozen desktop background, a GTK4 annotation editor and a system tray
shortcut. Recording is a later phase.

## Build and run

```sh
sudo apt-get install libgtk-4-dev libgtk4-layer-shell-dev libadwaita-1-dev
make build
make run ARGS='outputs'
make run ARGS='doctor'
make run ARGS='capture output HDMI-A-1 --out /tmp/capture.png'
make run ARGS='capture output --all --out /tmp/desktop.png'
make run ARGS='capture region --out /tmp/region.png'
make run ARGS='capture region --edit'
make run ARGS='edit /tmp/region.png'
make test
make check
```

Run capture commands from your Wayland desktop session. The binary is
`target/debug/gsnag`. Existing files are preserved unless `--overwrite` is given.
Use `--cursor` to include the pointer and `--backend ext|wlr` to test a specific
protocol. The default backend is ext-image-copy-capture when advertised, otherwise
wlr-screencopy. A failed ext session reports its error rather than silently
changing backends.

Rust 1.90.0 is pinned in `rust-toolchain.toml`. On this machine the toolchain is
installed in `.tools/`; the Makefile sets the necessary environment automatically.
Elsewhere, install Rust through rustup, then run the same make targets. The current
capture backend uses native Rust Wayland protocols; the region selector requires
GTK4 (4.12+) and gtk4-layer-shell (1.0+). The editor requires libadwaita (1.5+).
FFmpeg is not needed yet. Build
concurrency defaults to three jobs for the Pi. Development debug symbols are
disabled to reduce GTK build memory; use `CARGO_PROFILE_DEV_DEBUG=1` when needed
for debugging. For a lower-memory first build, use `CARGO_BUILD_JOBS=1 make build`.

## Install as a desktop app (.deb)

On the Pi running Debian/Raspberry Pi OS Trixie:

```sh
sudo apt-get install dpkg-dev binutils desktop-file-utils
make deb
sudo apt-get install ./target/debian/gsnag_0.1.2_arm64.deb
```

Open **gsnag** in the app menu (Graphics) to add its system tray icon. Click
the icon to select a region and press **Enter** to open it in the editor.
The package includes the executable and an app icon;
apt resolves runtime library dependencies. This is a native package for the
build machine's architecture, generated from a release build. To replace a test
build with the same version, rebuild and add `--reinstall` to the install command.
Remove it with `sudo apt-get remove gsnag`.

Run these commands from the repository directory. When running from another
directory, use the full path to the `.deb` file.

## Global keyboard shortcuts (labwc)

After installing the package, run these commands as your desktop user, without sudo:

```sh
gsnag shortcuts install
gsnag shortcuts status
# To undo the installation:
gsnag shortcuts uninstall
```

| Shortcut | Action |
| --- | --- |
| Print | Select a region and open the editor |
| Shift+Print | Capture all displays and open the editor |
| Super+Shift+S | Select a region and open the editor |

The installer backs up `rc.xml`, replaces conflicting bindings (including an
existing Print-to-grim binding), and reloads labwc. It preserves other XML and
restores the original bindings on uninstall. If no user rc.xml exists, it copies
the system configuration first; uninstall removes that copy when unchanged.
Unrelated later edits are preserved. If gsnag's managed section was edited,
uninstall stops and points to the backup. Repeated installation is a no-op.

`--config /path/to/rc.xml` supports a custom labwc configuration;
`--no-reload` edits only the file for testing or an offline session. Use `--config`
when your compositor was started with a custom `-C` directory. These bindings
work without the tray running. Exact-window capture, direct-to-clipboard capture
and recording shortcuts are not implemented yet.

## System tray

Run `gsnag tray`, or open gsnag from the app menu. Only one tray instance runs
per desktop session. Left-click captures a region; right-click opens the menu
with **Fånga region**, **Fånga hela skrivbordet** and **Avsluta gsnag**. Some
panels show the menu for both mouse buttons. Full desktop capture includes all
outputs and opens the editor. The desktop entry also offers a direct region
capture action for launchers that support application actions.

While a capture/editor launched from the tray is open, its capture actions are
disabled. Closing the editor or cancelling selection enables them again. Exiting
the tray leaves an already open editor running. A short delay lets the panel menu
close before the screen is captured.

The icon uses StatusNotifierItem, supported by wf-panel-pi on this machine.
If the panel is unavailable, a small window offers the same capture actions;
the tray reconnects when the panel returns. Capture failures appear in that
window. This increment does not enable login autostart or change desktop settings.

## Region selection

```sh
make run ARGS='capture region --out /tmp/region.png'
```

All outputs are captured before the overlay appears. Drag a rectangle, adjust
it, then press **Enter** to save the frozen pixels. The overlay itself is never
included in the export. **Esc** or **right-click** cancels without creating or
changing the destination, including when `--overwrite` is set.

| Control | Action |
| --- | --- |
| Left-drag outside the selection | Draw a new rectangle; snap within 6 logical pixels of display edges |
| Drag inside / drag a corner | Move / resize the selection |
| Shift while drawing / resizing | Draw a square / preserve the selection's aspect ratio |
| Arrow keys | Move by 1 logical pixel |
| Ctrl + arrow keys | Resize the right or bottom edge by 1 logical pixel |
| Shift + arrow keys | Use a 10-pixel step; combines with Ctrl |
| Enter | Save a non-empty selection |
| Esc / right-click | Cancel |
| Tab | Show that window selection is unavailable; remain in region mode |

A crosshair, magnifier, coordinates and live dimensions help place the edges.
Selections can span outputs. Like `capture output --all`, region exports use
one pixel per logical coordinate; HiDPI content is resampled and desktop gaps
stay transparent. The magnifier displays those export pixels. A display layout
change during selection aborts the operation so stale geometry is not used.
`--cursor`, `--backend auto|ext|wlr`, and `--overwrite` also work for regions.

## Annotation editor

```sh
make run ARGS='capture region --edit'
make run ARGS='capture output HDMI-A-1 --edit'
make run ARGS='edit /tmp/region.png --out /tmp/annotated.png'
make run ARGS='edit /tmp/work.gsnag'
make run ARGS='export /tmp/work.gsnag --out /tmp/annotated.webp'
```

`--edit` sends a capture to the editor; `--out` is optional and only suggests a
filename in its export dialog. The editor does not write an image automatically.
`edit` opens PNG, JPEG, WebP or `.gsnag` files. `export` renders a project without
opening a window and preserves existing files unless `--overwrite` is specified.

Choose a tool and drag on the image. Text and callouts also support a single
click to create a default-size object. Step markers are numbered automatically.
Use **Select** to move objects, resize their corner handles, or adjust the
endpoints of a line/arrow. The sidebar has color, stroke/filter strength, font
size, fill, text and step-number controls. **Apply to selection** (Ctrl+Enter)
updates an existing object; the current properties also apply to new objects.
Scroll the sidebar to reach the remaining controls on a small display.

| Tool / action | Shortcut |
| --- | --- |
| Select / rectangle / ellipse / line | V / R / E / L |
| Arrow / freehand / highlight / text | A / F / H / T |
| Blur / pixelate / numbered step / callout | B / P / N / C |
| Crop / pan | X / Space |
| Constrain shapes / line angles while dragging | Shift |
| Move selected object by 1 / 10 pixels | Arrows / Shift+arrows |
| Raise / lower selected object | Page Up / Page Down |
| Delete selected object | Delete |
| Undo / redo | Ctrl+Z / Ctrl+Shift+Z or Ctrl+Y |
| Zoom / pan | Ctrl+wheel / wheel or middle-button drag |
| Fit / actual pixels | Ctrl+0 / Ctrl+1 |
| Save project / save project as | Ctrl+S / Ctrl+Shift+S |
| Export image / copy image | Ctrl+E / Ctrl+C |
| Cancel a gesture and return to Select | Esc |

Crop changes the export area without discarding original pixels; **Reset crop**
restores the full image. Drop shadow and a torn bottom edge are optional image
effects. Undo/redo retains up to 100 completed edits and does not duplicate the
base image. Editor images are limited to 100 megapixels and 32767 pixels per
dimension, including effect padding.

**Save project** writes a `.gsnag` ZIP containing `document.json` and `base.png`,
so objects remain editable after reopening. **Export** writes a flattened PNG,
JPEG (quality 90, transparent areas composited on white), or lossless WebP.
Projects retain the original pixels underneath blur/pixelation; share a flattened
export when hiding content. Saving uses a temporary file and atomic replacement,
so an encoding failure does not truncate an existing destination.

**Copy** puts the rendered image on the Wayland clipboard. Keep the editor open
until pasting if no clipboard manager is running. The **Drag image** button
offers a PNG file and image data to applications that accept drag-and-drop.
Closing with unsaved edits offers saving the project, discarding, or continuing.

## Workspace

| Crate | Current responsibility |
| --- | --- |
| `gsnag-core` | Output geometry, logical desktop composition and region cropping |
| `gsnag-proto` | Registry, xdg-output, capabilities, native shm capture |
| `gsnag` | Capture, editor and project-export CLI |
| `gsnag-overlay` | GTK4 layer-shell region selection, frozen background and input handling |
| `gsnag-editor` | GTK4/libadwaita editor, objects/history, Cairo/Pango rendering and project/export storage |
| `gsnag-record` | Skeleton for software encoding and PipeWire audio |

Single-output images retain native pixel resolution. `--all` combines outputs at
one pixel per logical desktop coordinate, including negative positions and
transparent gaps; HiDPI outputs are resampled. Outputs are captured sequentially,
so moving content across monitors is not an atomic desktop snapshot.

## Status and next work

- Phase 0: workspace, pinned toolchain, build/test/lint targets and environment
  reconnaissance are implemented.
- Phase 1: output discovery, protocol detection, ext capture, wlr fallback and PNG
  output are implemented. Both capture backends work on this Pi at 1920x1080.
- Exact window capture is unavailable on the installed labwc because the foreign
  toplevel capture-source manager is absent. `doctor` reports this separately
  from foreign-toplevel list support. No window-capture command is implemented yet.
- Mixed-scale layout and transforms have unit coverage; rotated and multiple
  physical displays still need live verification.
- Phase 2: frozen-screen region selection is implemented with one overlay per
  output, mouse adjustment, magnifier, snapping and keyboard controls. Window
  mode remains unavailable; Tab reports this instead of selecting an inexact
  window rectangle.
- Phase 3: the annotation editor is implemented, including the planned tools,
  object selection/resizing/z-order, undo/redo, image export, clipboard and
  editable projects. The phase 2 region selector has also been confirmed working
  by the user on the physical desktop.
- Phase 4: the single-instance tray and labwc shortcut install/status/uninstall
  are implemented. Configuration UI, history and login autostart remain upcoming.
- A native `.deb` and desktop menu launcher are available now through `make deb`,
  using dpkg tools. Later phases add recording and distribution packaging polish.

The optional `python3 tests/region-smoke.py` integration test starts a private
headless labwc with two outputs (scale 1 and 2, including a negative origin).
It requires `labwc`, `wlr-randr`, `wtype`, `wayland-scanner`, a C compiler,
`libwayland-dev`, and Python Pillow. It sends input only to that private compositor
and leaves screenshots/logs in the printed temporary directory. `WTYPE` and
`WLR_POINTER_XML` can override the input binary and protocol XML paths.

`python3 tests/editor-smoke.py` exercises the editor in a private headless labwc:
annotation tools, mouse adjustment, undo/redo, saving/reopening projects, crop,
export, dragging a PNG into another GTK app, and capture-to-editor handoff.
It isolates the session bus and settings so file dialogs stay on the test
compositor. It uses the same test tools plus the GTK4/layer-shell development
packages; install `wl-clipboard` or set `WL_PASTE` to also verify clipboard pixels.
Set `GSNAG` to an absolute binary path to test an extracted package instead of
the default development binary.

`python3 tests/tray-smoke.py` verifies the tray on a private D-Bus session and
headless labwc: icon/menu registration, singleton behavior, capture actions,
busy state, cancellation, panel restart and preservation of open editors when
the tray exits. It additionally requires `dbus-daemon`, Python dbus/PyGObject and
`pgrep`, and accepts `GSNAG` and `WTYPE` overrides.

`python3 tests/shortcuts-smoke.py` exercises the real Print, Shift+Print and
Super+Shift+S bindings on a private labwc, checks idempotent installation,
byte-exact removal, preservation of unrelated edits and removal of a generated
configuration. It uses the same `GSNAG`/`WTYPE` overrides and never installs
bindings in the user's configuration.

See [implementation notes](docs/implementation.md) for capture constraints and
protocol references. Machine-specific development notes are kept outside Git.

## License

gsnag is available under the [MIT License](LICENSE).
