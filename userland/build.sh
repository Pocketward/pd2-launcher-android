#!/usr/bin/env bash
# Builds the App userland end to end, every step in a container (see docs/BUILDING.md):
#   fetch-debian.sh   pinned Debian 13 arm64 packages -> staging tree and cross-build sysroot
#   build-xvfb.sh     Xvfb from xorg-server 21.1.25 with the prefix's compiled-in paths
#   build-mesa.sh     Mesa 26.2.3: Zink, softpipe, GLX and EGL for glvnd, Turnip (KGSL) and its ICD JSON
#   build-panvk.sh    Mesa's PanVK with our kbase backend (Mali) and its ICD JSON (PD2_PANVK_STAGE=1)
#   build-glibc.sh    only with PD2_USERLAND_WITH_GLIBC=1: our glibc (glibc/) replaces Debian's libc6
#   build-relay.sh    x-input-relay, touch and keyboard into the X screen (x-input-relay.c, our code)
#   relocate.sh       interpreter, run paths, links, etc/ for the prefix
#   pack.sh           out/userland.tar.zst + out/manifest.json (with where the placeholder is),
#                     copied into the APK's assets
#   check-tree.sh     verifies the packed archive
# Pass --resolve to re-resolve packages.txt into packages.lock first.
# Every program is built for the placeholder prefix in config.sh; the app rewrites it to its own
# data folder as it unpacks the tree, so the release app and the development build share it.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

if [[ "${PD2_USERLAND_IN_CONTAINER:-}" == 1 ]]; then
    echo "refusing: build.sh runs on the host and starts the containers itself" >&2
    exit 1
fi

# shellcheck source=config.sh
. "$here/config.sh"
echo "App userland for the prefix $USERLAND"
step() { echo "== $1"; "$here/$1" "${@:2}"; }

step fetch-debian.sh "$@"
step build-xvfb.sh
step build-mesa.sh
PD2_PANVK_STAGE=1 step build-panvk.sh
[[ "${PD2_USERLAND_WITH_GLIBC:-0}" == 1 ]] && step build-glibc.sh
step build-relay.sh
step relocate.sh
step pack.sh
step check-tree.sh
