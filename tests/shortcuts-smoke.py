#!/usr/bin/env python3
"""Verify real labwc keybindings and byte-exact uninstall on a private compositor."""
import os
from pathlib import Path
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]
ARTIFACTS = Path(tempfile.mkdtemp(prefix='gsnag-shortcuts-test-'))
runtime = ARTIFACTS / 'runtime'
runtime.mkdir(mode=0o700)
config = ARTIFACTS / 'config'
config.mkdir()
rc = config / 'rc.xml'
original = '<labwc_config><keyboard><default/><keybind key="Print"><action name="Execute"><command>false</command></action></keybind></keyboard></labwc_config>\n'
rc.write_text(original)
(config / 'autostart').write_text('')
env = dict(os.environ, XDG_RUNTIME_DIR=str(runtime), WAYLAND_DISPLAY='wayland-0',
           WLR_BACKENDS='headless', WLR_HEADLESS_OUTPUTS='1', WLR_RENDERER='pixman',
           GSK_RENDERER='cairo', GTK_A11Y='none', GSETTINGS_BACKEND='memory',
           DBUS_SESSION_BUS_ADDRESS='unix:path=' + str(runtime / 'no-bus'))
for key in ('DISPLAY', 'WAYLAND_SOCKET'):
    env.pop(key, None)
exe = os.environ.get('GSNAG', str(ROOT / 'target/debug/gsnag'))
wtype = os.environ.get('WTYPE', 'wtype')
compositor = None


def run(*args):
    result = subprocess.run(args, env=env, capture_output=True, text=True)
    assert result.returncode == 0, f'{args}: {result.stderr}'
    return result


def shortcut(action):
    return run(exe, 'shortcuts', action, '--config', str(rc), '--no-reload')


def pids():
    # labwc detaches Execute children; identify only gsnag on this private display.
    result = subprocess.run(['pgrep', '-x', 'gsnag'], capture_output=True, text=True)
    found = set()
    expected = ('XDG_RUNTIME_DIR=' + str(runtime)).encode()
    for pid in map(int, result.stdout.split()):
        try:
            if expected in Path(f'/proc/{pid}/environ').read_bytes().split(b'\0'):
                found.add(pid)
        except (FileNotFoundError, PermissionError, ProcessLookupError):
            pass
    return found


def wait(predicate, seconds=15):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        if predicate():
            return
        time.sleep(.1)
    raise AssertionError('Timed out; inspect ' + str(ARTIFACTS))


try:
    shortcut('install')
    installed = rc.read_bytes()
    shortcut('install')
    assert rc.read_bytes() == installed
    assert 'Installed' in shortcut('status').stdout
    shortcut('uninstall')
    assert rc.read_bytes() == original.encode()
    shortcut('install')
    with (ARTIFACTS / 'labwc.log').open('w') as log:
        compositor = subprocess.Popen(['labwc', '-C', str(config)], env=env, stdout=log, stderr=log)
    env['LABWC_PID'] = str(compositor.pid)
    wait(lambda: (runtime / 'wayland-0').exists())
    for modifiers, desktop in [([], False), (['-M', 'logo', '-M', 'shift'], False), (['-M', 'shift'], True)]:
        before = pids()
        key = 's' if 'logo' in modifiers else 'Print'
        release = ['-m', 'shift', '-m', 'logo'] if 'logo' in modifiers else ['-m', 'shift'] if modifiers else []
        run(wtype, '-s', '300', *modifiers, '-k', key, *release, '-s', '100')
        wait(lambda: bool(pids() - before))
        child = next(iter(pids() - before))
        command = Path(f'/proc/{child}/cmdline').read_bytes()
        assert (b'--all' in command) == desktop, command
        time.sleep(1)
        run(exe, 'capture', 'output', 'HEADLESS-1', '--out', str(ARTIFACTS / (key + ('-desktop' if desktop else '') + '.png')))
        if desktop:
            os.kill(child, 15)
        else:
            run(wtype, '-s', '300', '-k', 'Escape', '-s', '100')
        wait(lambda: child not in pids())
    shortcut('uninstall')
    assert rc.read_bytes() == original.encode()
    run('labwc', '--reconfigure')
    time.sleep(.5)
    before = pids()
    run(wtype, '-s', '300', '-k', 'Print', '-s', '100')
    time.sleep(.5)
    assert pids() == before
    # Preserve unrelated edits while restoring a replaced binding.
    shortcut('install')
    rc.write_text(rc.read_text().replace('</labwc_config>', '<theme><name>test</name></theme></labwc_config>'))
    shortcut('uninstall')
    assert rc.read_text() == original.replace('</labwc_config>', '<theme><name>test</name></theme></labwc_config>')
    # No pre-existing user configuration: remove the generated copy on uninstall.
    rc.unlink()
    shortcut('install')
    shortcut('uninstall')
    assert not rc.exists()
    print('PASS: Print, Shift+Print, Super+Shift+S; idempotence; exact restore; unrelated edits; generated config removal', flush=True)
finally:
    if compositor is not None:
        for pid in pids():
            try:
                os.kill(pid, 15)
            except ProcessLookupError:
                pass
        compositor.terminate()
        compositor.wait(timeout=5)
    print('Artifacts:', ARTIFACTS, flush=True)
