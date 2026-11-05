#!/usr/bin/env bash
# Builds RADV for Samsung's Xclipse GPUs, the driver of the RADV route (DriverChoice: the game's
# Zink on our RADV, on the GPU's DRM node, only on an Xclipse), for the App userland's glibc, into
# the system-driver part (pack.sh), beside the Venus ICD.
#
# Source: config.sh's RADV_XC_* pins.
#   - JimVulkan/radv-xclipse at RADV_XC_COMMIT (MIT), a Mesa 27.0.0-devel tree (Mesa main of
#     2026-10-19/20) with the Xclipse changes, built by its authors for Android's bionic.
#   - libdrm RADV_XC_LIBDRM_VERSION, built by meson from the tree's subprojects/libdrm.wrap (which
#     applies the tree's two libdrm patches) and linked into the driver statically: Mesa main needs
#     libdrm_amdgpu 2.4.133 and Debian 13 has 2.4.124. Mesa's meson.build allows that fallback
#     only with -Dallow-fallback-for=libdrm.
#   - Upstream Mesa's files that the tree leaves out and a glibc X11 build needs
#     (RADV_XC_UPSTREAM_FILES, each checked by SHA-256).
#   - Our stand-ins for <android/log.h> (stderr) and <sys/system_properties.h> (no properties):
#     radv-compat/, as include/xclipse_linux_compat, which the tree's meson.build adds to glibc
#     builds but the tree does not carry.
#   - Our WSI patches, patches/radv-xclipse/*.patch: patches/mesa/0001-0004 and 0006-0008 (the
#     cached reads, the present counter, the Game view's pacing, the direct present into the app's
#     buffers, the fence wait in the present thread, the back pixmap, the app's clock), ported to
#     the tree: its X11 swapchains carry their own software flag (wsi_swapchain::sw), so the
#     patches ask chain->base.sw where upstream's ask wsi->sw, and 0007's fields of struct
#     x11_swapchain follow "window" (the tree has no event_conn). Applied exactly (fuzz 0, no
#     offset). 0005 is OpenGL only.
# Built in the userland's Mesa image (containers/Containerfile.mesa: gcc for aarch64-linux-gnu,
# Debian 13 arm64 X11 and xcb headers, glslang), with no network in the container (the sources are
# fetched and checked first): RADV only, the X11 window system (Xvfb; the app sets
# MESA_VK_WSI_DEBUG=sw), no LLVM (ACO), kcmp off (Android's seccomp policy), for the prefix /usr:
# no path compiled into it names the userland (it reads no file of its own there).
#
# Output: the cache's radv-out/tree (usr/lib/aarch64-linux-gnu/libvulkan_radeon.so,
# usr/share/vulkan/icd.d/radeon_icd.aarch64.json naming it at the userland's prefix, and the
# tree's licence and notices in usr/share/licenses/radv-xclipse/) with BUILDINFO; --install (tools
# image) copies it into the staging tree and adds its files to the system-driver part's list.
# The source tree and the configured build folder stay in the cache's work/ (radv-src, radv-build
# with SETUP, what the setup used): a later run with the same SETUP reuses them, and when only
# patches/radv-xclipse/ changes, it restores the tree's 4 WSI files, applies the patches again,
# and ninja rebuilds only what uses them. Each step prints a line; a failure prints its line and
# command (the ERR trap).
set -Eeuo pipefail
trap 'echo "build-radv.sh${PD2_USERLAND_IN_CONTAINER:+ (in the image)}: FAILED at line $LINENO: $BASH_COMMAND (exit $?)" >&2' ERR

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=config.sh
. "$here/config.sh"

say() { echo "[build-radv $(date +%T)] $*"; }
fail() { echo "build-radv.sh: $*" >&2; exit 1; }

mesa_image() {
    local engine image
    engine="$(container_engine)"
    image="localhost/pd2-userland-mesa:$( (cat "$USERLAND_SRC/containers/Containerfile.mesa"
        echo "$DEBIAN_IMAGE $DEBIAN_SNAPSHOT") | sha256sum | cut -c1-16)"
    if ! have_image "$engine" "$image"; then
        "$engine" build --platform linux/amd64 \
            --build-arg "DEBIAN_IMAGE=$DEBIAN_IMAGE" --build-arg "DEBIAN_SNAPSHOT=$DEBIAN_SNAPSHOT" \
            -t "$image" -f "$USERLAND_SRC/containers/Containerfile.mesa" "$USERLAND_SRC/containers" >&2
        archive_image "$engine" "$image"
    fi
    echo "$image"
}

if [[ "${PD2_USERLAND_IN_CONTAINER:-}" != 1 ]]; then
    fetch_source "$RADV_XC_URL" "$RADV_XC_SHA256" >/dev/null
    fetch_source "$RADV_XC_LIBDRM_URL" "$RADV_XC_LIBDRM_SHA256" >/dev/null
    fetch_source "$RADV_XC_UPSTREAM_URL" "$RADV_XC_UPSTREAM_SHA256" >/dev/null
    say "sources checked"
    image="$(mesa_image)"
    engine="$(container_engine)"
    mkdir -p "$USERLAND_CACHE/work"
    # No network in the container: the sources are fetched and checked above.
    "$engine" run --rm --network=none --platform linux/amd64 \
        -v "$USERLAND_SRC:/src:ro" -v "$USERLAND_CACHE:/cache" \
        -e PD2_USERLAND_IN_CONTAINER=1 -e PD2_USERLAND_CACHE=/cache -e "PD2_IMAGE=$image" \
        "$image" bash /src/build-radv.sh --build
    run_in_tools build-radv.sh --install
    exit
fi

out=/cache/radv-out
libdir=lib/aarch64-linux-gnu
lib=usr/$libdir/libvulkan_radeon.so
icd=usr/share/vulkan/icd.d/radeon_icd.aarch64.json
# Not usr/share/doc: relocate.sh drops that folder.
doc=usr/share/licenses/radv-xclipse

if [[ "${1:-}" == --build ]]; then
    src=/cache/work/radv-src
    build=/cache/work/radv-build
    tree_archive="/cache/sources/$(basename "$RADV_XC_URL")"
    MESON_OPTS=(
        -Dallow-fallback-for=libdrm --force-fallback-for=libdrm -Dlibdrm:default_library=static
        --prefix=/usr --libdir="$libdir" --buildtype=release -Db_ndebug=true
        -Dvulkan-drivers=amd -Dgallium-drivers= -Damdgpu-virtio=false -Dallow-kcmp=disabled
        -Dplatforms=x11 -Dglx=disabled -Degl=disabled -Dgbm=disabled -Dopengl=false
        -Dgles1=disabled -Dgles2=disabled -Dllvm=disabled -Dvalgrind=disabled -Dlibunwind=disabled
        -Dlmsensors=disabled -Dvulkan-layers= -Dtools= -Dbuild-tests=false -Dhtml-docs=disabled
        -Dvideo-codecs= -Dexpat=enabled -Dzstd=enabled -Dshader-cache=enabled -Dxlib-lease=disabled
    )
    WSI_FILES=(src/vulkan/wsi/wsi_common.c src/vulkan/wsi/wsi_common.h src/vulkan/wsi/wsi_common_private.h
        src/vulkan/wsi/wsi_common_x11.c)
    # What the source tree and the configured build folder come from; another value needs a new setup.
    setup_identity() {
        echo "image=${PD2_IMAGE:-}"
        echo "radv-xclipse=$RADV_XC_COMMIT $RADV_XC_SHA256"
        echo "libdrm=$RADV_XC_LIBDRM_VERSION $RADV_XC_LIBDRM_SHA256"
        echo "upstream=$RADV_XC_UPSTREAM_COMMIT $RADV_XC_UPSTREAM_SHA256"
        printf '%s\n' "${RADV_XC_UPSTREAM_FILES[@]}" "${MESON_OPTS[@]}"
        (cd /src && find radv-compat -type f | LC_ALL=C sort | xargs sha256sum)
    }
    sid="$(setup_identity)"
    pid="$(cd /src && find patches/radv-xclipse -type f -name '*.patch' | LC_ALL=C sort | xargs sha256sum)"
    # What the output comes from: the setup, the patches, the prefix and this script.
    id="$sid
$pid
prefix=$USERLAND
build-radv.sh $(sha256sum < /src/build-radv.sh | cut -d' ' -f1)"
    # (Files are read only when they exist: a failing command in $(...) would trip the ERR trap.)
    if [[ -d "$out/tree" && -f "$out/BUILDINFO" && "$(< "$out/BUILDINFO")" == "$id" ]]; then
        say "reusing radv-out (BUILDINFO matches)"
        exit 0
    fi
    if [[ -f "$build/out/build.ninja" && -f "$build/SETUP" && "$(< "$build/SETUP")" == "$sid" ]]; then
        say "reusing the configured build folder (SETUP matches): ninja builds only what changed"
    else
        say "new source tree and build folder"
        rm -rf "$src" "$build"
        mkdir -p "$src" "$build"
        tar -xzf "$tree_archive" -C "$src" --strip-components=1
        [[ ! -e "$src/include/xclipse_linux_compat" ]] || fail "the tree has include/xclipse_linux_compat now: check radv-compat/"
        cp -r /src/radv-compat "$src/include/xclipse_linux_compat"
        upstream_paths=()
        for entry in "${RADV_XC_UPSTREAM_FILES[@]}"; do
            [[ ! -e "$src/${entry#* }" ]] || fail "the tree has ${entry#* } now: check RADV_XC_UPSTREAM_FILES"
            upstream_paths+=("mesa-$RADV_XC_UPSTREAM_COMMIT/${entry#* }")
        done
        tar -xzf "/cache/sources/$(basename "$RADV_XC_UPSTREAM_URL")" -C "$src" --strip-components=1 "${upstream_paths[@]}"
        for entry in "${RADV_XC_UPSTREAM_FILES[@]}"; do
            echo "${entry%% *}  $src/${entry#* }" | sha256sum -c --quiet - || fail "SHA-256 mismatch: ${entry#* }"
        done
        say "upstream files restored: ${#RADV_XC_UPSTREAM_FILES[@]}"
        # meson takes the wrap's archive from the package cache (no download in the image).
        mkdir -p "$src/subprojects/packagecache"
        cp "/cache/sources/$(basename "$RADV_XC_LIBDRM_URL")" "$src/subprojects/packagecache/"
        cat > "$build/cross.ini" <<EOF
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
        say "meson setup (log: $build/setup.log)"
        meson setup "$build/out" "$src" --cross-file "$build/cross.ini" --wrap-mode=nodownload "${MESON_OPTS[@]}" \
            > "$build/setup.log" 2>&1 || { tail -60 "$build/setup.log" || true; fail "meson setup failed (above: the end of $build/setup.log)"; }
        echo "$sid" > "$build/SETUP"
    fi

    # The WSI patches, when the tree does not have this set yet: the tree's own 4 WSI files back
    # (with the time of now, so ninja rebuilds what uses them), then each patch, exactly.
    if [[ -f "$src/.pd2-wsi-patches" && "$(< "$src/.pd2-wsi-patches")" == "$pid" ]]; then
        say "WSI patches: already in the tree"
    else
        say "WSI patches: the tree's ${#WSI_FILES[@]} WSI files restored"
        rm -f "$src/.pd2-wsi-patches"
        tar -xzf "$tree_archive" -C "$src" --strip-components=1 --touch \
            "${WSI_FILES[@]/#/radv-xclipse-$RADV_XC_COMMIT/}"
        for p in /src/patches/radv-xclipse/*.patch; do
            patch -d "$src" -p1 --forward --fuzz=0 --no-backup-if-mismatch < "$p" > "$build/patch.log" 2>&1 ||
                { cat "$build/patch.log"; fail "$(basename "$p") does not apply to the tree"; }
            if grep -q -E 'offset|fuzz' "$build/patch.log"; then
                cat "$build/patch.log"
                fail "$(basename "$p") applies only with an offset: refresh it for the tree"
            fi
            say "WSI patch applied: $(basename "$p")"
        done
        echo "$pid" > "$src/.pd2-wsi-patches"
    fi

    rm -rf "$out"
    mkdir -p "$out/tree/usr/$libdir" "$out/tree/usr/share/vulkan/icd.d" "$out/tree/$doc"
    say "ninja: src/amd/vulkan/libvulkan_radeon.so (log: $build/ninja.log)"
    ninja -C "$build/out" -j"$(nproc)" src/amd/vulkan/libvulkan_radeon.so > "$build/ninja.log" 2>&1 ||
        { grep -E 'error|Error' "$build/ninja.log" | head -40 || true; tail -30 "$build/ninja.log" || true; fail "ninja failed (above: errors from $build/ninja.log)"; }
    aarch64-linux-gnu-strip --strip-unneeded -o "$out/tree/$lib" "$build/out/src/amd/vulkan/libvulkan_radeon.so"
    chmod 0755 "$out/tree/$lib"
    # The ICD JSON: the API version of the tree's Vulkan headers (RADV is a Vulkan 1.4 driver); the
    # library at the userland's prefix (the app relocates it with the rest). The tree's own
    # radeon_icd.aarch64.json is another ninja target, not built here.
    hv="$(sed -n 's/^#define VK_HEADER_VERSION \([0-9][0-9]*\).*/\1/p' "$src/include/vulkan/vulkan_core.h")"
    [[ -n "$hv" ]] || fail "no VK_HEADER_VERSION in $src/include/vulkan/vulkan_core.h"
    cat > "$out/tree/$icd" <<EOF
{
    "ICD": {
        "api_version": "1.4.$hv",
        "library_path": "$USERLAND/$lib"
    },
    "file_format_version": "1.0.1"
}
EOF
    # The licence (MIT) and the notices of what is compiled into the driver.
    install -m 0644 "$src/LICENSE" "$out/tree/$doc/LICENSE"
    install -m 0644 "$src/android/NOTICE.txt" "$out/tree/$doc/NOTICE.txt"
    install -m 0644 "$src/THIRD_PARTY_NOTICES.md" "$out/tree/$doc/THIRD_PARTY_NOTICES.md"
    aarch64-linux-gnu-readelf -d "$out/tree/$lib" > "$build/readelf.txt"
    sed -n 's/.*(NEEDED).*\[\(.*\)\]$/\1/p' "$build/readelf.txt" > "$out/needed.txt"
    [[ -s "$out/needed.txt" ]] || fail "readelf found no NEEDED entry in $lib"
    # libdrm is in the driver (static): the userland's libdrm 2.4.124 must never serve it.
    if grep -q '^libdrm' "$out/needed.txt"; then
        fail "$lib needs a shared libdrm ($(grep '^libdrm' "$out/needed.txt" | tr '\n' ' ')): not static"
    fi
    echo "$id" > "$out/BUILDINFO"
    say "built: $(stat -c %s "$out/tree/$lib") bytes; ICD api_version 1.4.$hv; needs: $(tr '\n' ' ' < "$out/needed.txt")"
    exit 0
fi

# --install (tools image): the build into the staging tree; its files join the system-driver part.
staging=/cache/work/staging
[[ -d "$staging/usr" ]] || fail "no staging tree: run fetch-debian.sh first"
[[ -f "$out/BUILDINFO" && -d "$out/tree" ]] || fail "no RADV build: run build-radv.sh"
parts=/cache/work/parts/system-driver.files
[[ -f "$parts" ]] || fail "no system-driver part ($parts): build-sysdrv.sh runs first (build.sh)"
cp -a "$out/tree/." "$staging/"
# The part's list stays sorted (pack.sh checks it); a run again adds nothing twice.
(cat "$parts"; cd "$out/tree" && find . -type f | sed 's|^\./||') | LC_ALL=C sort -u > "$parts.new"
mv "$parts.new" "$parts"
want="$USERLAND/$lib"
got="$(jq -r .ICD.library_path "$staging/$icd")"
[[ "$got" == "$want" ]] || fail "RADV ICD library_path is $got, want $want"
for needed in $(cat "$out/needed.txt"); do
    found="$(find "$staging/lib/aarch64-linux-gnu" "$staging/usr/lib/aarch64-linux-gnu" -maxdepth 1 -name "$needed" -print -quit 2>/dev/null)"
    [[ -n "$found" ]] || fail "$lib needs $needed, which the staging tree does not have"
done
say "RADV installed: ${RADV_XC_COMMIT:0:12} (radv-xclipse), ICD -> $got, in the system-driver part"
