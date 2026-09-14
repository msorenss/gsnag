# Capture implementation notes

The project uses a six-crate workspace. Phases 1–3 implement native still capture,
the frozen-screen region selector and the annotation editor. Recording remains
a skeleton.

## Protocol behavior

- The registry is collected before binding optional managers. Two synchronization
  barriers collect wl_output state; xdg-output then supplies logical geometry.
- ext capture waits for the session's complete buffer constraints, allocates a
  fresh memfd, attaches a wl_shm buffer and damages the entire frame before capture.
- wlr capture waits for buffer_done at version 3 and uses the buffer event for
  versions 1 and 2. The compositor controls stride and format.
- Pixel conversion supports native-endian ARGB/XRGB/ABGR/XBGR8888, row padding,
  premultiplied alpha and wlr Y inversion. The buffer/output transform is inverted
  to recover display orientation before composing logical outputs.
- Capture and registry waits have a ten-second deadline. Errors such as missing
  protocols, stopped sessions, unknown formats and disconnected displays propagate
  to the CLI. Each still owns its Wayland connection, releasing all server objects
  even when capture fails; successful captures also destroy objects explicitly.
- Buffer and composed-image allocations are limited to 100 megapixels / 400 MB.
- Foreign-toplevel capability detection does not yet bind/enumerate window handles.
  A future window-selection implementation must account for the missing capture
  source manager on this machine.

Continuous capture, PTS retention, buffer reuse, damage tracking and dmabuf are
future recording work; the current FrameSource API is specifically for stills.
Direct capture and editor exports use the same atomic writer: encode to a
temporary file in the destination directory, flush/sync, then persist using
replacement or no-clobber semantics. An encoding failure removes the temporary
file and leaves an existing destination intact.

## References

- [wayland-client](https://docs.rs/wayland-client/0.31.15/wayland_client/)
- [ext-image-copy-capture client bindings](https://docs.rs/wayland-protocols/0.32.13/wayland_protocols/ext/image_copy_capture/v1/client/index.html)
- [wlr-screencopy client bindings](https://docs.rs/wayland-protocols-wlr/0.3.12/wayland_protocols_wlr/screencopy/v1/client/index.html)
- [grim rendering reference](https://github.com/emersion/grim/blob/master/render.c)
- [GTK4 layer-shell Rust API](https://docs.rs/gtk4-layer-shell/0.7.1/gtk4_layer_shell/trait.LayerShell.html)
- [GTK4 FileDialog](https://gtk-rs.org/gtk4-rs/stable/latest/docs/gtk4/struct.FileDialog.html)
- [GTK4 clipboard and drag data transfer](https://blogs.gnome.org/gtk/2020/01/29/data-transfer-in-gtk4/)

## Region selection

`gsnag capture region --out FILE.png` captures every output, composes the logical
desktop, and only then maps GTK windows. One Cairo ARGB32 image is shared by all
output surfaces. Cairo receives native-endian premultiplied pixels; exported
pixels come from the original RGBA composition, not a recapture of the overlay.
`desktop_bounds` is shared by composition and cropping, preserving negative
origins, mixed scales and transparent gaps with half-open rectangle edges.

Each GTK window uses the overlay layer, all four anchors, exclusive keyboard
interactivity and exclusive zone -1 to cover panels without reserving space.
Monitors are matched by connector name and checked against captured logical
geometry before mapping. Layout changes or disconnected outputs terminate the
session, destroy its windows, and leave the destination alone.

Pointer events are handled by a capture-phase `EventControllerLegacy`. Wayland's
implicit pointer grab keeps motion/release events on the originating surface
throughout a drag, with coordinates allowed outside that surface. Adding that
surface's logical origin yields global coordinates even when crossing to a
different-scale output. Every surface redraws from the same selection state.
Corner resizing preserves the prior ratio with Shift; new Shift selections are
square. Keyboard nudges bypass magnetic snapping for single-pixel adjustment.

Enter confirms and Esc/right-click cancel. The selection is retained after mouse
release to allow adjustment. Empty rectangles and regions wholly inside desktop
gaps cannot be confirmed. All GTK windows are destroyed on completion; the CLI
opens the PNG destination only after confirmation. Tab explains that window mode
is unavailable. Foreign-toplevel enumeration and exact window capture remain
deferred because the installed compositor lacks the capture-source manager.

The XML bundled with the locked protocol crates is the implementation reference
for event ordering, buffer constraints, transforms and destruction rules.

## Early desktop package

The user requested a testable `.deb` before the later integration phases and
deferred global shortcuts. `make deb` builds the locked release binary, then
`packaging/build-deb.py` stages it with a desktop menu entry and SVG icon.
`dpkg-shlibdeps` derives runtime dependencies from the actual binary;
`dpkg-deb --root-owner-group` produces a native-architecture package without
requiring root or cargo-deb. No maintainer scripts change desktop settings.
The menu entry now runs `gsnag tray` in the current Wayland session; a desktop
action still offers direct region capture. Installation is performed explicitly
with apt, as described in README.

## Tray shortcut

`gsnag tray` uses a libadwaita/GApplication instance named `se.gsnag.Tray` for
single-instance activation. ksni 0.3.6 runs its StatusNotifierItem service on its
own runtime thread; an async-channel feeds actions to the GLib main context.
The icon supplies both a theme name and an embedded ARGB pixmap. The menu offers
region capture, composed-desktop capture and exit. A missing/restarting panel
shows a small GTK launcher while ksni waits and re-registers with the watcher.

Capture commands run as child processes using the absolute current executable,
after 250 ms for menu dismissal. This increment deliberately retains the existing
capture/editor event loops rather than forwarding all CLI commands into GTK.
The tray disables capture actions until its child exits, reports child failures
visibly and reaps completed children. Exiting the tray preserves an open editor.
Automatic login startup, configuration UI and history remain deferred.

The private-bus integration test exports a minimal StatusNotifierWatcher and
drives the actual SNI/DBusMenu interfaces, including host restart and duplicate
activation. See the [ksni API](https://docs.rs/ksni/0.3.6/ksni/) for its runtime and
watcher-reconnection behavior.

## Labwc shortcut installation

`gsnag shortcuts install|uninstall|status` supports both `labwc_config` and the
namespaced `openbox_config` used by Raspberry Pi OS. roxmltree validates XML and
supplies byte ranges; edits preserve the rest of the original text. Conflicting
Print, Shift+Print and Super+Shift+S bindings are replaced with owned markers,
and new bindings are enclosed by `gsnag:begin`/`gsnag:end` comments. Commands use
quoted executable paths and XML escaping. Unsupported capture modes get no keys.

Before writing, the installer saves a uniquely named backup and restoration
metadata beside rc.xml. An OS file lock serializes installs, and temporary files
are synced before atomic replacement. Uninstall restores byte-identical content
when unchanged, preserves unrelated later edits, and refuses changes inside its
managed section. If no user configuration existed, installation copies the first
system rc.xml from XDG_CONFIG_DIRS; unchanged uninstall removes that generated
file. `labwc --reconfigure` runs after successful changes unless `--no-reload`
was requested. A failed reload reports that the files were already saved.

## Annotation editor internals

`gsnag-editor` separates the serializable document model, renderer, storage,
canvas interaction and GTK/libadwaita shell. The immutable base is an
`Arc<RgbaImage>`; annotations retain source-image coordinates, style, text,
freehand points and step numbers. Hit-testing searches back-to-front and uses
stroke distance for outlines and lines. Dragging a completed object creates a
preview with the original object omitted; committing the gesture creates one
reversible command. Undo/redo records metadata snapshots, never base-image
copies. A saved-content checkpoint distinguishes unsaved changes even after
undoing back to the saved state.

The Cairo/Pango renderer is shared by canvas, PNG/JPEG/WebP exports, clipboard
textures, drag-out PNGs and the headless `export` command. Cropping changes the
visible/exported rectangle without rewriting the base or object coordinates.
The viewport accounts for crop origin and shadow padding. Rendered images are
cached between edits; filter previews use a lightweight region tint while
dragging, then filter a copy of the original pixels on commit. Text uses Pango
layout/wrapping and supports Unicode. Blur strength and pixel-block size follow
the stroke/filter control. Torn edges clip the bottom edge; shadows add a
20-pixel transparent margin on every side.

Version 1 `.gsnag` projects are ZIPs containing exactly `document.json` and
`base.png`. Loading validates the version, object IDs, coordinates, styles,
point counts and crop bounds. JSON is limited to 16 MiB, PNG entries to 410 MB,
and image decoding to 400 MB, 100 megapixels and 32767 pixels per dimension.
Projects retain original unfiltered pixels; only flattened image exports are
suitable for sharing hidden content. JPEG exports composite alpha on white at
quality 90, and WebP exports are lossless.

GTK file dialogs handle destination selection/overwrite confirmation; saving a
project preserves editability, while exporting an image leaves the project save
state unchanged. Closing a dirty document offers save, discard or cancellation.
Clipboard lifetime follows Wayland: a clipboard manager is needed to retain
the image after the editor exits. Drag-out offers `GdkFileList`, `GdkTexture` and
`image/png`; its temporary PNG stays alive until another drag or editor exit.

`capture ... --edit` hands the captured image directly to the editor after the
region selector has destroyed its windows. Its optional `--out` only suggests
an export path. Plain capture commands keep their original CLI behavior and
now use atomic PNG writing. Application single-instance behavior and tray
integration remain phase 4 work.
