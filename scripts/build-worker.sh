#!/usr/bin/env bash
set -euo pipefail
root=$(cd -- "$(dirname -- "$0")/.." && pwd)
if [[ $(uname -s) != Linux ]]; then echo 'Use build-worker.ps1 on Windows.' >&2; exit 1; fi
case $(uname -m) in
    x86_64) platform=linux-x86_64; expected_host=x86_64-unknown-linux-gnu ;;
    aarch64|arm64) platform=linux-aarch64; expected_host=aarch64-unknown-linux-gnu ;;
    *) echo 'Native Linux x86-64 and ARM64 are supported.' >&2; exit 1 ;;
esac
for required in cargo rustc python3 tar xz sha256sum sed realpath install stat; do
    command -v "$required" >/dev/null || { echo "Missing $required" >&2; exit 1; }
done
host=$(rustc -vV | sed -n 's/^host: //p')
[[ "$host" == "$expected_host" ]] || { echo "Use the native GNU Rust toolchain for $platform." >&2; exit 1; }
version=$(python3 - "$root/Cargo.toml" <<'PY'
import pathlib, re, sys
match = re.search(r'^version\s*=\s*"([0-9A-Za-z.+_-]+)"', pathlib.Path(sys.argv[1]).read_text(), re.M)
if match is None: raise SystemExit('Could not read the client version')
print(match.group(1))
PY
)
target="$root/target/graphics-worker"
cargo build --manifest-path "$root/crates/ferris-graphics/Cargo.toml" --release --locked --bin ferris-worker --target-dir "$target"
mkdir -p -- "$root/dist"
output=${1:-"$root/dist/ferris-worker-$version-$platform.tar.xz"}
output=$(realpath -m -- "$output")
[[ "$output" == *.tar.xz ]] || { echo 'Linux worker output must end with .tar.xz.' >&2; exit 1; }
[[ ! -e "$output" ]] || { echo "Output already exists: $output" >&2; exit 1; }
mkdir -p -- "$(dirname -- "$output")"
stage=$(mktemp -d "$root/dist/.worker.XXXXXX")
trap 'rm -rf -- "$stage"' EXIT
install -m 755 -- "$target/release/ferris-worker" "$stage/ferris-worker"
cp -- "$root/LICENSE.md" "$root/THIRD_PARTY.md" "$stage/"
tar --owner=0 --group=0 --numeric-owner -cJf "$output" -C "$stage" ferris-worker LICENSE.md THIRD_PARTY.md
hash=$(sha256sum -- "$output"); hash=${hash%% *}
bytes=$(stat -c %s -- "$output")
printf '{"sha256":"%s","bytes":%s,"archive":"tar.xz","executable":"ferris-worker"}\n' "$hash" "$bytes" > "$output.artifact.json"
printf '%s\n' "$output"
cat -- "$output.artifact.json"
