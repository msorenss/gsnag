#!/usr/bin/env python3
"""Test tray registration, singleton, capture and panel restart on private D-Bus/Wayland."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time
import dbus

ROOT = Path(__file__).resolve().parents[1]
ARTIFACTS = Path(tempfile.mkdtemp(prefix='gsnag-tray-test-'))
runtime = ARTIFACTS / 'runtime'
runtime.mkdir(mode=0o700)
config = ARTIFACTS / 'config'
config.mkdir()
(config / 'rc.xml').write_text('<labwc_config/>')
(config / 'autostart').write_text('')
env = dict(os.environ, GSNAG_LANGUAGE="sv", XDG_RUNTIME_DIR=str(runtime), WAYLAND_DISPLAY='wayland-0',
           WLR_BACKENDS='headless', WLR_HEADLESS_OUTPUTS='1', WLR_RENDERER='pixman',
           GSK_RENDERER='cairo', GTK_A11Y='none', GTK_USE_PORTAL='0',
           GSETTINGS_BACKEND='memory', XDG_CONFIG_HOME=str(config))
for key in ('DISPLAY', 'WAYLAND_SOCKET'):
    env.pop(key, None)
exe = os.environ.get('GSNAG', str(ROOT / 'target/debug/gsnag'))
wtype = os.environ.get('WTYPE', 'wtype')
children = []
capture_pids = set()


def spawn(args, label):
    with (ARTIFACTS / (label + '.log')).open('w') as log:
        child = subprocess.Popen(args, env=env, stdout=log, stderr=log)
    children.append(child)
    return child


def wait(predicate, seconds=15):
    until = time.monotonic() + seconds
    while time.monotonic() < until:
        if predicate():
            return
        time.sleep(.1)
    raise AssertionError('Timed out; inspect ' + str(ARTIFACTS))


def child_pids():
    output = subprocess.run(['pgrep', '-P', str(tray.pid)], capture_output=True, text=True)
    return set(map(int, output.stdout.split()))


def key(name):
    subprocess.run([wtype, '-s', '300', '-k', name, '-s', '100'], env=env, check=True)


def shot(name):
    subprocess.run([exe, 'capture', 'output', 'HEADLESS-1', '--out', str(ARTIFACTS / name)],
                   env=env, check=True, stdout=subprocess.DEVNULL)


try:
    # No service directories: do not auto-launch portals or desktop daemons.
    bus_config = ARTIFACTS / 'bus.conf'
    bus_config.write_text(
        '<busconfig><type>session</type><listen>unix:tmpdir=' + str(runtime)
        + '</listen><auth>EXTERNAL</auth><policy context="default">'
        '<allow own="*"/><allow send_destination="*"/><allow receive_sender="*"/>'
        '</policy></busconfig>')
    daemon = subprocess.Popen(['dbus-daemon', '--config-file=' + str(bus_config), '--nofork', '--print-address=1'],
                              stdout=subprocess.PIPE, text=True, env=env)
    children.append(daemon)
    env['DBUS_SESSION_BUS_ADDRESS'] = daemon.stdout.readline().strip()
    bus = dbus.bus.BusConnection(env['DBUS_SESSION_BUS_ADDRESS'])
    compositor = spawn(['labwc', '-C', str(config)], 'labwc')
    wait(lambda: (runtime / 'wayland-0').exists())
    registrations = ARTIFACTS / 'items.jsonl'
    watcher = spawn(['python3', str(ROOT / 'tests/tray-watcher.py'), str(registrations)], 'watcher')
    wait(lambda: bus.name_has_owner('org.kde.StatusNotifierWatcher'))
    tray = spawn([exe, 'tray'], 'tray')
    wait(lambda: registrations.exists() and registrations.stat().st_size > 0)
    item = json.loads(registrations.read_text().splitlines()[-1])
    obj = bus.get_object(item['service'], item['path'])
    props = dbus.Interface(obj, 'org.freedesktop.DBus.Properties')
    assert props.Get('org.kde.StatusNotifierItem', 'Id') == 'gsnag'
    assert len(props.Get('org.kde.StatusNotifierItem', 'IconPixmap')) > 0
    menu_path = props.Get('org.kde.StatusNotifierItem', 'Menu')
    menu = dbus.Interface(bus.get_object(item['service'], str(menu_path)), 'com.canonical.dbusmenu')

    def items():
        layout = menu.GetLayout(0, -1, dbus.Array([], signature='s'))[1]
        return {str(child[1]['label']): (int(child[0]), dict(child[1]))
                for child in layout[2] if 'label' in child[1]}

    def enabled():
        return bool(items()['Fånga region'][1].get('enabled', True))

    def click(label):
        menu.Event(items()[label][0], 'clicked', dbus.Int32(0, variant_level=1), dbus.UInt32(0))

    assert set(items()) == {'Fånga region', 'Fånga hela skrivbordet', 'Avsluta gsnag', 'Spela in video…', 'Pausa inspelning', 'Återuppta inspelning', 'Stoppa och spara', 'Språk'}
    duplicate = subprocess.run([exe, 'tray'], env=env, capture_output=True, timeout=10)
    assert duplicate.returncode == 0
    assert len(registrations.read_text().splitlines()) == 1
    print('PASS: SNI icon/menu and single tray instance', flush=True)

    click('Fånga region')
    wait(lambda: len(child_pids()) == 1)
    capture_pids.update(child_pids())
    wait(lambda: not enabled())
    dbus.Interface(obj, 'org.kde.StatusNotifierItem').Activate(0, 0)
    time.sleep(.5)
    assert len(child_pids()) == 1
    shot('region.png')
    key('Escape')
    wait(enabled)
    assert not child_pids()
    print('PASS: menu launches region, suppresses duplicate capture, cancellation restores menu', flush=True)

    dbus.Interface(obj, 'org.kde.StatusNotifierItem').Activate(0, 0)
    wait(lambda: len(child_pids()) == 1)
    capture_pids.update(child_pids())
    time.sleep(1)
    key('Escape')
    wait(enabled)
    print('PASS: left-click activation launches region capture', flush=True)

    watcher.terminate()
    watcher.wait(timeout=5)
    time.sleep(.6)
    shot('panel-offline.png')
    watcher = spawn(['python3', str(ROOT / 'tests/tray-watcher.py'), str(registrations)], 'watcher-restart')
    wait(lambda: len(registrations.read_text().splitlines()) == 2)
    print('PASS: tray re-registers after panel restart', flush=True)

    click('Fånga hela skrivbordet')
    wait(lambda: len(child_pids()) == 1)
    capture_pids.update(child_pids())
    pid = next(iter(child_pids()))
    assert b'--all' in Path(f'/proc/{pid}/cmdline').read_bytes()
    time.sleep(2)
    shot('desktop-editor.png')
    click('Avsluta gsnag')
    assert tray.wait(timeout=10) == 0
    assert Path(f'/proc/{pid}').exists(), 'Quitting the tray must preserve an open editor'
    print('PASS: desktop capture opens editor; exiting tray preserves editor', flush=True)
finally:
    for pid in capture_pids:
        try:
            os.kill(pid, 15)
        except ProcessLookupError:
            pass
    for child in reversed(children):
        if child.poll() is None:
            child.terminate()
            try:
                child.wait(timeout=5)
            except subprocess.TimeoutExpired:
                child.kill()
                child.wait()
    print('Artifacts:', ARTIFACTS, flush=True)
