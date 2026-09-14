#!/usr/bin/env python3
"""Real H.264 recordings on an isolated labwc; requires ffmpeg/ffprobe and wlr-randr."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]
ARTIFACTS = Path(tempfile.mkdtemp(prefix="gsnag-record-test-"))
runtime = ARTIFACTS / "runtime"
runtime.mkdir(mode=0o700)
config = ARTIFACTS / "config"
config.mkdir()
(config / "rc.xml").write_text("<labwc_config/>")
(config / "autostart").write_text("")
env = dict(os.environ, XDG_RUNTIME_DIR=str(runtime), WAYLAND_DISPLAY="wayland-0",
           WLR_BACKENDS="headless", WLR_HEADLESS_OUTPUTS="1", WLR_RENDERER="pixman",
           GSK_RENDERER="cairo", GTK_A11Y="none", GSNAG_LANGUAGE="en")
env.pop("WAYLAND_SOCKET", None)
env.pop("DISPLAY", None)
gsnag = os.environ.get("GSNAG", str(ROOT / "target/debug/gsnag"))
children = []


def run(*args, check=True):
    return subprocess.run(args, env=env, check=check, capture_output=True, text=True, timeout=30)


def wait(predicate, timeout=15):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        if predicate():
            return
        time.sleep(.05)
    raise AssertionError("Timeout")


def record(name, *args):
    log = open(ARTIFACTS / (name + ".log"), "w")
    p = subprocess.Popen([gsnag, "record", "output", "--all", "--out", str(ARTIFACTS / name),
                          "--fps", "5", "--max-width", "640", *args], env=env, stdout=log, stderr=log)
    log.close()
    children.append(p)
    return p


def verify(name, minimum, maximum):
    path = ARTIFACTS / name
    data = json.loads(run("ffprobe", "-v", "error", "-show_streams", "-show_format", "-of", "json", str(path)).stdout)
    video = next(s for s in data["streams"] if s["codec_type"] == "video")
    assert video["codec_name"] == "h264", data
    assert (video["width"], video["height"]) == (640, 360), data
    seconds = float(data["format"]["duration"])
    assert minimum <= seconds <= maximum, seconds
    decoded = run("ffmpeg", "-v", "error", "-i", str(path), "-f", "null", "-")
    assert not decoded.stderr, decoded.stderr
    return data


try:
    log = open(ARTIFACTS / "labwc.log", "w")
    children.append(subprocess.Popen(["labwc", "-C", str(config)], env=env, stdout=log, stderr=log))
    log.close()
    wait(lambda: (runtime / "wayland-0").exists())
    run("wlr-randr", "--output", "HEADLESS-1", "--custom-mode", "640x360@60")
    for backend, ext in [("ext", "mp4"), ("wlr", "mkv")]:
        name = backend + "." + ext
        p = record(name, "--duration", "2", "--backend", backend)
        assert p.wait(timeout=25) == 0, (ARTIFACTS / (name + ".log")).read_text()
        verify(name, 1.7, 2.3)
        before = (ARTIFACTS / name).read_bytes()
        assert run(gsnag, "record", "output", "--all", "--out", str(ARTIFACTS / name), "--duration", "1", check=False).returncode != 0
        assert (ARTIFACTS / name).read_bytes() == before
        print("PASS", backend, ext, "decoded, duration, overwrite protection", flush=True)
    p = record("controls.mp4")
    wait(lambda: (runtime / "gsnag-record.sock").exists())
    wait(lambda: json.loads(run(gsnag, "record", "status").stdout)["frames"] >= 5)
    second = record("second.mp4", "--duration", "1")
    assert second.wait(timeout=10) != 0
    assert not (ARTIFACTS / "second.mp4").exists()
    run(gsnag, "record", "pause")
    time.sleep(.2)
    before = json.loads(run(gsnag, "record", "status").stdout)
    time.sleep(1)
    after = json.loads(run(gsnag, "record", "status").stdout)
    assert before["frames"] == after["frames"] and after["paused"], (before, after)
    run(gsnag, "record", "resume")
    wait(lambda: json.loads(run(gsnag, "record", "status").stdout)["frames"] >= before["frames"] + 5)
    run(gsnag, "record", "stop")
    assert p.wait(timeout=5) == 0, (ARTIFACTS / "controls.mp4.log").read_text()
    verify("controls.mp4", (before["frames"] + 5) / 5, (before["frames"] + 5) / 5 + 3)
    assert not (runtime / "gsnag-record.sock").exists()
    print("PASS IPC pause/resume/stop, singleton, finalized file, socket cleanup", flush=True)
    # A quiet desktop must remain recordable beyond the normal Wayland timeout.
    p = record("static.mp4", "--duration", "12")
    assert p.wait(timeout=25) == 0, (ARTIFACTS / "static.mp4.log").read_text()
    verify("static.mp4", 11.7, 12.3)
    print("PASS static desktop beyond 10 seconds", flush=True)
finally:
    for child in reversed(children):
        if child.poll() is None:
            child.terminate()
            try:
                child.wait(timeout=3)
            except subprocess.TimeoutExpired:
                child.kill()
                child.wait()
    print("Artifacts:", ARTIFACTS, flush=True)
