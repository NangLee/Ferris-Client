#!/usr/bin/env bash
set -euo pipefail
root=$(cd -- "$(dirname -- "$0")/.." && pwd)
if [[ -z ${FERRIS_UPDATE_PUBLIC_KEY:-} && -f "$root/public-signing-key.txt" ]]; then
    FERRIS_UPDATE_PUBLIC_KEY=$(tr -d '\r\n' < "$root/public-signing-key.txt")
    export FERRIS_UPDATE_PUBLIC_KEY
fi
if ! command -v cargo >/dev/null; then echo 'Install Rust stable from https://rustup.rs first.' >&2; exit 1; fi
if [[ -z ${FERRIS_UPDATE_PUBLIC_KEY:-} ]]; then
    echo 'Development build without a pinned key: signed runtime installation stays unavailable until rebuilt with FERRIS_UPDATE_PUBLIC_KEY.' >&2
fi
cd -- "$root"
exec cargo run --locked --bin ferris-client -- "$@"
