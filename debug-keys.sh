#!/usr/bin/env bash
# Pre-fills the development build's key screen: writes D2_KEY and D2_LOD_KEY from
# the developer's .env.local into the private storage of the development build on the device adb
# reaches (com.pd2launcher.app.dev, files/debug-keys.properties, through run-as), where only a
# debug build reads it (app: DebugKeys.kt). No APK carries the keys. They go to the device on
# standard input, never on a command line, and are never printed.
# The file is PD2_DEBUG_KEYS_FILE, else the first .env.local of: this folder; where this folder is
# inside a larger Git checkout, that checkout's root, then its main checkout's root (for a linked
# worktree). With several devices attached, adb's own choice of device applies. `debug-keys.sh
# --remove` deletes the file.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
package=com.pd2launcher.app.dev
file=files/debug-keys.properties
die() { printf 'debug-keys.sh: %s\n' "$*" >&2; exit 1; }

command -v adb >/dev/null || die "adb is not installed"
if [[ "${1:-}" == --remove ]]; then
    adb shell "run-as $package rm -f $file"
    echo "removed $file of $package"
    exit 0
fi
[[ $# == 0 ]] || die "usage: debug-keys.sh [--remove]"

keys_file="${PD2_DEBUG_KEYS_FILE:-$here/.env.local}"
if [[ -z "${PD2_DEBUG_KEYS_FILE:-}" && ! -f "$keys_file" ]] &&
    top="$(git -C "$here" rev-parse --show-toplevel 2>/dev/null)" && [[ "$top" != "$(cd "$here" && pwd -P)" ]]; then
    common="$(git -C "$here" rev-parse --path-format=absolute --git-common-dir)"
    keys_file="$top/.env.local"
    [[ -f "$keys_file" ]] || keys_file="$(dirname "$common")/.env.local"
fi
[[ -f "$keys_file" ]] || die "no .env.local (PD2_DEBUG_KEYS_FILE names another file)"
# Only those two lines are read, and only into this shell's variables.
d2="$(sed -n 's/^D2_KEY=//p' "$keys_file" | head -n 1 | tr -d "\"' \r")"
lod="$(sed -n 's/^D2_LOD_KEY=//p' "$keys_file" | head -n 1 | tr -d "\"' \r")"
[[ "$d2" =~ ^[A-Za-z0-9-]+$ && "$lod" =~ ^[A-Za-z0-9-]+$ ]] || die "$keys_file has no D2_KEY and D2_LOD_KEY lines with CD keys"
# printf is the shell's own: the keys reach no other program's arguments, only adb's standard input.
printf 'D2_KEY=%s\nD2_LOD_KEY=%s\n' "$d2" "$lod" |
    adb shell "run-as $package sh -c 'umask 077 && mkdir -p files && cat > $file'"
echo "debug keys from $keys_file written to $file of $package"
