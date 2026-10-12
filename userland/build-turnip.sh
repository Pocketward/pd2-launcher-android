#!/usr/bin/env bash
# Cross-builds Mesa's Turnip (Vulkan driver "freedreno") for aarch64 against the staged sysroot,
# KGSL kernel interface only, no GL, and installs libvulkan_freedreno.so plus its ICD JSON into
# the staging tree. Options from Mesa's meson.options (mesa-26.2.3):
#   vulkan-drivers=freedreno   Turnip
#   freedreno-kmds=kgsl        KGSL only: with no msm and no Gallium, Turnip does not link libdrm
#                              (meson.build:327-340)
#   allow-kcmp=disabled        kcmp is blocked by Android's seccomp policy (meson.build:1083, meson.options:610)
#   platforms=x11              Vulkan X11 WSI (Zink on Xvfb, the Launcher's wgpu window)
#   prefix=$USERLAND/usr       the ICD JSON's library_path is prefix/libdir (meson.build:2524)
# and our patches/mesa/*.patch (see patches/mesa/README.md) on the unpacked source.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=config.sh
. "$here/config.sh"

if [[ "${PD2_USERLAND_IN_CONTAINER:-}" != 1 ]]; then
    run_in_tools build-turnip.sh "$@"
    exit
fi

work=/cache/work
sysroot="$work/sysroot"
build="$work/turnip-build"
dest="$work/turnip-dest"
[[ -d "$sysroot/usr/include" ]] || { echo "no sysroot: run fetch-debian.sh first" >&2; exit 1; }

tarball="$(fetch_source "$MESA_URL" "$MESA_SHA256")"
rm -rf "$build" "$dest"
mkdir -p "$build"
tar -xJf "$tarball" -C "$build"
for p in "$here"/patches/mesa/*.patch; do
    patch -d "$build/mesa-$MESA_VERSION" -p1 --forward --quiet < "$p"
    echo "applied $(basename "$p")"
done
write_cross_file "$build/cross.ini" "$sysroot"

libdir=lib/aarch64-linux-gnu
meson setup "$build/out" "$build/mesa-$MESA_VERSION" --cross-file "$build/cross.ini" \
    --prefix="$USERLAND/usr" --libdir="$libdir" \
    --buildtype=release -Db_ndebug=true \
    -Dvulkan-drivers=freedreno -Dfreedreno-kmds=kgsl -Dallow-kcmp=disabled \
    -Dgallium-drivers= -Dplatforms=x11 \
    -Dopengl=false -Dgles1=disabled -Dgles2=disabled -Dglx=disabled -Degl=disabled -Dgbm=disabled \
    -Dllvm=disabled -Dvalgrind=disabled -Dlibunwind=disabled -Dlmsensors=disabled \
    -Dvulkan-layers= -Dtools= -Dbuild-tests=false -Dhtml-docs=disabled \
    -Dexpat=enabled -Dzstd=enabled -Dshader-cache=enabled

ninja -C "$build/out" -j"$(nproc)"
DESTDIR="$dest" meson install -C "$build/out" --strip --no-rebuild >/dev/null

staging="$work/staging"
lib="$USERLAND/usr/$libdir/libvulkan_freedreno.so"
icd="$(find "$dest$USERLAND/usr/share/vulkan/icd.d" -name 'freedreno_icd*.json' | head -n1)"
[[ -f "$dest$lib" && -f "$icd" ]] || { echo "Turnip install incomplete" >&2; exit 1; }
install -D -m 0755 "$dest$lib" "$staging${lib#"$USERLAND"}"
install -D -m 0644 "$icd" "$staging/usr/share/vulkan/icd.d/$(basename "$icd")"
# Turnip's driconf defaults are compiled in; nothing else of the install is needed at run time.

got="$(jq -r .ICD.library_path "$icd")"
[[ "$got" == "$lib" ]] || { echo "ICD library_path is $got, want $lib" >&2; exit 1; }
aarch64-linux-gnu-readelf -d "$staging${lib#"$USERLAND"}" | sed -n 's/.*Shared library: \[\(.*\)\]/turnip needs \1/p'
echo "Turnip $MESA_VERSION installed; ICD $USERLAND/usr/share/vulkan/icd.d/$(basename "$icd") -> $got"
