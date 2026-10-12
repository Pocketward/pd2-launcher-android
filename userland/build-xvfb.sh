#!/usr/bin/env bash
# Cross-builds Xvfb from the pinned X server release for aarch64 against the staged sysroot and
# installs it into the staging tree. Not taken from Debian's xvfb package because its XKB paths
# are compile-time only (no runtime knob):
#   xkbcomp location   XKB_BIN_DIRECTORY   xkb/xkbInit.c:91, meson option xkb_bin_dir
#   XKM output dir     XKM_OUTPUT_DIR      xkb/ddxLoad.c:63-87 (falls back to /tmp/), xkb_output_dir
#   XKB data           XKB_BASE_DIRECTORY  xkb/xkbInit.c:90 (also -xkbdir at run time), xkb_dir
#   GLX swrast driver  DRI_DRIVER_PATH     include/meson.build:215 (from dri.pc dridriverdir)
# and Android needs two source changes (patches/xserver/): -nolock for every uid (the lock file
# needs /tmp and a hard link, os/utils.c LockServer) and a build-time shell for xkbcomp.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=config.sh
. "$here/config.sh"

if [[ "${PD2_USERLAND_IN_CONTAINER:-}" != 1 ]]; then
    run_in_tools build-xvfb.sh "$@"
    exit
fi

work=/cache/work
sysroot="$work/sysroot"
build="$work/xvfb-build"
dest="$work/xvfb-dest"
[[ -d "$sysroot/usr/include" ]] || { echo "no sysroot: run fetch-debian.sh first" >&2; exit 1; }

tarball="$(fetch_source "$XSERVER_URL" "$XSERVER_SHA256")"
rm -rf "$build" "$dest"
mkdir -p "$build"
tar -xJf "$tarball" -C "$build"
src="$build/xorg-server-$XSERVER_VERSION"
for p in "$here"/patches/xserver/*.patch; do
    patch -d "$src" -p1 --forward --quiet < "$p"
    echo "applied $(basename "$p")"
done

write_cross_file "$build/cross.ini" "$sysroot" "-DXSERVER_SHELL=\"$ANDROID_SHELL\""

meson setup "$build/out" "$src" --cross-file "$build/cross.ini" \
    --prefix="$USERLAND/usr" --libdir=lib/aarch64-linux-gnu \
    --buildtype=release -Db_ndebug=true \
    -Dxvfb=true -Dxorg=false -Dxephyr=false -Dxnest=false -Dxwin=false -Dxquartz=false \
    -Dglx=true -Dglamor=false \
    -Ddri1=false -Ddri2=false -Ddri3=false -Ddrm=false \
    -Dxdmcp=false -Dxdm-auth-1=false -Dsecure-rpc=false \
    -Dudev=false -Dudev_kms=false -Dhal=false -Dsystemd_logind=false \
    -Dpciaccess=false -Dint10=false -Dvgahw=false -Dlinux_apm=false -Dlinux_acpi=false \
    -Dxselinux=false -Dlibunwind=false -Dsha1=libmd \
    -Ddocs=false -Ddevel-docs=false -Ddocs-pdf=false \
    -Ddefault_font_path=built-ins \
    -Dxkb_dir="$USERLAND/usr/share/X11/xkb" \
    -Dxkb_bin_dir="$USERLAND/usr/bin" \
    -Dxkb_output_dir="$XKM_OUTPUT_DIR"

# The server's GLX loads swrast_dri.so from DRI_DRIVER_PATH, taken from dri.pc's dridriverdir;
# pkgconf prefixes that variable with the sysroot, so set the device path in the generated header.
sed -i "s|^#define DRI_DRIVER_PATH .*|#define DRI_DRIVER_PATH \"$USERLAND/usr/lib/aarch64-linux-gnu/dri\"|" \
    "$build/out/include/dix-config.h"

# Fail early if a compiled-in path is not the device path.
for want in "XKB_BIN_DIRECTORY \"$USERLAND/usr/bin\"" \
            "XKB_BASE_DIRECTORY \"$USERLAND/usr/share/X11/xkb\"" \
            "XKM_OUTPUT_DIR \"$XKM_OUTPUT_DIR/\"" \
            "DRI_DRIVER_PATH \"$USERLAND/usr/lib/aarch64-linux-gnu/dri\""; do
    grep -rqF "#define $want" "$build/out/include" || { echo "compiled-in path wrong: $want" >&2; grep -rn "${want%% *}" "$build/out/include" >&2; exit 1; }
done

ninja -C "$build/out" -j"$(nproc)"
DESTDIR="$dest" meson install -C "$build/out" --strip --no-rebuild >/dev/null

staging="$work/staging"
install -D -m 0755 "$dest$USERLAND/usr/bin/Xvfb" "$staging/usr/bin/Xvfb"
# protocol.txt: request/error names for the server's log messages (SERVER_MISC_CONFIG_PATH).
if [[ -f "$dest$USERLAND/usr/lib/xorg/protocol.txt" ]]; then
    install -D -m 0644 "$dest$USERLAND/usr/lib/xorg/protocol.txt" "$staging/usr/lib/xorg/protocol.txt"
fi
aarch64-linux-gnu-readelf -d "$staging/usr/bin/Xvfb" | sed -n 's/.*Shared library: \[\(.*\)\]/Xvfb needs \1/p'
echo "Xvfb $XSERVER_VERSION installed into staging"
