#!/usr/bin/env python3
"""Check translated UI literals, placeholders, and language preference precedence."""
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
ROOT = Path(__file__).resolve().parents[1]
sv = json.loads((ROOT / 'locales/sv.json').read_text())
de = json.loads((ROOT / 'locales/de.json').read_text())
assert sv.keys() == de.keys()
for key in sv:
    for catalog in (sv, de):
        assert catalog[key].strip(), key
        assert sorted(re.findall(r'\{\w+\}', key)) == sorted(re.findall(r'\{\w+\}', catalog[key])), key
for path in (ROOT / 'crates').glob('*/src/*.rs'):
    source = path.read_text()
    for message in re.findall(r'\b(?:tr|tf)\(\s*"([^"\n]+)"', source):
        assert message in sv, (path, message)
    if path.name == 'ui.rs' and 'gsnag-editor' in str(path):
        for message in re.findall(r'\b(?:button|row)\(\s*"([^"\n]+)"', source):
            assert message in sv, (path, message)
gsnag = os.environ.get('GSNAG', str(ROOT / 'target/debug/gsnag'))
with tempfile.TemporaryDirectory() as config:
    env = dict(os.environ, XDG_CONFIG_HOME=config, LANG='de_DE.UTF-8')
    for key in ['GSNAG_LANGUAGE', 'LC_ALL', 'LC_MESSAGES']:
        env.pop(key, None)
    def language(*args):
        return subprocess.check_output([gsnag, *args], env=env, text=True).strip()
    assert language('language') == 'de'
    assert language('language', 'sv') == 'sv'
    assert language('language') == 'sv'
    env['GSNAG_LANGUAGE'] = 'en'
    assert language('language') == 'en'
    assert language('--language', 'de', 'language') == 'de'
    del env['GSNAG_LANGUAGE']
    assert language('language', 'auto') == 'de'
print(f'PASS: {len(sv)} translated messages, placeholders, preference and overrides')
