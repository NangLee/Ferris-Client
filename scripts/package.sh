#!/usr/bin/env bash
set -euo pipefail
root=$(cd -- "$(dirname -- "$0")/.." && pwd)
if [[ $(uname -s) != Linux ]]; then echo 'Use package.ps1 on Windows.' >&2; exit 1; fi
if [[ -z ${FERRIS_UPDATE_PUBLIC_KEY:-} && -f "$root/public-signing-key.txt" ]]; then
    FERRIS_UPDATE_PUBLIC_KEY=$(tr -d '\r\n' < "$root/public-signing-key.txt")
    export FERRIS_UPDATE_PUBLIC_KEY
fi
: "${FERRIS_UPDATE_PUBLIC_KEY:?Set the public signing key before packaging}"
for required in cargo cargo-packager python3; do
    if ! command -v "$required" >/dev/null; then
        echo "Missing $required. Install the build dependencies and run: cargo install cargo-packager --version 0.11.8 --locked" >&2
        exit 1
    fi
done
bash "$root/scripts/build.sh"
mkdir -p -- "$root/dist"
config=$(mktemp "$root/dist/packager.XXXXXX.json")
trap 'rm -f -- "$config"' EXIT
export FERRIS_PACKAGE_ROOT="$root"
cargo metadata --manifest-path "$root/Cargo.toml" --locked --no-deps --format-version 1 | python3 -c '
import json, os, sys
metadata = json.load(sys.stdin)
package = next(item for item in metadata["packages"] if item["name"] == "ferris-client")
root = os.environ["FERRIS_PACKAGE_ROOT"]
config = {
    "name": "ferris-client",
    "productName": "Ferris Client", "version": package["version"],
    "identifier": "win.ferrisfun.client", "publisher": "Ferris", "category": "Utility",
    "description": "Contribute your computer to the Ferris osu!taiko replay render queue",
    "formats": ["deb", "appimage"], "outDir": root + "/dist", "binariesDir": root + "/target/release",
    "binaries": [{"path": "ferris-client", "main": True}],
    "icons": [root + "/assets/icon.png"],
    "resources": [root + "/THIRD_PARTY.md", root + "/LICENSE.md"],
    "deb": {"packageName": "ferris-client", "depends": ["libc6", "libgcc-s1", "libgl1", "libx11-6", "libxkbcommon0", "libdbus-1-3", "xdg-utils"]}
}
json.dump(config, sys.stdout, indent=2)
' > "$config"
cd -- "$root"
export APPIMAGE_EXTRACT_AND_RUN=1
cargo packager --release --config "$config"
printf 'Packages are in %s/dist\n' "$root"
