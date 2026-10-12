#!/usr/bin/env bash
# Builds Mesa's PanVK (Vulkan driver "panfrost") for aarch64 with our kernel backend for Arm's
# mali_kbase (mesa-kbase/), separately from Turnip: its own Mesa pin
# (PANVK_MESA_* in config.sh), image (containers/Containerfile.panvk) and cache (PANVK_CACHE).
#
#   1. the Mesa tree: the pinned archive; patches/mesa/*.patch, the Vulkan WSI series Turnip's build
#      applies too (the software present's cached reads, present counter, pacing by the Game view and
#      direct present into the app's buffers, which falls back to the X screen while our backend
#      imports no dma-buf); then patches/mesa-panvk/*.patch; then our files from mesa-kbase/ and
#      Arm's kbase uAPI headers, fetched at the commit config.sh pins (KBASE_UAPI_*, each file by
#      SHA-256) into include/kbase-uapi/ (copied over the tree; a file is rewritten only when its
#      content changed, so ninja rebuilds only what we touched). A new patch set re-extracts the
#      tree and reconfigures.
#   2. host tools, native amd64, once per Mesa commit: mesa_clc, vtn_bindgen2 and panfrost_compile
#      (Mesa builds PanVK's internal OpenCL shaders with them; a cross build cannot build them itself
#      because they link the build machine's LLVM).
#   3. the driver, cross aarch64 against the userland's staged sysroot, with
#      -Dmesa-clc=system -Dprecomp-compiler=system and the host tools on PATH. Options from Mesa's
#      meson.options; prefix $USERLAND/usr as for Turnip (the ICD's library_path is prefix/libdir).
#
# Outputs in $PANVK_CACHE/out: libvulkan_panfrost.so, panfrost_icd.aarch64.json, BUILDINFO.
# With PD2_PANVK_STAGE=1 they are also installed into the userland staging tree. The debug variant
# (PD2_USERLAND_VARIANT=debug, config.sh), whose prefix differs, has its own: out-debug, and the
# build folders panvk-build-debug and panvk-dest-debug; the Mesa tree and host tools are shared.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=config.sh
. "$here/config.sh"

if [[ "${PD2_USERLAND_IN_CONTAINER:-}" != 1 ]]; then
    RUN_EXTRA=(-e "PD2_PANVK_STAGE=${PD2_PANVK_STAGE:-0}")
    run_in_panvk build-panvk.sh "$@"
    exit
fi

kb=/kbase
work="$kb/work"
sysroot=/cache/work/sysroot
[[ -d "$sysroot/usr/include" ]] || { echo "no sysroot: run fetch-debian.sh first" >&2; exit 1; }
out="$kb/out$VARIANT_SUFFIX"
mkdir -p "$kb/sources" "$work" "$out"

# --- 1. the tree -------------------------------------------------------------------------------
tarball="$kb/sources/mesa-$PANVK_MESA_COMMIT.tar.gz"
if [[ ! -f "$tarball" ]]; then
    curl -fsSL --retry 3 -o "$tarball.part" "$PANVK_MESA_URL"
    mv "$tarball.part" "$tarball"
fi
echo "$PANVK_MESA_SHA256  $tarball" | sha256sum -c --quiet - || { echo "SHA-256 mismatch: $tarball" >&2; exit 1; }

# Arm's kbase uAPI headers, unchanged, each checked by SHA-256.
uapi="$kb/sources/kbase-uapi-$KBASE_UAPI_COMMIT"
for entry in "${KBASE_UAPI_FILES[@]}"; do
    sum="${entry%% *}"
    f="${entry##* }"
    if ! echo "$sum  $uapi/$f" | sha256sum -c --quiet - 2>/dev/null; then
        mkdir -p "$(dirname "$uapi/$f")"
        curl -fsSL --retry 3 -o "$uapi/$f.part" "$KBASE_UAPI_URL/$f"
        echo "$sum  $uapi/$f.part" | sha256sum -c --quiet - || { echo "SHA-256 mismatch: kbase uAPI $f" >&2; exit 1; }
        mv "$uapi/$f.part" "$uapi/$f"
    fi
done

# The WSI series shared with Turnip first, then PanVK's own.
patches=()
for set in mesa mesa-panvk; do
    while IFS= read -r p; do patches+=("$p"); done < <(find "$here/patches/$set" -name '*.patch' | sort)
done
stamp="$( (echo "$PANVK_MESA_COMMIT"; for p in "${patches[@]}"; do sha256sum < "$p"; done) | sha256sum | cut -c1-16)"
tree="$work/mesa-tree"
cross="$work/panvk-build$VARIANT_SUFFIX"
if [[ "$(cat "$tree/.pd2-stamp" 2>/dev/null)" != "$stamp" ]]; then
    echo "== preparing the Mesa tree ($PANVK_MESA_COMMIT, patch set $stamp)"
    rm -rf "$tree" "$work"/panvk-build*
    mkdir -p "$tree"
    tar -xzf "$tarball" -C "$tree" --strip-components=1
    for p in "${patches[@]}"; do
        patch -d "$tree" -p1 --forward --quiet < "$p"
        echo "applied ${p#"$here/patches/"}"
    done
    echo "$stamp" > "$tree/.pd2-stamp"
fi
# Our files and the uAPI headers: copy only what differs, keeping unchanged files' timestamps.
(cd "$here/mesa-kbase" && find . -type f) | while IFS= read -r f; do
    if ! cmp -s "$here/mesa-kbase/$f" "$tree/$f"; then
        install -D -m 0644 "$here/mesa-kbase/$f" "$tree/$f"
        echo "updated ${f#./}"
    fi
done
for entry in "${KBASE_UAPI_FILES[@]}"; do
    f="${entry##* }"
    if ! cmp -s "$uapi/$f" "$tree/include/kbase-uapi/$f"; then
        install -D -m 0644 "$uapi/$f" "$tree/include/kbase-uapi/$f"
        echo "updated include/kbase-uapi/$f"
    fi
done

# --- 2. host tools -----------------------------------------------------------------------------
host="$work/host-tools-$PANVK_MESA_COMMIT"
tools=(mesa_clc vtn_bindgen2 panfrost_compile)
have_tools() {
    local t
    for t in "${tools[@]}"; do [[ -x "$host/bin/$t" ]] || return 1; done
}
if ! have_tools; then
    echo "== building the host tools"
    rm -rf "$host"
    mkdir -p "$host/bin"
    cat > "$host/native.ini" <<EOF
[binaries]
llvm-config = '/usr/lib/llvm-19/bin/llvm-config'
EOF
    meson setup "$host/build" "$tree" --native-file "$host/native.ini" \
        --buildtype=release -Db_ndebug=true \
        -Dplatforms= -Dgallium-drivers= -Dvulkan-drivers=panfrost \
        -Dopengl=false -Dgles1=disabled -Dgles2=disabled -Dglx=disabled -Degl=disabled -Dgbm=disabled \
        -Dllvm=enabled -Dshared-llvm=enabled -Dmesa-clc=enabled -Dprecomp-compiler=enabled \
        -Dvulkan-layers= -Dtools= -Dbuild-tests=false -Dvalgrind=disabled -Dlibunwind=disabled \
        -Dlmsensors=disabled -Dzstd=disabled -Dxmlconfig=disabled -Dshader-cache=disabled \
        -Dhtml-docs=disabled -Dpanfrost-rust=false >/dev/null
    ninja -C "$host/build" src/compiler/clc/mesa_clc src/compiler/spirv/vtn_bindgen2 \
        src/panfrost/clc/panfrost_compile
    install -m 0755 "$host/build/src/compiler/clc/mesa_clc" "$host/build/src/compiler/spirv/vtn_bindgen2" \
        "$host/build/src/panfrost/clc/panfrost_compile" "$host/bin/"
    rm -rf "$host/build"
fi
export PATH="$host/bin:$PATH"

# --- 3. the driver -----------------------------------------------------------------------------
libdir=lib/aarch64-linux-gnu
if [[ ! -f "$cross/build.ninja" ]]; then
    echo "== configuring PanVK"
    rm -rf "$cross"
    mkdir -p "$cross"
    write_cross_file "$work/panvk-cross$VARIANT_SUFFIX.ini" "$sysroot"
    meson setup "$cross" "$tree" --cross-file "$work/panvk-cross$VARIANT_SUFFIX.ini" \
        --prefix="$USERLAND/usr" --libdir="$libdir" \
        --buildtype=release -Db_ndebug=true \
        -Dvulkan-drivers=panfrost -Dgallium-drivers= -Dplatforms=x11 \
        -Dopengl=false -Dgles1=disabled -Dgles2=disabled -Dglx=disabled -Degl=disabled -Dgbm=disabled \
        -Dmesa-clc=system -Dprecomp-compiler=system -Dllvm=disabled -Dpanfrost-rust=false \
        -Dvalgrind=disabled -Dlibunwind=disabled -Dlmsensors=disabled -Dallow-kcmp=disabled \
        -Dvulkan-layers= -Dtools= -Dbuild-tests=false -Dhtml-docs=disabled \
        -Dexpat=enabled -Dzstd=enabled -Dshader-cache=enabled
fi
ninja -C "$cross" -j"$(nproc)"
dest="$work/panvk-dest$VARIANT_SUFFIX"
rm -rf "$dest"
DESTDIR="$dest" meson install -C "$cross" --strip --no-rebuild >/dev/null

lib="$USERLAND/usr/$libdir/libvulkan_panfrost.so"
icd="$(find "$dest$USERLAND/usr/share/vulkan/icd.d" -name 'panfrost_icd*.json' | head -n1)"
[[ -f "$dest$lib" && -f "$icd" ]] || { echo "PanVK install incomplete" >&2; exit 1; }
got="$(jq -r .ICD.library_path "$icd")"
[[ "$got" == "$lib" ]] || { echo "ICD library_path is $got, want $lib" >&2; exit 1; }
install -m 0755 "$dest$lib" "$out/libvulkan_panfrost.so"
install -m 0644 "$icd" "$out/$(basename "$icd")"
{
    echo "mesa_commit=$PANVK_MESA_COMMIT"
    echo "patch_set=$stamp"
    for p in "${patches[@]}"; do echo "patch ${p#"$here/patches/"} $(sha256sum < "$p" | cut -c1-64)"; done
    (cd "$here/mesa-kbase" && find . -type f | sort | while IFS= read -r f; do
        echo "file ${f#./} $(sha256sum < "$f" | cut -c1-64)"; done)
    echo "kbase_uapi_commit=$KBASE_UAPI_COMMIT"
    for entry in "${KBASE_UAPI_FILES[@]}"; do echo "uapi ${entry##* } ${entry%% *}"; done
    echo "libvulkan_panfrost.so $(sha256sum < "$out/libvulkan_panfrost.so" | cut -c1-64)"
} > "$out/BUILDINFO"
rm -rf "$dest"

aarch64-linux-gnu-readelf -d "$out/libvulkan_panfrost.so" | sed -n 's/.*Shared library: \[\(.*\)\]/panvk needs \1/p'
if [[ "${PD2_PANVK_STAGE:-0}" == 1 ]]; then
    staging=/cache/work/staging
    install -D -m 0755 "$out/libvulkan_panfrost.so" "$staging${lib#"$USERLAND"}"
    install -D -m 0644 "$out/$(basename "$icd")" "$staging/usr/share/vulkan/icd.d/$(basename "$icd")"
    echo "PanVK installed into staging"
fi
echo "PanVK built: $out ($(du -h "$out/libvulkan_panfrost.so" | cut -f1)); ICD -> $got"
