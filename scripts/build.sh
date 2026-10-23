#!/usr/bin/env bash
set -euo pipefail
root=$(cd -- "$(dirname -- "$0")/.." && pwd)
if [[ -z ${FERRIS_UPDATE_PUBLIC_KEY:-} && -f "$root/public-signing-key.txt" ]]; then
    FERRIS_UPDATE_PUBLIC_KEY=$(tr -d '\r\n' < "$root/public-signing-key.txt")
    export FERRIS_UPDATE_PUBLIC_KEY
fi
: "${FERRIS_UPDATE_PUBLIC_KEY:?Set FERRIS_UPDATE_PUBLIC_KEY to the public signing key from Ferris admin}"
if ! command -v cargo >/dev/null; then echo 'Install Rust stable from https://rustup.rs first.' >&2; exit 1; fi
if ! command -v python3 >/dev/null; then echo 'Install python3 to validate the public key.' >&2; exit 1; fi
python3 - <<'PY'
import base64, os
value = os.environ['FERRIS_UPDATE_PUBLIC_KEY'].strip()
try:
    key = base64.b64decode(value + '=' * (-len(value) % 4), altchars=b'-_', validate=True)
except Exception as error:
    raise SystemExit('FERRIS_UPDATE_PUBLIC_KEY must be a base64 Ed25519 public key') from error
if len(key) != 32:
    raise SystemExit('FERRIS_UPDATE_PUBLIC_KEY must contain exactly 32 bytes')
PY
cd -- "$root"
cargo build --release --locked --bin ferris-client
printf 'Release binary: %s/target/release/ferris-client\n' "$root"
