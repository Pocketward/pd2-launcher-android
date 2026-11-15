#!/usr/bin/env bash
# Builds the userland parts of the system-driver mode, the game's GPU path for GPUs that Turnip and
# PanVK do not drive: Zink -> Mesa's Venus ICD -> vtest's socket -> virglrenderer's server in the
# APK (devdriver/) -> the phone's own Vulkan driver.
# A cross build for aarch64 in the base image (../containers/Containerfile.base), as build-mesa.sh:
# Venus, Mesa main at $MESA_COMMIT (the same archive as build-mesa.sh) with all our patches/mesa/*.patch
#      (the software WSI series: Zink presents through the Vulkan driver's WSI, so the present
#      counter, the Game view's pacing and the back pixmap work as with Turnip) and
#      patches/mesa-sysdrv/*.patch (Venus only: the vtest socket and Venus's debug options from
#      MESA_* variables, as the engine passes MESA_* variables to Wine processes and not VTEST_* or
#      VN_*; a dma-buf import over vtest through the server's import command, android/devdriver
#      patch 0004, for the zero-copy present, or a clean failure without it), Vulkan only:
#        vulkan-drivers=virtio          Venus (vtest is built in on non-Windows systems)
#        gallium-drivers=, opengl=false no GL in this build (build-mesa.sh's GL serves the other modes)
#      A build of its own, so Turnip keeps linking without libdrm (Mesa's meson.build: Turnip is
#      forced to link with libdrm when built alongside other Vulkan drivers). Main takes the Venus
#      protocol headers from the venus-protocol subproject: config.sh's VENUS_PROTOCOL_* release,
#      unpacked into the folder that subprojects/venus-protocol.wrap names (the build refuses a
#      wrap that names another release). The patches apply with --fuzz=0, so mesa-update.sh
#      checks and refreshes patches/mesa-sysdrv/ with the others when the pin moves.
#      Installs libvulkan_virtio.so and virtio_icd.aarch64.json.
# The mode's OpenGL is build-mesa.sh's Zink (glvnd vendor "mesa"), the one GL stack of every mode
# (ADR 0030: no second, older Zink).
#
# The installed files are listed as the userland's part "system-driver" (work/parts/, pack.sh): an
# archive of their own that an APK can leave out.
# Outputs in the cache's sysdrv-out/ with BUILDINFO (the image, the Mesa and Venus protocol pins,
# the prefix, this script, the patches); a later run with the same identity installs without building.
# `build-sysdrv.sh --identity` prints that identity on the host, before any build (the release
# workflow's cache key); the sources are fetched only when sysdrv-out does not have it.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=config.sh
. "$here/config.sh"

identity() {
    echo "image=${PD2_IMAGE:-}"
    echo "mesa=$MESA_COMMIT $MESA_SHA256"
    echo "venus-protocol=$VENUS_PROTOCOL_VERSION $VENUS_PROTOCOL_SHA256"
    echo "prefix=$USERLAND"
    for f in "$here/build-sysdrv.sh" "$here"/patches/mesa/*.patch "$here"/patches/mesa-sysdrv/*.patch; do
        echo "${f#"$here"/} $(sha256sum < "$f" | cut -d' ' -f1)"
    done
}
if [[ "${1:-}" == --identity ]]; then
    # The container's order of file names (no locale there).
    export LC_ALL=C
    PD2_IMAGE="${PD2_IMAGE:-$(base_image_name)}" identity
    exit 0
fi

if [[ "${PD2_USERLAND_IN_CONTAINER:-}" != 1 ]]; then
    if [[ ! -d "$USERLAND_CACHE/sysdrv-out/tree" || "$(cat "$USERLAND_CACHE/sysdrv-out/BUILDINFO" 2>/dev/null)" != "$("$here/build-sysdrv.sh" --identity)" ]]; then
        fetch_source "$MESA_URL" "$MESA_SHA256" >/dev/null
        fetch_source "$VENUS_PROTOCOL_URL" "$VENUS_PROTOCOL_SHA256" >/dev/null
    fi
    run_in_base build-sysdrv.sh --build
    run_in_base build-sysdrv.sh --install
    exit
fi

out=/cache/sysdrv-out
libdir=lib/aarch64-linux-gnu

if [[ "${1:-}" == --build ]]; then
    id="$(identity)"
    if [[ -d "$out/tree" && "$(cat "$out/BUILDINFO" 2>/dev/null)" == "$id" ]]; then
        echo "reusing sysdrv-out (BUILDINFO matches)"
        exit 0
    fi
    work=/cache/work/sysdrv
    rm -rf "$work" "$out"
    mkdir -p "$work" "$out/tree"
    cat > "$work/cross.ini" <<EOF
[binaries]
c = 'aarch64-linux-gnu-gcc'
cpp = 'aarch64-linux-gnu-g++'
ar = 'aarch64-linux-gnu-ar'
nm = 'aarch64-linux-gnu-nm'
strip = 'aarch64-linux-gnu-strip'
pkg-config = 'pkgconf'

[properties]
pkg_config_libdir = ['/usr/lib/aarch64-linux-gnu/pkgconfig', '/usr/share/pkgconfig']

[host_machine]
system = 'linux'
cpu_family = 'aarch64'
cpu = 'aarch64'
endian = 'little'
EOF
    common=(--cross-file "$work/cross.ini" --wrap-mode=nodownload --prefix="$USERLAND/usr"
        --buildtype=release -Db_ndebug=true -Dallow-kcmp=disabled -Dplatforms=x11 -Dgbm=disabled
        -Dgles1=disabled -Dgles2=disabled -Dllvm=disabled -Dvalgrind=disabled -Dlibunwind=disabled
        -Dlmsensors=disabled -Dvulkan-layers= -Dtools= -Dbuild-tests=false -Dhtml-docs=disabled
        -Dexpat=enabled -Dzstd=enabled -Dshader-cache=enabled -Dxlib-lease=disabled)

    # --- 1. Venus, Mesa main at $MESA_COMMIT ---------------------------------------------------
    src="$work/venus-src"
    mkdir -p "$src"
    tar -xzf "/cache/sources/$(basename "$MESA_URL")" -C "$src" --strip-components=1
    # The wrap's folder with the pinned release in it: meson then takes it without a download.
    wrap="$src/subprojects/venus-protocol.wrap"
    vproto="$(sed -n 's/^directory *= *//p' "$wrap")"
    [[ "$vproto" == "venus-protocol-${VENUS_PROTOCOL_VERSION#v}" ]] ||
        { echo "Mesa's $wrap names '$vproto', not venus-protocol-${VENUS_PROTOCOL_VERSION#v}: move config.sh's VENUS_PROTOCOL_* pin" >&2; exit 1; }
    mkdir -p "$src/subprojects/$vproto"
    tar -xzf "/cache/sources/$(basename "$VENUS_PROTOCOL_URL")" -C "$src/subprojects/$vproto" --strip-components=1
    for p in /src/patches/mesa/*.patch /src/patches/mesa-sysdrv/*.patch; do
        patch -d "$src" -p1 --forward --fuzz=0 --quiet < "$p"
        echo "venus: applied ${p#/src/patches/}"
    done
    meson setup "$work/venus-build" "$src" "${common[@]}" --libdir="$libdir" \
        -Dvulkan-drivers=virtio -Dgallium-drivers= -Dopengl=false -Dglx=disabled -Degl=disabled \
        -Dglvnd=disabled > "$work/venus-setup.log" 2>&1 || { tail -40 "$work/venus-setup.log"; exit 1; }
    ninja -C "$work/venus-build" -j"$(nproc)" > "$work/venus-ninja.log" 2>&1 || { tail -60 "$work/venus-ninja.log"; exit 1; }
    DESTDIR="$work/venus-dest" meson install -C "$work/venus-build" --strip --no-rebuild > /dev/null
    install -D -m 0755 "$work/venus-dest$USERLAND/usr/$libdir/libvulkan_virtio.so" "$out/tree/usr/$libdir/libvulkan_virtio.so"
    install -D -m 0644 "$work/venus-dest$USERLAND/usr/share/vulkan/icd.d/virtio_icd.aarch64.json" \
        "$out/tree/usr/share/vulkan/icd.d/virtio_icd.aarch64.json"

    rm -rf "$work"
    echo "$id" > "$out/BUILDINFO"
    exit 0
fi

# --install (base image): the build into the staging tree.
staging=/cache/work/staging
[[ -d "$staging/usr" ]] || { echo "no staging tree: run fetch-debian.sh first" >&2; exit 1; }
[[ -f "$out/BUILDINFO" && -d "$out/tree" ]] || { echo "no system-driver build: run build-sysdrv.sh" >&2; exit 1; }
cp -a "$out/tree/." "$staging/"
# The mode's files are a part of their own (pack.sh), which an APK can leave out.
mkdir -p /cache/work/parts
(cd "$out/tree" && find . -type f | sed 's|^\./||' | LC_ALL=C sort) > /cache/work/parts/system-driver.files
icd="$staging/usr/share/vulkan/icd.d/virtio_icd.aarch64.json"
want="$USERLAND/usr/$libdir/libvulkan_virtio.so"
got="$(jq -r .ICD.library_path "$icd")"
[[ "$got" == "$want" ]] || { echo "Venus ICD library_path is $got, want $want" >&2; exit 1; }
echo "system driver installed: Venus $MESA_VERSION (${MESA_COMMIT:0:12}, ICD -> $got)"
