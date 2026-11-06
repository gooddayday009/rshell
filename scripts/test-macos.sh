#!/bin/bash
set -euo pipefail
cd "$(dirname "$0")/.."
[[ "$(uname -s)" == Darwin ]] || { echo 'Run this script on macOS.' >&2; exit 1; }
for option in "$@"; do
  case "$option" in
    --credentials|--smoke) ;;
    *) echo 'Usage: bash scripts/test-macos.sh [--credentials] [--smoke]' >&2; exit 1 ;;
  esac
done
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace --all-targets --no-fail-fast
for option in "$@"; do
case "$option" in
  --credentials)
    cargo test --locked -p rshell-core encrypted_credentials_roundtrip -- --ignored
    cargo test --locked -p rshell-core managed_keys_roundtrip -- --ignored
    cargo test --locked -p rshell-core encrypted_history_survives_replacement_without_plaintext -- --ignored
    cargo test --locked -p rshell-core jumpserver_credentials_roundtrip -- --ignored
    ;;
  --smoke)
    cargo build --locked -p rshell
    python3 - <<'PY'
import os
from pathlib import Path
import subprocess
import tempfile
import time

root = Path.cwd()
artifacts = root / 'target' / 'test-artifacts'
artifacts.mkdir(parents=True, exist_ok=True)
binary = Path(os.environ.get('CARGO_TARGET_DIR', root / 'target')) / 'debug' / 'rshell'
config = tempfile.mkdtemp(prefix='macos-smoke-', dir=artifacts)
modes = ['demo', 'tabs-fit', 'tabs-overflow', 'split', 'local', 'serial',
         'ai-settings', 'ai-chat', 'ai-chat-long', 'ai-chat-maximized',
         'ai-chat-font-large', 'appearance', 'session', 'session-options',
         'sftp', 'import', 'telnet', 'host-keys', 'user-keys', 'hidden-chrome',
         'history-settings', 'tunnels', 'tunnels-light', 'tunnel-status']
for mode in modes:
    args = [str(binary), '--smoke-test']
    if mode not in ('local', 'ai-settings'):
        args.append('--demo')
    if mode.startswith('ai-chat'):
        args.append('--smoke-ai-chat')
        if mode != 'ai-chat':
            args.append('--smoke-ai-long-draft')
        if mode == 'ai-chat-maximized':
            args.append('--smoke-ai-maximized')
        if mode == 'ai-chat-font-large':
            args.append('--smoke-ai-font-large')
    elif mode in ('tunnels', 'tunnels-light'):
        args.append('--smoke-tunnels')
        if mode == 'tunnels-light':
            args.append('--smoke-light')
    elif mode == 'ai-settings':
        args.append('--ai-settings')
    elif mode not in ('demo', 'local'):
        args.append('--smoke-' + mode)
    screenshot = artifacts / ('macos-smoke-' + mode + '.png')
    env = dict(os.environ, RSHELL_CONFIG_DIR=config, RSHELL_SCREENSHOT=str(screenshot))
    started = time.time()
    subprocess.run(args, env=env, check=True, timeout=30)
    if not screenshot.is_file() or screenshot.stat().st_mtime < started:
        raise RuntimeError('Missing or stale screenshot: ' + str(screenshot))
    print(screenshot)
PY
    ;;
esac
done
