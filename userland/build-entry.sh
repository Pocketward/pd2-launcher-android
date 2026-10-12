#!/usr/bin/env bash
# Cross-builds pd2-fex (fex-entry.c, our own code), the entry point of App-provided FEX, against
# the staged sysroot and installs it as usr/bin/pd2-fex in the staging tree. relocate.sh then
# sets its interpreter like every other ELF. The app names it to the Launcher engine in
# PD2_APP_FEX.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=config.sh
. "$here/config.sh"

if [[ "${PD2_USERLAND_IN_CONTAINER:-}" != 1 ]]; then
    run_in_tools build-entry.sh "$@"
    exit
fi

work=/cache/work
sysroot="$work/sysroot"
[[ -d "$sysroot/usr/include" ]] || { echo "no sysroot: run fetch-debian.sh first" >&2; exit 1; }
out="$work/entry-build"
rm -rf "$out" && mkdir -p "$out"
aarch64-linux-gnu-gcc --sysroot="$sysroot" -O2 -Wall -Wextra -Werror \
    -DFEX_PATH="\"$USERLAND/usr/bin/FEX\"" -o "$out/pd2-fex" "$here/fex-entry.c"
aarch64-linux-gnu-strip "$out/pd2-fex"
install -D -m 0755 "$out/pd2-fex" "$work/staging/usr/bin/pd2-fex"
echo "pd2-fex installed into staging (execs $USERLAND/usr/bin/FEX)"
