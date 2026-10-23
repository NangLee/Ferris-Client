#!/usr/bin/env bash
set -euo pipefail
echo 'build-renderer.sh is retired. Use scripts/build-worker.sh. No Juz source is built or packaged.' >&2
if (( $# > 1 )); then
    echo 'The old source-directory/server-origin arguments are no longer supported.' >&2
    exit 1
fi
if (( $# == 1 )) && [[ "$1" != *.tar.xz ]]; then
    echo 'The optional argument is a new worker output .tar.xz path, not a Juz source directory.' >&2
    exit 1
fi
root=$(cd -- "$(dirname -- "$0")/.." && pwd)
exec bash "$root/scripts/build-worker.sh" "$@"
