#!/usr/bin/env python3
"""Exercise the editor in a private headless labwc, never the user's desktop.

Requires the region-smoke.py tools and optionally wl-paste (WL_PASTE override).
WTYPE may point to a locally extracted binary. Screenshots, projects and logs
remain in the printed /tmp directory. Build the workspace first.
"""
import glob
import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import tempfile
import time
import zipfile

from PIL import Image

ROOT = Path(__file__).resolve().parents[1]
ARTIFACTS = Path(tempfile.mkdtemp(prefix="gsnag-editor-test-"))
RUNTIME = ARTIFACTS / "runtime"
RUNTIME.mkdir(mode=0o700)
CONFIG = ARTIFACTS / "config"
CONFIG.mkdir()
(CONFIG / "rc.xml").write_text("<labwc_config/>\n")
(CONFIG / "autostart").write_text("")
ENV = dict(os.environ, XDG_RUNTIME_DIR=str(RUNTIME), WAYLAND_DISPLAY="wayland-0",
           WLR_BACKENDS="headless", WLR_HEADLESS_OUTPUTS="1", WLR_RENDERER="pixman",
           GSK_RENDERER="cairo", GTK_A11Y="none", GTK_USE_PORTAL="0", GIO_USE_VFS="local")
# GTK4 file dialogs can still contact a portal despite GTK_USE_PORTAL=0.
# Keep them inside this compositor, with no access to the desktop session bus.
ENV["DBUS_SESSION_BUS_ADDRESS"] = "unix:path=" + str(RUNTIME / "no-session-bus")
ENV["GSETTINGS_BACKEND"] = "memory"
ENV["XDG_CONFIG_HOME"] = str(ARTIFACTS / "settings")
ENV.pop("WAYLAND_SOCKET", None)
ENV.pop("DISPLAY", None)
GSNAG = os.environ.get("GSNAG", str(ROOT / "target/debug/gsnag"))
WTYPE = os.environ.get("WTYPE", "wtype")
WL_PASTE = os.environ.get("WL_PASTE") or shutil.which("wl-paste")
children = []


def run(*args):
    return subprocess.run(args, env=ENV, check=True, capture_output=True, text=True)


def wait_until(predicate, seconds=15):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        if predicate():
            return
        time.sleep(0.1)
    raise AssertionError("Timed out waiting for application; inspect artifacts")


def screenshot(name):
    path = ARTIFACTS / name
    run(GSNAG, "capture", "output", "HEADLESS-1", "--out", str(path))
    return Image.open(path).convert("RGBA")


def key(name, ctrl=False, shift=False):
    args = [WTYPE, "-s", "400"]
    if ctrl:
        args += ["-M", "ctrl"]
    if shift:
        args += ["-M", "shift"]
    args += ["-k", name]
    if shift:
        args += ["-m", "shift"]
    if ctrl:
        args += ["-m", "ctrl"]
    args += ["-s", "100"]
    run(*args)
    time.sleep(0.2)


def point(x, y):
    pointer.stdin.write(f"move {round(x)} {round(y)} 1280 720\n")
    pointer.stdin.flush()
    time.sleep(0.12)


def button(state, which=1):
    pointer.stdin.write(f"button {which} {state}\n")
    pointer.stdin.flush()
    time.sleep(0.12)


def click(x, y):
    point(x, y)
    button(1)
    button(0)


def draw(tool, points):
    key(tool)
    point(origin[0] + points[0][0], origin[1] + points[0][1])
    button(1)
    for x, y in points[1:]:
        point(origin[0] + x, origin[1] + y)
    button(0)
    time.sleep(0.2)


def save_dialog(path):
    time.sleep(0.7)
    # The CLI-suggested name is already filled in by GtkFileDialog.
    screenshot("dialog-" + path.name + ".png")
    key("Return")
    time.sleep(0.6)
    if not path.exists():
        key("Return")
    wait_until(path.exists)
    time.sleep(0.3)


def project_content(path):
    with zipfile.ZipFile(path) as archive:
        return json.loads(archive.read("document.json"))["document"]


def spawn_editor(*args):
    log = open(ARTIFACTS / f"editor-{len(children)}.log", "w")
    proc = subprocess.Popen([GSNAG, *args], env=ENV, stdout=log, stderr=log)
    log.close()
    children.append(proc)
    time.sleep(1.2)
    assert proc.poll() is None, "Editor exited during startup"
    return proc


try:
    xmls = glob.glob(str(ROOT / ".tools/cargo/registry/src/*/wayland-protocols-wlr-*/wlr-protocols/unstable/wlr-virtual-pointer-unstable-v1.xml"))
    xmls += glob.glob(os.path.expanduser("~/.cargo/registry/src/*/wayland-protocols-wlr-*/wlr-protocols/unstable/wlr-virtual-pointer-unstable-v1.xml"))
    xml = os.environ.get("WLR_POINTER_XML") or sorted(xmls)[-1]
    run("wayland-scanner", "client-header", xml, str(ARTIFACTS / "virtual-pointer.h"))
    run("wayland-scanner", "private-code", xml, str(ARTIFACTS / "virtual-pointer.c"))
    flags = shlex.split(run("pkg-config", "--cflags", "--libs", "wayland-client").stdout)
    run("cc", "-Wall", "-Wextra", "-Werror", str(ROOT / "tests/virtual-pointer.c"),
        str(ARTIFACTS / "virtual-pointer.c"), "-I" + str(ARTIFACTS), *flags, "-o", str(ARTIFACTS / "pointer"))
    flags = shlex.split(run("pkg-config", "--cflags", "--libs", "gtk4-layer-shell-0", "gtk4").stdout)
    run("cc", "-Wall", "-Wextra", "-Werror", str(ROOT / "tests/image-drop-target.c"),
        *flags, "-o", str(ARTIFACTS / "drop-target"))
    log = open(ARTIFACTS / "labwc.log", "w")
    compositor = subprocess.Popen(["labwc", "-C", str(CONFIG)], env=ENV, stdout=log, stderr=log)
    log.close()
    children.append(compositor)
    wait_until(lambda: (RUNTIME / "wayland-0").exists())
    pointer = subprocess.Popen([str(ARTIFACTS / "pointer")], env=ENV, stdin=subprocess.PIPE, text=True)
    children.append(pointer)
    base = Image.new("RGBA", (640, 360))
    for y in range(360):
        for x in range(640):
            base.putpixel((x, y), (253, 0, 253, 255) if x < 2 or y < 2 or x >= 638 or y >= 358
                          else (40 + x // 4, 35 + y // 2, 100 + (x // 20 + y // 20) % 2 * 80, 255))
    base_path = ARTIFACTS / "base.png"
    base.save(base_path)
    export_path = ARTIFACTS / "edited.png"
    editor = spawn_editor("edit", str(base_path), "--out", str(export_path))
    key("Escape")
    key("1", ctrl=True)
    screen = screenshot("initial-editor.png")
    pink = [(x, y) for y in range(screen.height) for x in range(screen.width)
            if screen.getpixel((x, y))[:3] == (253, 0, 253)]
    assert pink, "Cannot locate source image in editor screenshot"
    origin = (min(p[0] for p in pink), min(p[1] for p in pink))
    assert max(p[0] for p in pink) - origin[0] == 639
    assert max(p[1] for p in pink) - origin[1] == 359

    draw("a", [(60, 60), (250, 120)])
    draw("r", [(60, 160), (220, 260)])
    draw("v", [(60, 190), (80, 200)])
    draw("v", [(240, 270), (260, 290)])
    key("z", ctrl=True)
    key("z", ctrl=True, shift=True)
    key("Delete")
    key("z", ctrl=True)
    draw("n", [(300, 220)])
    draw("n", [(350, 220)])
    draw("f", [(300, 60), (320, 90), (350, 70), (370, 95)])
    draw("t", [(380, 170), (610, 220)])
    draw("b", [(400, 260), (460, 330)])
    draw("p", [(480, 260), (540, 330)])
    draw("h", [(380, 160), (610, 190)])
    draw("e", [(15, 15), (100, 80)])
    draw("l", [(280, 300), (380, 310)])
    draw("c", [(400, 25), (600, 135)])
    click(100, 592)
    key("a", ctrl=True)
    run(WTYPE, "-s", "250", "Åäö — klart")
    key("Return", ctrl=True)
    key("Escape")
    screenshot("annotated-editor.png")
    project = ARTIFACTS / "edited.gsnag"
    key("s", ctrl=True)
    save_dialog(project)
    content = project_content(project)
    objects = content["annotations"]
    assert len(objects) == 12, [a["kind"] for a in objects]
    assert next(a for a in objects if a["kind"] == "Callout")["text"] == "Åäö — klart"
    rect = next(a for a in objects if a["kind"] == "Rect")
    for name, expected in {"x": 80, "y": 170, "width": 180, "height": 120}.items():
        assert abs(rect["bounds"][name] - expected) < 0.01, rect
    assert [a["number"] for a in objects if a["kind"] == "Step"] == [1, 2]
    assert len(next(a for a in objects if a["kind"] == "Freehand")["points"]) >= 4
    print("PASS: all annotation tools, move/resize, undo/redo, deletion, step numbering, project save", flush=True)

    key("e", ctrl=True)
    save_dialog(export_path)
    cli_export = ARTIFACTS / "from-project.png"
    run(GSNAG, "export", str(project), "--out", str(cli_export))
    rendered = Image.open(export_path).convert("RGBA")
    assert rendered.tobytes() == Image.open(cli_export).convert("RGBA").tobytes()
    assert rendered.tobytes() != base.tobytes()
    if WL_PASTE:
        key("Escape")
        key("c", ctrl=True)
        pasted = ARTIFACTS / "clipboard.png"
        with pasted.open("wb") as output:
            subprocess.run([WL_PASTE, "--type", "image/png"], env=ENV, stdout=output, check=True, timeout=10)
        assert Image.open(pasted).convert("RGBA").tobytes() == rendered.tobytes()
        print("PASS: clipboard pixels match PNG export", flush=True)
    else:
        print("SKIP: install wl-paste or set WL_PASTE to verify clipboard", flush=True)
    dropped = ARTIFACTS / "dropped.png"
    before_drop = screenshot("pre-drop.png").getpixel((1180, 100))
    receiver = subprocess.Popen([str(ARTIFACTS / "drop-target"), str(dropped)], env=ENV)
    children.append(receiver)
    for attempt in range(30):
        assert receiver.poll() is None, "Drop target exited during startup"
        if screenshot(f"drop-ready-{attempt}.png").getpixel((1180, 100)) != before_drop:
            break
        time.sleep(0.2)
    else:
        raise AssertionError("Drop target did not map")
    screenshot("drop-target.png")
    point(832, 697)
    button(1)
    point(920, 600)
    # PNG preparation precedes Wayland start_drag; keep moving after that grab.
    time.sleep(0.8)
    point(1180, 130)
    time.sleep(0.3)
    point(1181, 131)
    time.sleep(0.3)
    button(0)
    wait_until(dropped.exists)
    assert receiver.wait(timeout=5) == 0
    assert Image.open(dropped).convert("RGBA").tobytes() == rendered.tobytes()
    print("PASS: dragging the image offers a PNG that matches the export", flush=True)
    key("q", ctrl=True)
    assert editor.wait(timeout=10) == 0
    reopened = spawn_editor("edit", str(project))
    key("Escape")
    key("1", ctrl=True)
    screenshot("reopened-project.png")
    draw("x", [(20, 20), (620, 340)])
    key("z", ctrl=True)
    key("y", ctrl=True)
    key("s", ctrl=True)
    time.sleep(0.5)
    crop = project_content(project)["crop"]
    assert crop == {"x": 20.0, "y": 20.0, "width": 600.0, "height": 320.0}, crop
    cropped = ARTIFACTS / "cropped.png"
    run(GSNAG, "export", str(project), "--out", str(cropped))
    assert Image.open(cropped).convert("RGBA").tobytes() == rendered.crop((20, 20, 620, 340)).tobytes()
    print("PASS: non-destructive crop, crop undo/redo and crop project persistence", flush=True)
    key("q", ctrl=True)
    assert reopened.wait(timeout=10) == 0, "Saved project should close without a dirty prompt"
    print("PASS: GUI and CLI exports agree, project reopens with editable objects", flush=True)

    # Capture-to-editor must also work after the region selector's GTK loop exits.
    captured = spawn_editor("capture", "region", "--edit", "--out", str(ARTIFACTS / "capture.png"))
    point(400, 200)
    button(1)
    point(800, 500)
    button(0)
    key("Return")
    time.sleep(1)
    assert captured.poll() is None
    screenshot("capture-to-editor.png")
    key("q", ctrl=True)
    time.sleep(0.3)
    assert captured.poll() is None, "An unsaved capture should offer saving before closing"
    key("Escape")
    capture_project = ARTIFACTS / "capture.gsnag"
    key("s", ctrl=True)
    save_dialog(capture_project)
    with zipfile.ZipFile(capture_project) as archive:
        import io
        assert Image.open(io.BytesIO(archive.read("base.png"))).size == (400, 300)
    key("q", ctrl=True)
    assert captured.wait(timeout=10) == 0
    assert not (ARTIFACTS / "capture.png").exists(), "--edit must not export before confirmation"
    print("PASS: region selection opens editor, project retains captured pixels, no automatic export", flush=True)
finally:
    for child in reversed(children):
        if child.poll() is None:
            child.terminate()
            try:
                child.wait(timeout=5)
            except subprocess.TimeoutExpired:
                child.kill()
                child.wait()
    print(f"Artifacts: {ARTIFACTS}", flush=True)
