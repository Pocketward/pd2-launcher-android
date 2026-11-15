#!/usr/bin/env bash
# Cross-builds Mesa (config.sh's MESA_COMMIT of main) for aarch64 (Debian multiarch, in the base image
# ../containers/Containerfile.base)
# and installs it into the staging tree: Zink (OpenGL on Vulkan) without LLVM, softpipe for Xvfb's
# GLX extension (its swrast_dri.so lists the GLX visuals; the game draws through Zink), virgl for
# the system-driver mode's VirGL route (GALLIUM_DRIVER=virpipe on the GLX software path: OpenGL
# over vtest's socket to the APK's virglrenderer server, which draws with the phone's own GLES),
# GLX (dri) and EGL as glvnd vendors, and Turnip (Vulkan driver "freedreno", KGSL only). The ARM64
# Wine's OpenGL loads it in the game's own process. Options from Mesa's meson.options (at MESA_COMMIT):
#   gallium-drivers=zink,softpipe,virgl   Zink, softpipe and virgl (its vtest winsys) in libgallium
#   vulkan-drivers=freedreno        Turnip
#   freedreno-kmds=kgsl             KGSL only (Android's Adreno kernel driver)
#   allow-kcmp=disabled             kcmp is blocked by Android's seccomp policy (meson.build:1147)
#   platforms=x11, glx=dri, egl=enabled, glvnd=enabled
#   prefix=$USERLAND/usr            the ICD JSON's library_path and the DRI search path are below the
#                                   prefix, and the app relocates them with the rest
# and our patches/mesa/*.patch (see patches/mesa/README.md) on the unpacked source. The build goes
# to the cache's mesa-out/ with its BUILDINFO (the image, the Mesa pin, this script, the patches),
# which a later run with the same identity installs without building. `build-mesa.sh --identity`
# prints that identity on the host, before any build (the release workflow's cache key); the
# Mesa archive is fetched only when mesa-out does not have it.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=config.sh
. "$here/config.sh"

identity() {
    echo "image=${PD2_IMAGE:-}"
    echo "mesa=$MESA_COMMIT $MESA_SHA256"
    echo "prefix=$USERLAND"
    for f in "$here/build-mesa.sh" "$here"/patches/mesa/*.patch; do
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
    if [[ ! -d "$USERLAND_CACHE/mesa-out/tree" || "$(cat "$USERLAND_CACHE/mesa-out/BUILDINFO" 2>/dev/null)" != "$("$here/build-mesa.sh" --identity)" ]]; then
        fetch_source "$MESA_URL" "$MESA_SHA256" >/dev/null
    fi
    run_in_base build-mesa.sh --build
    run_in_base build-mesa.sh --install
    exit
fi

out=/cache/mesa-out

if [[ "${1:-}" == --build ]]; then
    id="$(identity)"
    if [[ -d "$out/tree" && "$(cat "$out/BUILDINFO" 2>/dev/null)" == "$id" ]]; then
        echo "reusing mesa-out (BUILDINFO matches)"
        exit 0
    fi
    src=/cache/work/mesa-src
    build=/cache/work/mesa-build
    rm -rf "$src" "$build" "$out"
    mkdir -p "$src" "$build" "$out"
    tar -xzf "/cache/sources/$(basename "$MESA_URL")" -C "$src" --strip-components=1
    for p in /src/patches/mesa/*.patch; do
        patch -d "$src" -p1 --forward --fuzz=0 --quiet < "$p"
        echo "applied $(basename "$p")"
    done
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
    meson setup "$build/out" "$src" --cross-file "$build/cross.ini" --wrap-mode=nodownload \
        --prefix="$USERLAND/usr" --libdir=lib/aarch64-linux-gnu --buildtype=release -Db_ndebug=true \
        -Dgallium-drivers=zink,softpipe,virgl -Dvulkan-drivers=freedreno -Dfreedreno-kmds=kgsl -Dallow-kcmp=disabled \
        -Dplatforms=x11 -Dglx=dri -Degl=enabled -Dglvnd=enabled -Dgbm=disabled -Dopengl=true \
        -Dgles1=disabled -Dgles2=disabled -Dllvm=disabled -Dvalgrind=disabled -Dlibunwind=disabled \
        -Dlmsensors=disabled -Dvulkan-layers= -Dtools= -Dbuild-tests=false -Dhtml-docs=disabled \
        -Dexpat=enabled -Dzstd=enabled -Dshader-cache=enabled -Dxlib-lease=disabled > "$build/setup.log" 2>&1 \
        || { tail -40 "$build/setup.log"; exit 1; }
    ninja -C "$build/out" -j"$(nproc)" > "$build/ninja.log" 2>&1 || { tail -60 "$build/ninja.log"; exit 1; }
    DESTDIR="$out/install" meson install -C "$build/out" --strip --no-rebuild > "$build/install.log" 2>&1
    mv "$out/install$USERLAND" "$out/tree"
    rm -rf "$out/install" "$src" "$build"
    # Headers and pkg-config files are for builds against Mesa; nothing of them runs.
    rm -rf "$out/tree/usr/include" "$out/tree/usr/lib/aarch64-linux-gnu/pkgconfig"
    echo "$id" > "$out/BUILDINFO"
    exit 0
fi

# --install (base image): the build into the staging tree, over Debian's glvnd libraries.
staging=/cache/work/staging
[[ -d "$staging/usr" ]] || { echo "no staging tree: run fetch-debian.sh first" >&2; exit 1; }
[[ -f "$out/BUILDINFO" && -d "$out/tree" ]] || { echo "no Mesa build: run build-mesa.sh" >&2; exit 1; }
cp -a "$out/tree/." "$staging/"
icd="$staging/usr/share/vulkan/icd.d/freedreno_icd.aarch64.json"
lib="$USERLAND/usr/lib/aarch64-linux-gnu/libvulkan_freedreno.so"
got="$(jq -r .ICD.library_path "$icd")"
[[ "$got" == "$lib" ]] || { echo "ICD library_path is $got, want $lib" >&2; exit 1; }
for f in usr/lib/aarch64-linux-gnu/libGLX_mesa.so.0 usr/lib/aarch64-linux-gnu/libEGL_mesa.so.0 \
    usr/lib/aarch64-linux-gnu/dri/zink_dri.so usr/lib/aarch64-linux-gnu/dri/swrast_dri.so \
    usr/share/glvnd/egl_vendor.d/50_mesa.json; do
    [[ -e "$staging/$f" ]] || { echo "Mesa install incomplete: no $f" >&2; exit 1; }
done
echo "Mesa $MESA_VERSION (${MESA_COMMIT:0:12}) installed (Zink, softpipe, virgl, GLX, EGL, Turnip); ICD -> $got"
