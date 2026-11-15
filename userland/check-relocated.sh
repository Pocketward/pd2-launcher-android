#!/usr/bin/env bash
# The relocated runs of check-tree.sh without a container, for an aarch64 host (the release
# workflow's arm64 job, where the base image, an amd64 image, does not run) or an x86 host with
# qemu-aarch64 in binfmt_misc: unpacks the packed App userland (pack.sh's userland.tar.zst and
# the parts' archives over it, as the app does) into a scratch data folder, rewrites the
# placeholder for that folder as the app does (rewrite-placeholder.py), and runs Xvfb -help and
# xkbcomp -version from there with no library environment: the interpreter, the run paths and the
# loader's own folders must all be at the padded path.
#
# Usage: check-relocated.sh [<userland assets> [<part assets>]]
#   default: ../app/src/main/assets/userland and ../app/system-driver/assets (pack.sh copies there)
# Needs bash, python3, tar and zstd.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=config.sh
. "$here/config.sh"

assets="${1:-$USERLAND_ASSETS}"
part_assets="${2:-$USERLAND_PART_ASSETS}"
[[ -f "$assets/userland.tar.zst" && -f "$assets/manifest.json" ]] ||
    { echo "check-relocated: no packed userland in $assets (userland/build.sh)" >&2; exit 1; }
[[ "$(uname -m)" == aarch64 ]] || grep -qsx enabled /proc/sys/fs/binfmt_misc/qemu-aarch64 ||
    { echo "check-relocated: needs an aarch64 host, or qemu-user for aarch64 in binfmt_misc" >&2; exit 1; }

scratch="$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/check-relocated.XXXXXX")"
trap 'rm -rf "$scratch"' EXIT
# A data folder as on the device: the tree in files/userland/<version>, current naming it.
data="$scratch/data"
tree="$data/files/userland/tree"
mkdir -p "$tree"
ln -s tree "$data/files/userland/current"
zstd -dc "$assets/userland.tar.zst" | tar -x -p -C "$tree"
manifests=("$assets/manifest.json")
for d in "$part_assets"/userland-*/; do
    [[ -f "$d/manifest.json" ]] || continue
    zstd -dc "$d/userland.tar.zst" | tar -x -p -C "$tree"
    manifests+=("$d/manifest.json")
    echo "part $(basename "$d") unpacked over the main tree"
done
python3 "$here/rewrite-placeholder.py" "$tree" "$data" "${manifests[@]}"

fail=0
bin="$data/files/userland/current/usr/bin"
# Each: the command, then what its output must hold.
for run in "Xvfb -help|use: X" "xkbcomp -version|xkbcomp"; do
    command="${run%%|*}"
    # Interpreter and libraries only at the padded path: no library environment.
    # shellcheck disable=SC2086 # the command's arguments
    if got="$( (env -i "$bin/${command%% *}" ${command#* } 2>&1 || true) | grep -m1 -E "${run#*|}")"; then
        echo "relocated $command: $got"
    else
        # shellcheck disable=SC2086
        echo "FAIL: relocated $command: $(env -i "$bin/${command%% *}" ${command#* } 2>&1 | head -n 3)" >&2
        fail=1
    fi
done
[[ "$fail" == 0 ]] && echo "check-relocated: OK ($(uname -m))"
exit "$fail"
