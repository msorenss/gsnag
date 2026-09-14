#!/usr/bin/env python3
"""Integration test against a private, two-output labwc compositor.

Requires labwc, wlr-randr, wtype, wayland-scanner, a C compiler,
libwayland-dev, the GTK4 build dependencies and Python Pillow. Build gsnag first. WTYPE can point to a
locally extracted binary. Artifacts and logs remain in the printed /tmp path.
No input is ever sent to the user's compositor.
"""
import glob
import json
import os
from pathlib import Path
import shlex
import subprocess
import tempfile
import time

from PIL import Image

ROOT = Path(__file__).resolve().parents[1]
ARTIFACTS = Path(tempfile.mkdtemp(prefix="gsnag-region-test-"))
RUNTIME = ARTIFACTS / "runtime"
RUNTIME.mkdir(mode=0o700)
CONFIG = ARTIFACTS / "config"
CONFIG.mkdir()
(CONFIG / "rc.xml").write_text("<labwc_config/>\n")
(CONFIG / "autostart").write_text("")
ENV = dict(os.environ, GSNAG_LANGUAGE="en", XDG_RUNTIME_DIR=str(RUNTIME), WAYLAND_DISPLAY="wayland-0",
           WLR_BACKENDS="headless", WLR_HEADLESS_OUTPUTS="2", WLR_RENDERER="pixman",
           GSK_RENDERER="cairo", GTK_A11Y="none")
ENV.pop("WAYLAND_SOCKET", None)
ENV.pop("DISPLAY", None)
GSNAG = str(ROOT / "target/debug/gsnag")
WTYPE = os.environ.get("WTYPE", "wtype")
children = []


def run(*args):
    return subprocess.run(args, env=ENV, check=True, capture_output=True, text=True)


def wait_until(predicate, seconds=15):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        if predicate():
            return
        time.sleep(0.1)
    raise AssertionError("Timed out waiting for test application")


def capture(name):
    path = ARTIFACTS / name
    run(GSNAG, "capture", "output", "--all", "--out", str(path))
    return Image.open(path).convert("RGBA")


def start_region(name, *options):
    log = open(ARTIFACTS / (name + ".log"), "w")
    process = subprocess.Popen([GSNAG, "capture", "region", "--out", str(ARTIFACTS / name), *options],
                               env=ENV, stdout=log, stderr=log)
    log.close()
    children.append(process)
    # The private desktop is static; wait until the overlay actually draws.
    deadline = time.monotonic() + 20
    index = 0
    while time.monotonic() < deadline:
        assert process.poll() is None, (ARTIFACTS / (name + ".log")).read_text()
        path = ARTIFACTS / f"ready-{name}-{index}.png"
        run(GSNAG, "capture", "output", "--all", "--out", str(path))
        current = Image.open(path).convert("RGBA")
        path.unlink()
        if current.tobytes() != baseline.tobytes():
            return process
        index += 1
        time.sleep(0.2)
    raise AssertionError("Overlay did not draw")


def key(*args):
    # Allow GTK to bind wl_keyboard when the first virtual keyboard appears.
    run(WTYPE, "-s", "250", *args)
    time.sleep(0.15)


def point(x, y):
    # Layout is [-1280, 640) x [0, 720); input is relative to its bounding box.
    pointer.stdin.write(f"move {x + 1280} {y} 1920 720\n")
    pointer.stdin.flush()
    time.sleep(0.15)


def button(which, state):
    pointer.stdin.write(f"button {which} {state}\n")
    pointer.stdin.flush()
    time.sleep(0.15)


def finish(process, expected=0):
    assert process.wait(timeout=10) == expected
    restored = capture(f"restored-{len(list(ARTIFACTS.glob('restored-*')))}.png")
    assert restored.tobytes() == baseline.tobytes(), "Overlay surface remained after exit"


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
    run("cc", "-Wall", "-Wextra", "-Werror", str(ROOT / "tests/test-desktop.c"),
        *flags, "-o", str(ARTIFACTS / "test-desktop"))
    log = open(ARTIFACTS / "labwc.log", "w")
    compositor = subprocess.Popen(["labwc", "-C", str(CONFIG)], env=ENV, stdout=log, stderr=log)
    log.close()
    children.append(compositor)
    wait_until(lambda: (RUNTIME / "wayland-0").exists())
    run("wlr-randr", "--output", "HEADLESS-1", "--pos", "-1280,0",
        "--output", "HEADLESS-2", "--scale", "2", "--pos", "0,0")
    time.sleep(0.3)
    outputs = json.loads(run(GSNAG, "outputs", "--json").stdout)
    assert sorted((o["x"], o["logical_width"], o["logical_height"]) for o in outputs) == [(-1280, 1280, 720), (0, 640, 360)]
    pointer = subprocess.Popen([str(ARTIFACTS / "pointer")], env=ENV, stdin=subprocess.PIPE, text=True)
    children.append(pointer)
    background = subprocess.Popen([str(ARTIFACTS / "test-desktop")], env=ENV)
    children.append(background)
    time.sleep(1)
    baseline = capture("baseline.png")
    assert baseline.getpixel((100, 100))[0] > 0, "Test background did not appear"

    process = start_region("cross-output.png")
    point(-200, 180)
    button(1, 1)
    point(-10, 230)
    point(200, 300)
    button(1, 0)
    # Exercise selection movement and keyboard resizing before accepting.
    key("-k", "Right")
    key("-M", "ctrl", "-k", "Down", "-m", "ctrl")
    capture("selected-overlay.png")
    key("-k", "Return")
    finish(process)
    actual = Image.open(ARTIFACTS / "cross-output.png").convert("RGBA")
    expected = baseline.crop((1081, 180, 1481, 301))
    assert actual.size == (400, 121), actual.size
    assert actual.tobytes() == expected.tobytes(), "Saved pixels differ from frozen background"
    print("PASS: cross-output drag, negative origin, scale 1/2, arrows, Ctrl+arrows, Enter, frozen crop", flush=True)

    # A reverse drag starts on the scaled output and ends on the other surface.
    process = start_region("reverse-wlr.png", "--backend", "wlr")
    point(200, 300)
    button(1, 1)
    point(-200, 180)
    button(1, 0)
    key("-k", "Return")
    finish(process)
    actual = Image.open(ARTIFACTS / "reverse-wlr.png").convert("RGBA")
    assert actual.size == (400, 120), actual.size
    assert actual.tobytes() == baseline.crop((1080, 180, 1480, 300)).tobytes()
    print("PASS: reverse drag from HiDPI output with wlr capture", flush=True)

    process = start_region("adjusted.png")
    point(-600, 160)
    button(1, 1)
    shift = subprocess.Popen([WTYPE, "-s", "250", "-M", "shift", "-s", "1500", "-m", "shift"], env=ENV)
    children.append(shift)
    time.sleep(0.5)
    point(-440, 260)
    button(1, 0)
    assert shift.wait(timeout=5) == 0
    # The Shift-drag above makes a 160 x 160 square. Move it, then resize a corner.
    point(-520, 220)
    button(1, 1)
    point(-500, 240)
    button(1, 0)
    point(-420, 340)
    button(1, 1)
    point(-400, 350)
    button(1, 0)
    key("-k", "Return")
    finish(process)
    actual = Image.open(ARTIFACTS / "adjusted.png").convert("RGBA")
    assert actual.size == (180, 170), actual.size
    assert actual.tobytes() == baseline.crop((700, 180, 880, 350)).tobytes()
    print("PASS: Shift square, mouse movement and corner resize", flush=True)

    process = start_region("cancel.png")
    key("-k", "Return")  # Empty Enter keeps selection open.
    assert process.poll() is None
    key("-k", "Tab")  # Unsupported window mode leaves region selection usable.
    assert process.poll() is None
    key("-k", "Escape")
    finish(process)
    assert not (ARTIFACTS / "cancel.png").exists()

    preserved = ARTIFACTS / "preserved.png"
    preserved.write_bytes(b"preserve this existing destination")
    process = start_region("preserved.png", "--overwrite")
    point(-100, 200)
    button(3, 1)
    button(3, 0)
    finish(process)
    assert preserved.read_bytes() == b"preserve this existing destination"
    print("PASS: empty Enter, Tab fallback, Escape, right-click, no writes on cancellation, surface cleanup", flush=True)

    process = start_region("layout-change.png")
    run("wlr-randr", "--output", "HEADLESS-2", "--off")
    assert process.wait(timeout=10) == 1
    assert not (ARTIFACTS / "layout-change.png").exists()
    assert "during selection; try again" in (ARTIFACTS / "layout-change.png.log").read_text()
    print("PASS: output disconnect aborts cleanly", flush=True)
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
