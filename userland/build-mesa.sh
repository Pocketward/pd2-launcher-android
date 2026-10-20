#!/usr/bin/env bash
# Cross-builds Mesa 26.2.3 for aarch64 (Debian multiarch, in its own image containers/Containerfile.mesa)
# and installs it into the staging tree: Zink (OpenGL on Vulkan) without LLVM, softpipe for Xvfb's
# GLX extension (its swrast_dri.so lists the GLX visuals; the game draws through Zink), GLX (dri)
# and EGL as glvnd vendors, and Turnip (Vulkan driver "freedreno", KGSL only). The ARM64 Wine's
# OpenGL loads it in the game's own process. Options from Mesa's meson.options (mesa-26.2.3):
#   gallium-drivers=zink,softpipe   Zink and softpipe in libgallium
#   vulkan-drivers=freedreno        Turnip
#   freedreno-kmds=kgsl             KGSL only (Android's Adreno kernel driver)
#   allow-kcmp=disabled             kcmp is blocked by Android's seccomp policy (meson.build:1083)
#   platforms=x11, glx=dri, egl=enabled, glvnd=enabled
#   prefix=$USERLAND/usr            the ICD JSON's library_path and the DRI search path are below the
#                                   prefix, and the app relocates them with the rest
# and our patches/mesa/*.patch (see patches/mesa/README.md) on the unpacked source. The build goes
# to the cache's mesa-out/ with its BUILDINFO (the image, the Mesa pin, this script, the patches),
# which a later run with the same identity installs without building.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=config.sh
. "$here/config.sh"

# The Mesa image's name: the tag hashes its Containerfile and the two Debian pins.
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
    fetch_source "$MESA_URL" "$MESA_SHA256" >/dev/null
    image="$(mesa_image)"
    engine="$(container_engine)"
    mkdir -p "$USERLAND_CACHE/work"
    "$engine" run --rm --platform linux/amd64 \
        -v "$USERLAND_SRC:/src:ro" -v "$USERLAND_CACHE:/cache" \
        -e PD2_USERLAND_IN_CONTAINER=1 -e PD2_USERLAND_CACHE=/cache -e "PD2_IMAGE=$image" \
        "$image" bash /src/build-mesa.sh --build
    run_in_tools build-mesa.sh --install
    exit
fi

out=/cache/mesa-out
identity() {
    echo "image=${PD2_IMAGE:-}"
    echo "mesa=$MESA_VERSION $MESA_SHA256"
    echo "prefix=$USERLAND"
    for f in /src/build-mesa.sh /src/patches/mesa/*.patch; do
        echo "${f#/src/} $(sha256sum < "$f" | cut -d' ' -f1)"
    done
}

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
    tar -xJf "/cache/sources/$(basename "$MESA_URL")" -C "$src" --strip-components=1
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
        -Dgallium-drivers=zink,softpipe -Dvulkan-drivers=freedreno -Dfreedreno-kmds=kgsl -Dallow-kcmp=disabled \
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

# --install (tools image): the build into the staging tree, over Debian's glvnd libraries.
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
echo "Mesa $MESA_VERSION installed (Zink, softpipe, GLX, EGL, Turnip); ICD -> $got"
