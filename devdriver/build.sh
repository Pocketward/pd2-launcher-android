#!/usr/bin/env bash
# Builds the system-driver bridge's servers for Android (bionic, NDK r27c, API 30) in the NDK
# container, two builds of one virglrenderer source:
#   1. Venus (the Venus route): virglrenderer's vtest server and render server with Venus only
#      (-Dvrend=false: no OpenGL renderer, no libepoxy), plus our patches/virglrenderer/*.patch.
#   2. VirGL (the VirGL route, OpenGL to the phone's GLES): a second vtest server with virglrenderer's
#      OpenGL renderer (vrend) only (-Dvenus=false -Dvrend=true -Dplatforms=egl), on libepoxy
#      (static, EGL only, no GLX and no X11), with patches/virglrenderer/0001 (os_misc.c with the
#      NDK) and our patches/virglrenderer-gl/*.patch. vrend runs in the vtest server's child for
#      each client; it needs the phone's GLES 3.0 or later with EGL_KHR_surfaceless_context and
#      EGL_KHR_create_context. A server of its own, so the Venus route's server stays as it is.
# Output: <out>/virgl_test_server, <out>/virgl_render_server, <out>/virgl_test_server_gl,
# <out>/SHA256SUMS, <out>/BUILDINFO
# (the build's identity: the pins, the build type, the SHA-256 of this script, the Containerfiles
# and every patch; the app's build.sh builds again when it differs from `build.sh --identity`).
#
# Usage: build.sh <out-dir> | build.sh --identity
#   sources are cached in $PD2_DEVDRIVER_SRC (default devdriver/src in the build cache), the
#   build tree in $PD2_DEVDRIVER_WORK (default devdriver/build there).
# The build image is devdriver/Containerfile on top of the Android build image that the app's
# build.sh builds (localhost/pd2-android-build:<hash>); run that once first.
#
# Pins:
#   virglrenderer main e18f8c00a9969d41f1cde6b2c7a73af8f518e660 (2026-10-20), MIT; main, not the
#     1.3.0 tag: it has -Dvrend=false (06960f84) and speaks Venus protocol spec 4 as our Venus
#     ICD does (1.3.0: spec 3)
#   venus-protocol v1.1.3 (the subproject that virglrenderer main's wrap names), MIT; our Venus ICD
#     (Mesa main, userland/build-sysdrv.sh) takes the same release (userland/config.sh
#     VENUS_PROTOCOL_*): move both together
# Patches: 0001 (os_misc.c with the NDK), 0002 (host-visible memory from a dma-buf heap),
#   0003 (vtest without vrend), 0004 (vtest imports a client's dma-buf: the zero-copy present),
#   0005 (host-visible memory from AHardwareBuffers where the driver shares no dma-buf, as
#   Qualcomm's drivers; the servers link libandroid for it), 0006 (the device's object mutex is
#   destroyed after its objects: bionic aborts on a lock of a destroyed mutex), 0007 (the render
#   server's log lines also go to stderr, where the app reads a lost context), 0008 (the context's
#   ring mutex is destroyed after the ring monitor, for the same abort), 0009 (device extensions
#   that a driver runs wrong are hidden from the guest, with a log line: Samsung's Xclipse driver
#   loses VK_EXT_vertex_input_dynamic_state; PD2_VKR_HIDE_EXTENSIONS hides more for tests).
#   patches/test/ is never applied here.
# The VirGL server's pin and patches:
#   libepoxy 1.5.10 (GitHub's archive of the tag), MIT
#   virglrenderer-gl/0001 (vrend's EGL on Android without GBM: the default display, surfaceless),
#   0002 (no dual-source blending on Android: Qualcomm's GLES drivers fail to link vrend's
#   shaders with several color outputs, virglrenderer issue #223), 0003 (VCMD_PD2_APP_PRESENT:
#   the server blits the guest's front buffer into the app's AHardwareBuffers and gives them to
#   the app's present server with a fence, so the frames stay on the GPU; links libandroid), 0004
#   (integer GLSL types for integer vertex attributes and color buffers on every host, not only
#   Arm's: ANGLE and Qualcomm's GLES read integer attributes wrongly into vrend's float inputs;
#   VIRGL_USE_INTEGER=0 turns it off).
set -euo pipefail
[[ -z "${TRACE:-}" ]] || set -x

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
work="${PD2_DEVDRIVER_WORK:-$HOME/.cache/pd2-android/devdriver/build}"

buildtype="${PD2_DEVDRIVER_BUILDTYPE:-release}"
VIRGL_COMMIT=e18f8c00a9969d41f1cde6b2c7a73af8f518e660
VIRGL_SHA256=ad7b1fd7d6922f80777378c7ecf065b4e7ac0971defb9b91934a6203e3a33acf
VIRGL_URL="https://gitlab.freedesktop.org/virgl/virglrenderer/-/archive/$VIRGL_COMMIT/virglrenderer-$VIRGL_COMMIT.tar.gz"
VPROTO_VERSION=v1.1.3
VPROTO_SHA256=c5a362ca28e0a35042db1baeadaf9183500b216841e49d72714b80bae50b4445
VPROTO_URL="https://gitlab.freedesktop.org/virgl/venus-protocol/-/archive/$VPROTO_VERSION/venus-protocol-$VPROTO_VERSION.tar.gz"
EPOXY_VERSION=1.5.10
EPOXY_SHA256=a7ced37f4102b745ac86d6a70a9da399cc139ff168ba6b8002b4d8d43c900c15
EPOXY_URL="https://github.com/anholt/libepoxy/archive/refs/tags/$EPOXY_VERSION.tar.gz"

identity() {
    echo "virglrenderer=$VIRGL_COMMIT $VIRGL_SHA256"
    echo "venus-protocol=$VPROTO_VERSION $VPROTO_SHA256"
    echo "libepoxy=$EPOXY_VERSION $EPOXY_SHA256"
    echo "buildtype=$buildtype"
    local f
    for f in "$here/build.sh" "$here/Containerfile" "$here/../containers/Containerfile" "$here"/patches/virglrenderer/*.patch \
        "$here"/patches/virglrenderer-gl/*.patch; do
        echo "${f#"$here/"} $(sha256sum < "$f" | cut -d' ' -f1)"
    done
}
if [[ "${1:-}" == --identity ]]; then
    identity
    exit 0
fi

out="$(mkdir -p "$1" && cd "$1" && pwd)"
src="${PD2_DEVDRIVER_SRC:-$HOME/.cache/pd2-android/devdriver/src}"

fetch() { # url sha256 file
    [[ -f "$3" ]] || { curl -fsSL --retry 5 --retry-all-errors -o "$3.part" "$1" && mv "$3.part" "$3"; }
    echo "$2  $3" | sha256sum -c --quiet -
}
mkdir -p "$src" "$work"
fetch "$VIRGL_URL" "$VIRGL_SHA256" "$src/virglrenderer-$VIRGL_COMMIT.tar.gz"
fetch "$VPROTO_URL" "$VPROTO_SHA256" "$src/venus-protocol-$VPROTO_VERSION.tar.gz"
fetch "$EPOXY_URL" "$EPOXY_SHA256" "$src/libepoxy-$EPOXY_VERSION.tar.gz"

rm -rf "$work/virgl" "$work/virgl-gl" "$work/epoxy" "$work/epoxy-install"
mkdir -p "$work/virgl/subprojects/venus-protocol-1.1.3"
tar -xzf "$src/virglrenderer-$VIRGL_COMMIT.tar.gz" -C "$work/virgl" --strip-components=1
# The wrap's directory name (subprojects/venus-protocol.wrap); with it present, meson needs no network.
tar -xzf "$src/venus-protocol-$VPROTO_VERSION.tar.gz" -C "$work/virgl/subprojects/venus-protocol-1.1.3" --strip-components=1
for p in "$here"/patches/virglrenderer/*.patch; do
    patch -d "$work/virgl" -p1 --forward --fuzz=0 --quiet --no-backup-if-mismatch < "$p"
    echo "applied $(basename "$p")"
done
# The VirGL server's tree: the same source with 0001 (os_misc.c with the NDK) and the vrend patches.
mkdir -p "$work/virgl-gl" "$work/epoxy"
tar -xzf "$src/virglrenderer-$VIRGL_COMMIT.tar.gz" -C "$work/virgl-gl" --strip-components=1
tar -xzf "$src/libepoxy-$EPOXY_VERSION.tar.gz" -C "$work/epoxy" --strip-components=1
for p in "$here"/patches/virglrenderer/0001-*.patch "$here"/patches/virglrenderer-gl/*.patch; do
    patch -d "$work/virgl-gl" -p1 --forward --fuzz=0 --quiet --no-backup-if-mismatch < "$p"
    echo "applied $(basename "$p") (VirGL server)"
done

# The image tag hashes the Containerfile and its base, so edits rebuild and the rest is reused.
base="localhost/pd2-android-build:$(sha256sum "$here/../containers/Containerfile" | cut -c1-16)"
image="${PD2_NDK_IMAGE:-localhost/pd2-devdriver-ndk:$( (cat "$here/Containerfile"; echo "$base") | sha256sum | cut -c1-16)}"
if ! podman image exists "$image"; then
    podman image exists "$base" || { echo "no $base: run the app's build.sh once first" >&2; exit 1; }
    podman build --build-arg BASE="$base" -t "$image" -f "$here/Containerfile" "$here"
fi

podman run --rm -v "$work:/work" -v "$out:/out" -e BUILDTYPE="$buildtype" "$image" bash -euo pipefail -c '
T=/opt/android-sdk/ndk/27.2.12479018/toolchains/llvm/prebuilt/linux-x86_64/bin
cat > /work/cross.ini <<EOF
[binaries]
c = '\''$T/aarch64-linux-android30-clang'\''
cpp = '\''$T/aarch64-linux-android30-clang++'\''
ar = '\''$T/llvm-ar'\''
strip = '\''$T/llvm-strip'\''
pkg-config = '\''pkgconf'\''

[built-in options]
c_link_args = ['\''-llog'\'', '\''-landroid'\'']

[host_machine]
system = '\''android'\''
cpu_family = '\''aarch64'\''
cpu = '\''aarch64'\''
endian = '\''little'\''
EOF
# Venus only; the server loads the system libvulkan.so at run time (vulkan-dload); each client
# gets its own render server process (render-server-worker=process).
meson setup /work/virgl/out /work/virgl --cross-file /work/cross.ini --prefix=/opt/devdriver \
    --wrap-mode=nodownload --buildtype=$BUILDTYPE -Ddefault_library=static \
    -Dvenus=true -Dvrend=false -Dvulkan-dload=true -Drender-server-worker=process \
    -Dtests=false -Dvideo=false -Dtracing=none > /work/virgl-setup.log
ninja -C /work/virgl/out vtest/virgl_test_server server/virgl_render_server > /work/virgl-build.log
install -m 0755 /work/virgl/out/vtest/virgl_test_server /work/virgl/out/server/virgl_render_server /out/
$T/llvm-strip /out/virgl_test_server /out/virgl_render_server
$T/llvm-readelf -d /out/virgl_test_server /out/virgl_render_server | grep NEEDED

# The VirGL server: libepoxy, static and EGL only (it opens the phone'\''s libEGL.so and
# libGLESv2.so at run time), then the vtest server with vrend only, on EGL (no GBM on Android:
# patch virglrenderer-gl/0001). Its own cross file finds only that libepoxy.
sed -e "s|^\[built-in options\]|[properties]\npkg_config_libdir = ['\''/work/epoxy-install/lib/pkgconfig'\'']\n\n[built-in options]|" \
    -e "s|^c_link_args = .*|c_link_args = ['\''-llog'\'', '\''-ldl'\'']|" /work/cross.ini > /work/cross-gl.ini
meson setup /work/epoxy/out /work/epoxy --cross-file /work/cross-gl.ini --prefix=/work/epoxy-install \
    --libdir=lib --buildtype=$BUILDTYPE -Ddefault_library=static \
    -Degl=yes -Dglx=no -Dx11=false -Dtests=false -Ddocs=false > /work/epoxy-setup.log
ninja -C /work/epoxy/out install > /work/epoxy-build.log
meson setup /work/virgl-gl/out /work/virgl-gl --cross-file /work/cross-gl.ini --prefix=/opt/devdriver \
    --wrap-mode=nodownload --buildtype=$BUILDTYPE -Ddefault_library=static \
    -Dvenus=false -Dvrend=true -Dplatforms=egl \
    -Dtests=false -Dvideo=false -Dtracing=none > /work/virgl-gl-setup.log
grep -q "^#define HAVE_EPOXY_EGL_H\b" /work/virgl-gl/out/config.h || { echo "the VirGL server has no EGL (virgl-gl-setup.log)" >&2; exit 1; }
ninja -C /work/virgl-gl/out vtest/virgl_test_server > /work/virgl-gl-build.log
install -m 0755 /work/virgl-gl/out/vtest/virgl_test_server /out/virgl_test_server_gl
$T/llvm-strip /out/virgl_test_server_gl
$T/llvm-readelf -d /out/virgl_test_server_gl | grep NEEDED
'
(cd "$out" && sha256sum virgl_test_server virgl_render_server virgl_test_server_gl > SHA256SUMS)
identity > "$out/BUILDINFO"
echo "built $out/virgl_test_server $out/virgl_render_server $out/virgl_test_server_gl"
