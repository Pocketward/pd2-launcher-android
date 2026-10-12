#!/usr/bin/env bash
# Cross-builds x-input-relay (x-input-relay.c, our own code) against the staged sysroot and
# installs it as usr/bin/x-input-relay in the staging tree: the app's touch and keyboard input
# into the X screen through XTEST. relocate.sh then sets its
# interpreter like every other ELF; libXtst, libXext and libX11 come from packages.txt.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=config.sh
. "$here/config.sh"

if [[ "${PD2_USERLAND_IN_CONTAINER:-}" != 1 ]]; then
    run_in_tools build-relay.sh "$@"
    exit
fi

work=/cache/work
sysroot="$work/sysroot"
[[ -f "$sysroot/usr/include/X11/extensions/XTest.h" ]] || { echo "no libXtst headers in the sysroot: run fetch-debian.sh first" >&2; exit 1; }
out="$work/relay-build"
rm -rf "$out" && mkdir -p "$out"
aarch64-linux-gnu-gcc --sysroot="$sysroot" -O2 -Wall -Wextra -Werror \
    -o "$out/x-input-relay" "$here/x-input-relay.c" -lXtst -lXext -lX11
aarch64-linux-gnu-strip "$out/x-input-relay"
install -D -m 0755 "$out/x-input-relay" "$work/staging/usr/bin/x-input-relay"
echo "x-input-relay installed into staging"
