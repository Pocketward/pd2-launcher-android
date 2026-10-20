#!/usr/bin/env bash
# Box64's wowbox64.dll, the x86 CPU DLL of the ARM64 Wine (WoW64): Box64 at the commit
# config.sh pins, built as Box64's release workflow builds its WOW64 configuration, with
# llvm-mingw in the Wine build image, with our patches (patches/box64).
# Output: parts/wowbox64/wowbox64.dll (stripped).
#
# Usage: wowbox64.sh [--identity]
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=config.sh
. "$here/config.sh"

identity() {
    part_identity "box64=$BOX64_COMMIT $BOX64_SHA256 $BOX64_DATE_EPOCH" "llvm-mingw=$LLVM_MINGW_NAME $LLVM_MINGW_SHA256" \
        "debian=$DEBIAN_IMAGE $DEBIAN_SNAPSHOT" \
        "$here/wowbox64.sh" "$RUNTIME_DIR/containers/Containerfile.wine" "$RUNTIME_DIR"/patches/box64/*.patch
}

if [[ "${1:-}" == --identity ]]; then
    identity
    exit 0
fi

if ! in_container; then
    id="$(identity)"
    if part_ready wowbox64 "$id"; then
        echo "wowbox64: reusing parts/wowbox64"
        exit 0
    fi
    fetch "$BOX64_URL" "$BOX64_SHA256" "box64-$BOX64_COMMIT.tar.gz" >/dev/null
    fetch "$LLVM_MINGW_URL" "$LLVM_MINGW_SHA256" >/dev/null
    rm -rf "$RUNTIME_PARTS/wowbox64"
    mkdir -p "$RUNTIME_PARTS/wowbox64"
    # Box64 shows its build date and time (__DATE__, __TIME__): a fixed one (the commit's day)
    # keeps the DLL the same in every build.
    # shellcheck disable=SC2034 # read by run_in (config.sh)
    RUN_EXTRA=(-e SOURCE_DATE_EPOCH="$BOX64_DATE_EPOCH")
    run_in "$(runtime_image wine)" wowbox64.sh
    part_done wowbox64 "$id"
    exit 0
fi

mingw="$(llvm_mingw)"
export PATH="$mingw/bin:$PATH"
src=/cache/work/box64-src
build=/cache/work/wowbox64-build
rm -rf "$src" "$build"
mkdir -p "$src" "$build"
tar -xzf "/cache/dl/box64-$BOX64_COMMIT.tar.gz" -C "$src" --strip-components=1
for p in /src/patches/box64/*.patch; do
    patch -d "$src" -p1 --fuzz=0 --no-backup-if-mismatch --quiet < "$p"
    echo "applied $(basename "$p")"
done
cmake -S "$src" -B "$build" -DWOW64=1 -DARM_DYNAREC=1 -DNOGIT=1 -DCMAKE_BUILD_TYPE=RelWithDebInfo \
    -DCMAKE_C_COMPILER=aarch64-linux-gnu-gcc -DCMAKE_ASM_COMPILER=aarch64-linux-gnu-gcc > "$build/cmake.log"
make -C "$build" -j"$(nproc)" wowbox64 > "$build/make.log" 2>&1 || { tail -40 "$build/make.log"; exit 1; }
f="$(find "$build" -name wowbox64.dll | head -1)"
llvm-strip -o /cache/parts/wowbox64/wowbox64.dll "$f"
ls -l /cache/parts/wowbox64/wowbox64.dll
rm -rf "$src" "$build"
