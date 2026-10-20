#!/usr/bin/env bash
# Builds the runtime the app provides and copies it into the APK's assets
# (../app/src/main/assets/runtime/, gitignored): the ARM64 Wine 11.0 with Box64's wowbox64.dll for
# its 32-bit x86 code, its prefix template and the x86 .NET 6 runtime of the official PD2 launcher,
# laid out under the Launcher's managed root as the engine names them (pack.py). Every part is
# built in a container (Podman, or rootless Docker) and kept in the cache (PD2_RUNTIME_CACHE,
# default ~/.cache/pd2-android/runtime) with the identity of its inputs, so a part whose inputs did
# not change is used again:
#   wine.sh       Wine (config.sh pins it) for ARM64 in WoW64 mode, with patches/wine
#   wowbox64.sh   Box64's wowbox64.dll (config.sh pins the commit), with patches/box64
#   prefix.sh     the prefix template, made by that Wine (needs the App userland, userland/build.sh,
#                 and qemu-user for aarch64 in binfmt_misc)
#   dotnet.sh     the .NET runtime and Windows Desktop runtime, x86
#   then          trim.py with wine-keep.txt (the modules the game, the official launcher and the
#                 install chain load), stage-wineb.py (the interpreter of Wine's programs is the
#                 userland's loader; wowbox64.dll beside Wine's modules), pack.py without what
#                 runtime-cut.txt names
# `build.sh --version` prints the version the current parts give without building anything (the
# version of the last pack when its inputs did not change).
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=config.sh
. "$here/config.sh"

identity() {
    part_identity "$here/build.sh" "$here/trim.py" "$here/wine-keep.txt" "$here/stage-wineb.py" "$here/pack.py" \
        "$here/runtime-cut.txt" "$here/dotnet.sh" "$here/containers/Containerfile.wine" \
        "$here/containers/Containerfile.tools" "debian=$DEBIAN_IMAGE $DEBIAN_SNAPSHOT" \
        "dotnet=$DOTNET_VERSION $DOTNET_RUNTIME_SHA512 $DOTNET_DESKTOP_SHA512" \
        "wine $(part_hash wine)" "wowbox64 $(part_hash wowbox64)" "prefix $(part_hash prefix)"
}

case "${1:-}" in
    --wine) # in the Wine image
        python3 /src/trim.py /cache/parts/wine/full /src/wine-keep.txt /cache/work/runtime/wine-min
        python3 /src/stage-wineb.py /cache/work/runtime/wine-min /cache/parts/wowbox64/wowbox64.dll \
            /cache/work/runtime/wine "$USERLAND/lib/ld-linux-aarch64.so.1"
        rm -rf /cache/work/runtime/wine-min
        exit 0
        ;;
    --pack) # in the tools image
        rm -rf /cache/work/runtime/assets
        python3 /src/pack.py /cache/work/runtime/wine /cache/parts/prefix/prefix /cache/work/runtime/dotnet \
            /src/runtime-cut.txt /cache/work/runtime/assets
        exit 0
        ;;
    --version)
        cat "$RUNTIME_PARTS/runtime/runtime.info" 2>/dev/null | head -n 1
        exit 0
        ;;
esac

"$here/wine.sh"
"$here/wowbox64.sh"
"$here/prefix.sh"
id="$(identity)"
if part_ready runtime "$id"; then
    echo "runtime: reusing parts/runtime"
else
    rm -rf "$RUNTIME_WORK/runtime" "$RUNTIME_PARTS/runtime"
    mkdir -p "$RUNTIME_WORK/runtime" "$RUNTIME_PARTS/runtime"
    run_in "$(runtime_image wine)" build.sh --wine
    "$here/dotnet.sh"
    run_in "$(runtime_image tools)" build.sh --pack
    mv "$RUNTIME_WORK/runtime/assets/runtime.tar.zst" "$RUNTIME_WORK/runtime/assets/runtime.info" "$RUNTIME_PARTS/runtime/"
    part_done runtime "$id"
    rm -rf "$RUNTIME_WORK/runtime"
fi
mkdir -p "$RUNTIME_ASSETS"
cp "$RUNTIME_PARTS/runtime/runtime.tar.zst" "$RUNTIME_PARTS/runtime/runtime.info" "$RUNTIME_ASSETS/"
echo "runtime: version $(head -n 1 "$RUNTIME_ASSETS/runtime.info"), $(stat -c %s "$RUNTIME_ASSETS/runtime.tar.zst") bytes"
