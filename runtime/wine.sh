#!/usr/bin/env bash
# Wine (config.sh pins the release) for ARM64 in WoW64 mode: aarch64 Unix and PE sides and an
# i386 PE side, whose 32-bit x86 code wowbox64.dll (wowbox64.sh) emulates. First Wine's build
# tools for amd64 (make __tooldeps__), then the cross build with llvm-mingw for the PE parts, with
# our patches (patches/wine). Only the Linux libraries the game and the official launcher need
# are on: X11 (Xrender, Xrandr, XInput2, Xcursor, Xfixes, Xcomposite, Xinerama), OpenGL (EGL and
# GLX), Vulkan, FreeType and fontconfig, ALSA (winealsa.drv: the game's sound to the app,
# pd2audio.sh), GnuTLS; no sound server, device, network-auth, media, printing or scanner libraries.
# Output: parts/wine/full (the whole install below WINE_PREFIX_DIR, stripped); trim.py makes the
# reduced Wine of the APK from it.
#
# Usage: wine.sh [--identity]
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=config.sh
. "$here/config.sh"

identity() {
    part_identity "wine=$WINE_VERSION $WINE_SHA256" "llvm-mingw=$LLVM_MINGW_NAME $LLVM_MINGW_SHA256" \
        "debian=$DEBIAN_IMAGE $DEBIAN_SNAPSHOT" "prefix=$WINE_PREFIX_DIR" \
        "$here/wine.sh" "$RUNTIME_DIR/containers/Containerfile.wine" "$RUNTIME_DIR"/patches/wine/*.patch
}

if [[ "${1:-}" == --identity ]]; then
    identity
    exit 0
fi

if ! in_container; then
    id="$(identity)"
    if part_ready wine "$id"; then
        echo "wine: reusing parts/wine"
        exit 0
    fi
    fetch "$WINE_URL" "$WINE_SHA256" >/dev/null
    fetch "$LLVM_MINGW_URL" "$LLVM_MINGW_SHA256" >/dev/null
    rm -rf "$RUNTIME_PARTS/wine"
    mkdir -p "$RUNTIME_PARTS/wine"
    run_in "$(runtime_image wine)" wine.sh
    part_done wine "$id"
    exit 0
fi

src=/cache/work/wine-src
tools=/cache/work/wine-tools
build=/cache/work/wine-build
out=/cache/parts/wine
rm -rf "$src" "$tools" "$build"
mkdir -p "$src" "$tools" "$build"
tar -xJf "/cache/dl/$(basename "$WINE_URL")" -C "$src" --strip-components=1
for p in /src/patches/wine/*.patch; do
    patch -d "$src" -p1 --fuzz=0 --quiet < "$p"
    echo "applied $(basename "$p")"
done
mingw="$(llvm_mingw)"
export PATH="$mingw/bin:$PATH"

echo "== Wine's build tools (amd64)"
cd "$tools"
"$src/configure" --enable-win64 --without-x --without-mingw --disable-tests > configure.log 2>&1 || { tail -40 configure.log; exit 1; }
make -j"$(nproc)" __tooldeps__ > make.log 2>&1 || { tail -60 make.log; exit 1; }
[[ -x tools/sfnt2fon/sfnt2fon ]] || { echo "no sfnt2fon: the bitmap fonts would be left out" >&2; exit 1; }

echo "== Wine for ARM64 (aarch64 + i386 PE)"
cd "$build"
PKG_CONFIG_LIBDIR=/usr/lib/aarch64-linux-gnu/pkgconfig:/usr/share/pkgconfig \
"$src/configure" --prefix="$WINE_PREFIX_DIR" --host=aarch64-linux-gnu --enable-archs=aarch64,i386 \
    --with-wine-tools="$tools" --disable-tests --disable-win16 \
    --with-x --with-opengl --with-vulkan --with-freetype --with-fontconfig --with-alsa --with-gnutls \
    --with-xinput2 --with-xrandr --with-xrender --with-xcursor --with-xfixes --with-xcomposite --with-xinerama \
    --without-capi --without-cups --without-dbus --without-ffmpeg --without-gettext --without-pulse \
    --without-gphoto --without-gssapi --without-gstreamer --without-krb5 \
    --without-netapi --without-opencl --without-oss --without-pcap --without-pcsclite --without-sane \
    --without-sdl --without-udev --without-unwind --without-usb --without-v4l2 --without-wayland \
    --without-xshm --without-xxf86vm --without-xinput --without-xshape --without-inotify \
    > configure.log 2>&1 || { tail -40 configure.log; exit 1; }
grep -E '^configure: (WARNING|error)' configure.log || true
make -j"$(nproc)" > make.log 2>&1 || { tail -60 make.log; exit 1; }
rm -rf "$out/full" "$out/install"
make install-lib DESTDIR="$out/install" > install.log 2>&1 || { tail -40 install.log; exit 1; }
mv "$out/install$WINE_PREFIX_DIR" "$out/full"
rm -rf "$out/install"
cd "$out/full"
find . -name '*.so' -exec aarch64-linux-gnu-strip --strip-unneeded {} +
aarch64-linux-gnu-strip --strip-unneeded bin/wine bin/wineserver 2>/dev/null || true
find lib/wine/aarch64-windows lib/wine/i386-windows -type f -exec llvm-strip --strip-unneeded {} + 2>/dev/null || true
du -sh lib/wine/*
rm -rf "$src" "$tools" "$build"
