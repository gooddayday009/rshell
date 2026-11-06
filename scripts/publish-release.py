"""Publish only a complete, checksum-verified multi-platform artifact set."""
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import urllib.parse
import urllib.request

repo = os.environ['GH_REPO']
tag = os.environ['RELEASE_TAG']
assert re.fullmatch(r'v\d+\.\d+\.\d+', tag), 'Invalid release tag'
version = tag[1:]
manifest = Path('Cargo.toml').read_text()
assert re.search(r'^version = "' + re.escape(version) + '"$', manifest, re.M)
files = {}
for suffix in ['macos-arm64.zip', 'macos-x64.zip', 'windows-x64-setup.exe', 'windows-x64-portable.zip']:
    name = f'RShell-{version}-{suffix}'
    data = (Path('dist') / name).read_bytes()
    check = (Path('dist') / (name + '.sha256')).read_bytes()
    fields = check.decode().strip().split()
    assert fields == [hashlib.sha256(data).hexdigest(), name] or fields == [hashlib.sha256(data).hexdigest().upper(), name], name
    files[name] = data
    files[name + '.sha256'] = check
assert {p.name for p in Path('dist').iterdir()} == set(files), 'Unexpected release files'
base = 'https://api.github.com/repos/' + repo
headers = {'Authorization': 'Bearer ' + os.environ['GH_TOKEN'], 'Accept': 'application/vnd.github+json', 'User-Agent': 'RShell-Release'}
def api(path, method='GET', body=None, raw=None):
    url = path if path.startswith('https://uploads.github.com/') else base + path
    h = dict(headers)
    if raw is not None:
        data = raw
        h['Content-Type'] = 'application/octet-stream'
    elif body is not None:
        data = json.dumps(body).encode()
        h['Content-Type'] = 'application/json'
    else:
        data = None
    with urllib.request.urlopen(urllib.request.Request(url, data=data, headers=h, method=method), timeout=180) as response:
        return json.load(response)
releases = api('/releases?per_page=100')
release = next((r for r in releases if r['tag_name'] == tag), None)
notes_path = Path('docs/releases') / (version + '.md')
notes = notes_path.read_text() if notes_path.exists() else f'RShell {version}: Windows x64 and macOS arm64/x64 packages with SHA-256 checksums.'
if release is None:
    release = api('/releases', 'POST', {'tag_name': tag, 'target_commitish': subprocess.check_output(['git','rev-parse','HEAD'],text=True).strip(), 'name': 'RShell ' + version, 'body': notes, 'draft': True, 'prerelease': False})
assert release['draft'], 'Never overwrite a published release'
existing = {a['name']: a for a in api('/releases/' + str(release['id']) + '/assets')}
assert set(existing).issubset(files), 'Unexpected draft assets'
for name, data in files.items():
    asset = existing.get(name)
    if asset is None:
        asset = api(release['upload_url'].split('{')[0] + '?name=' + urllib.parse.quote(name), 'POST', raw=data)
    assert asset['size'] == len(data) and asset.get('digest') == 'sha256:' + hashlib.sha256(data).hexdigest(), name
    print('Verified:', name, flush=True)
assets = api('/releases/' + str(release['id']) + '/assets')
assert len(assets) == 8 and {a['name'] for a in assets} == set(files)
release = api('/releases/' + str(release['id']), 'PATCH', {'body': notes, 'draft': False, 'prerelease': False, 'make_latest': 'true'})
assert not release['draft'] and not release['prerelease']
print('Published:', release['html_url'])
