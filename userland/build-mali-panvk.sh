#!/usr/bin/env bash
# Builds the community PanVK tree mali-panvk for Arm's Mali GPUs on the stock kbase driver, for the
# App userland's glibc, into the system-driver part (pack.sh), beside RADV for Xclipse. It never
# replaces our PanVK (build-panvk.sh, usr/lib/aarch64-linux-gnu/libvulkan_panfrost.so): its files
# have their own folders, and its ICD JSON is outside icd.d, so only a VK_DRIVER_FILES that names
# it loads it.
#
# Source: config.sh's MALI_PANVK_* pins.
#   - JimVulkan/mali-panvk at MALI_PANVK_COMMIT (MIT), a Mesa 27.0.0-devel subset (Mesa main
#     d1e369d8..f28352d1, 2026-10-17/19) with its own kbase backend (job manager and CSF Malis),
#     PanVK for Valhall v9 on the job manager, and geometry, tessellation, transform feedback and
#     BCn emulation; built by its authors for Android's bionic.
#   - Upstream Mesa's files that the subset leaves out and a glibc X11 build needs
#     (MALI_PANVK_UPSTREAM_FILES, from the tree's base commit, each checked by SHA-256).
#   - radv-compat/ as include/xclipse_linux_compat: the tree has radv-xclipse's util and Vulkan
#     runtime, whose meson.build adds that folder to glibc builds and whose u_xclipse_prof.c
#     includes <android/log.h> and <sys/system_properties.h>; the tree does not carry the folder.
#   - patches/radv-xclipse/*.patch, our WSI series as ported to radv-xclipse: this tree's four WSI
#     files are byte-identical to radv-xclipse's at RADV_XC_COMMIT, so the same patches apply
#     exactly (fuzz 0, no offset; the script refuses anything else).
#   - patches/mali-panvk/*.patch, our fixes for this tree (a model, a hash, a kernel signal, the
#     dma-buf size, and on the job manager a lost device, time limits, room for open submissions,
#     and callbacks safe after a lost device; the direct present's blit recorded for each present and
#     its dma-buf on a MediaTek gralloc; see
#     their README.md), after the WSI series and in the same exact way. The tree itself stays as
#     it is: it changes only through these patches.
# Built in the PanVK image (containers/Containerfile.panvk: LLVM 19 for the host tools), with no
# network in the container (the sources are fetched and checked first): the host tools
# (mesa_clc, vtn_bindgen2, panfrost_compile) natively from this tree, as its OpenCL kernels are
# its own; then the driver, cross aarch64 against the userland's staged sysroot (as build-panvk.sh),
# PanVK only, the X11 window system (Xvfb; the app sets MESA_VK_WSI_DEBUG=sw), no Android
# platform and so no IMapper gralloc, no LLVM, kcmp off (Android's seccomp policy).
#
# Output: the cache's mali-panvk-out/tree (usr/lib/aarch64-linux-gnu/mali-panvk/libvulkan_panfrost.so,
# usr/share/mali-panvk/mali_panvk_icd.aarch64.json naming it at the userland's prefix, and in
# usr/share/licenses/mali-panvk/ the tree's LICENSE, android/NOTICE.txt, THIRD_PARTY_NOTICES.md and
# COPYRIGHT-HOLDERS.txt: the copyright lines of the tree's own files that its NOTICE.txt does not
# name) with BUILDINFO; --install (tools image) copies it into the staging tree and adds its files
# to the system-driver part's list. The source tree, the host tools and the configured build
# folder stay in MALI_PANVK_CACHE/work: a later run with the same SETUP reuses them, and when only
# the patches change, it restores the tree's files that the patches change, applies the patches
# again, and ninja rebuilds only what uses them. Each step prints a line; a failure prints its line and
# command (the ERR trap).
set -Eeuo pipefail
trap 'echo "build-mali-panvk.sh${PD2_USERLAND_IN_CONTAINER:+ (in the image)}: FAILED at line $LINENO: $BASH_COMMAND (exit $?)" >&2' ERR

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=config.sh
. "$here/config.sh"

say() { echo "[build-mali-panvk $(date +%T)] $*"; }
fail() { echo "build-mali-panvk.sh: $*" >&2; exit 1; }

if [[ "${PD2_USERLAND_IN_CONTAINER:-}" != 1 ]]; then
    fetch_source "$MALI_PANVK_URL" "$MALI_PANVK_SHA256" >/dev/null
    fetch_source "$MALI_PANVK_UPSTREAM_URL" "$MALI_PANVK_UPSTREAM_SHA256" >/dev/null
    say "sources checked"
    image="$(panvk_image)"
    engine="$(container_engine)"
    mkdir -p "$USERLAND_CACHE/work" "$MALI_PANVK_CACHE"
    # No network in the container: the sources are fetched and checked above.
    "$engine" run --rm --network=none --platform linux/amd64 \
        -v "$USERLAND_SRC:/src:ro" -v "$USERLAND_CACHE:/cache" -v "$MALI_PANVK_CACHE:/mali" \
        -e PD2_USERLAND_IN_CONTAINER=1 -e PD2_USERLAND_CACHE=/cache -e "PD2_IMAGE=$image" \
        "$image" bash /src/build-mali-panvk.sh --build
    RUN_EXTRA=(-v "$MALI_PANVK_CACHE:/mali")
    run_in_tools build-mali-panvk.sh --install
    exit
fi

out=/mali/mali-panvk-out
libdir=lib/aarch64-linux-gnu
lib=usr/$libdir/mali-panvk/libvulkan_panfrost.so
icd=usr/share/mali-panvk/mali_panvk_icd.aarch64.json
# Not usr/share/doc: relocate.sh drops that folder.
doc=usr/share/licenses/mali-panvk

if [[ "${1:-}" == --build ]]; then
    sysroot=/cache/work/sysroot
    [[ -d "$sysroot/usr/include" ]] || fail "no sysroot: run fetch-debian.sh first"
    src=/mali/work/src
    build=/mali/work/build
    host=/mali/work/host-tools
    tree_archive="/cache/sources/$(basename "$MALI_PANVK_URL")"
    upstream_archive="/cache/sources/$(basename "$MALI_PANVK_UPSTREAM_URL")"
    MESON_OPTS=(
        --prefix="$USERLAND/usr" --libdir="$libdir" --buildtype=release -Db_ndebug=true
        -Dvulkan-drivers=panfrost -Dgallium-drivers= -Dplatforms=x11
        -Dopengl=false -Dgles1=disabled -Dgles2=disabled -Dglx=disabled -Degl=disabled -Dgbm=disabled
        -Dmesa-clc=system -Dprecomp-compiler=system -Dllvm=disabled -Dpanfrost-rust=false
        -Dvalgrind=disabled -Dlibunwind=disabled -Dlmsensors=disabled -Dallow-kcmp=disabled
        -Dvulkan-layers= -Dtools= -Dbuild-tests=false -Dhtml-docs=disabled -Dvideo-codecs=
        -Dexpat=enabled -Dzstd=enabled -Dshader-cache=enabled -Dperfetto=false
    )
    HOST_OPTS=(
        --buildtype=release -Db_ndebug=true
        -Dplatforms= -Dgallium-drivers= -Dvulkan-drivers=panfrost
        -Dopengl=false -Dgles1=disabled -Dgles2=disabled -Dglx=disabled -Degl=disabled -Dgbm=disabled
        -Dllvm=enabled -Dshared-llvm=enabled -Dmesa-clc=enabled -Dprecomp-compiler=enabled
        -Dvulkan-layers= -Dtools= -Dbuild-tests=false -Dvalgrind=disabled -Dlibunwind=disabled
        -Dlmsensors=disabled -Dzstd=disabled -Dxmlconfig=disabled -Dshader-cache=disabled
        -Dhtml-docs=disabled -Dpanfrost-rust=false -Dvideo-codecs= -Dperfetto=false
    )
    # Our patches, in this order: the WSI series of radv-xclipse, then our fixes for this tree.
    PATCHES=(/src/patches/radv-xclipse/*.patch /src/patches/mali-panvk/*.patch)
    # The tree's files that they change (each patch changes files of the tree; it adds none).
    mapfile -t PATCHED_FILES < <(sed -n 's|^+++ b/\([^[:space:]]*\).*|\1|p' "${PATCHES[@]}" | LC_ALL=C sort -u)
    # What the source tree, the host tools and the configured build folder come from (the image,
    # the package lock, the prefix, the pins, the options, the compat headers); another value needs
    # a new setup.
    setup_identity() {
        build_identity "mali-panvk=$MALI_PANVK_COMMIT $MALI_PANVK_SHA256" \
            "upstream=$MALI_PANVK_UPSTREAM_COMMIT $MALI_PANVK_UPSTREAM_SHA256"
        printf '%s\n' "${MALI_PANVK_UPSTREAM_FILES[@]}" "${MESON_OPTS[@]}" "${HOST_OPTS[@]}"
        (cd /src && find radv-compat -type f | LC_ALL=C sort | xargs sha256sum)
    }
    sid="$(setup_identity)"
    pid="$(cd /src && find patches/radv-xclipse patches/mali-panvk -type f -name '*.patch' | LC_ALL=C sort | xargs sha256sum)"
    # What the output comes from: the setup, the patches and this script.
    id="$sid
$pid
build-mali-panvk.sh $(sha256sum < /src/build-mali-panvk.sh | cut -d' ' -f1)"
    # (Files are read only when they exist: a failing command in $(...) would trip the ERR trap.)
    if [[ -d "$out/tree" && -f "$out/BUILDINFO" && "$(< "$out/BUILDINFO")" == "$id" ]]; then
        say "reusing mali-panvk-out (BUILDINFO matches)"
        exit 0
    fi
    if [[ -f "$build/out/build.ninja" && -f "$build/SETUP" && "$(< "$build/SETUP")" == "$sid" ]]; then
        say "reusing the configured build folder (SETUP matches): ninja builds only what changed"
    else
        say "new source tree, host tools and build folder"
        rm -rf "$src" "$build" "$host"
        mkdir -p "$src" "$build" "$host/bin"
        tar -xzf "$tree_archive" -C "$src" --strip-components=1
        [[ ! -e "$src/include/xclipse_linux_compat" ]] || fail "the tree has include/xclipse_linux_compat now: check radv-compat/"
        cp -r /src/radv-compat "$src/include/xclipse_linux_compat"
        upstream_paths=()
        for entry in "${MALI_PANVK_UPSTREAM_FILES[@]}"; do
            [[ ! -e "$src/${entry#* }" ]] || fail "the tree has ${entry#* } now: check MALI_PANVK_UPSTREAM_FILES"
            upstream_paths+=("mesa-$MALI_PANVK_UPSTREAM_COMMIT/${entry#* }")
        done
        tar -xzf "$upstream_archive" -C "$src" --strip-components=1 "${upstream_paths[@]}"
        for entry in "${MALI_PANVK_UPSTREAM_FILES[@]}"; do
            echo "${entry%% *}  $src/${entry#* }" | sha256sum -c --quiet - || fail "SHA-256 mismatch: ${entry#* }"
        done
        say "upstream files restored: ${#MALI_PANVK_UPSTREAM_FILES[@]}"

        cat > "$host/native.ini" <<EOF
[binaries]
llvm-config = '/usr/lib/llvm-19/bin/llvm-config'
EOF
        say "host tools: meson setup (log: $host/setup.log)"
        meson setup "$host/build" "$src" --native-file "$host/native.ini" --wrap-mode=nodownload "${HOST_OPTS[@]}" \
            > "$host/setup.log" 2>&1 || { tail -60 "$host/setup.log" || true; fail "host tools: meson setup failed (above: the end of $host/setup.log)"; }
        say "host tools: ninja (log: $host/ninja.log)"
        ninja -C "$host/build" src/compiler/clc/mesa_clc src/compiler/spirv/vtn_bindgen2 \
            src/panfrost/clc/panfrost_compile > "$host/ninja.log" 2>&1 ||
            { grep -E 'error|Error' "$host/ninja.log" | head -40 || true; tail -30 "$host/ninja.log" || true; fail "host tools: ninja failed (above: errors from $host/ninja.log)"; }
        install -m 0755 "$host/build/src/compiler/clc/mesa_clc" "$host/build/src/compiler/spirv/vtn_bindgen2" \
            "$host/build/src/panfrost/clc/panfrost_compile" "$host/bin/"
        rm -rf "$host/build"

        # -Dmesa-clc=system and -Dprecomp-compiler=system take the host tools from PATH.
        write_cross_file "$build/cross.ini" "$sysroot"
        say "meson setup (log: $build/setup.log)"
        PATH="$host/bin:$PATH" meson setup "$build/out" "$src" --cross-file "$build/cross.ini" --wrap-mode=nodownload \
            "${MESON_OPTS[@]}" > "$build/setup.log" 2>&1 ||
            { tail -60 "$build/setup.log" || true; fail "meson setup failed (above: the end of $build/setup.log)"; }
        echo "$sid" > "$build/SETUP"
    fi

    # The patches, when the tree does not have this set yet: the tree's own files that they change
    # back (with the time of now, so ninja rebuilds what uses them), then each patch, exactly.
    if [[ -f "$src/.pd2-patches" && "$(< "$src/.pd2-patches")" == "$pid" ]]; then
        say "patches: already in the tree"
    else
        say "patches: the tree's ${#PATCHED_FILES[@]} files that they change restored"
        rm -f "$src/.pd2-wsi-patches" "$src/.pd2-patches"
        tar -xzf "$tree_archive" -C "$src" --strip-components=1 --touch \
            "${PATCHED_FILES[@]/#/mali-panvk-$MALI_PANVK_COMMIT/}"
        for p in "${PATCHES[@]}"; do
            patch -d "$src" -p1 --forward --fuzz=0 --no-backup-if-mismatch < "$p" > "$build/patch.log" 2>&1 ||
                { cat "$build/patch.log"; fail "$(basename "$p") does not apply to the tree"; }
            if grep -q -E 'offset|fuzz' "$build/patch.log"; then
                cat "$build/patch.log"
                fail "$(basename "$p") applies only with an offset: refresh it for the tree"
            fi
            say "patch applied: $(basename "$(dirname "$p")")/$(basename "$p")"
        done
        echo "$pid" > "$src/.pd2-patches"
    fi

    rm -rf "$out"
    mkdir -p "$out/tree/$(dirname "$lib")" "$out/tree/$(dirname "$icd")" "$out/tree/$doc"
    say "ninja: src/panfrost/vulkan/libvulkan_panfrost.so (log: $build/ninja.log)"
    PATH="$host/bin:$PATH" ninja -C "$build/out" -j"$(nproc)" src/panfrost/vulkan/libvulkan_panfrost.so > "$build/ninja.log" 2>&1 ||
        { grep -E 'error|Error' "$build/ninja.log" | head -40 || true; tail -30 "$build/ninja.log" || true; fail "ninja failed (above: errors from $build/ninja.log)"; }
    aarch64-linux-gnu-strip --strip-unneeded -o "$out/tree/$lib" "$build/out/src/panfrost/vulkan/libvulkan_panfrost.so"
    chmod 0755 "$out/tree/$lib"
    # The ICD JSON: the API version of the tree's Vulkan headers (1.4 on the CSF Malis, 1.3 on the
    # others: the driver reports its own); the library at the userland's prefix (the app relocates
    # it with the rest).
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
    # The licence (MIT) and the notices of what is compiled into the driver; then the copyright
    # lines of the tree's own files (not in its base) whose holder NOTICE.txt does not name: MIT
    # wants each such notice with the copies.
    install -m 0644 "$src/LICENSE" "$out/tree/$doc/LICENSE"
    install -m 0644 "$src/android/NOTICE.txt" "$out/tree/$doc/NOTICE.txt"
    install -m 0644 "$src/THIRD_PARTY_NOTICES.md" "$out/tree/$doc/THIRD_PARTY_NOTICES.md"
    tar -tzf "$upstream_archive" | sed -n "s|^mesa-$MALI_PANVK_UPSTREAM_COMMIT/||p" | LC_ALL=C sort > "$build/upstream.list"
    (cd "$src" && find src include -type f | LC_ALL=C sort) | LC_ALL=C comm -23 - "$build/upstream.list" > "$build/own.list"
    {
        echo "Copyright notices in the files of mali-panvk $MALI_PANVK_COMMIT that are not in its Mesa base"
        echo "($MALI_PANVK_UPSTREAM_COMMIT) and whose holder NOTICE.txt does not name. MIT (LICENSE)."
        echo
        while IFS= read -r f; do
            while IFS= read -r line; do
                holder="$(sed -E 's/^Copyright *(\([cC]\)|©)? *[0-9, -]* *//' <<<"$line")"
                if [[ -n "$holder" ]] && ! grep -qF "$holder" "$src/android/NOTICE.txt"; then
                    echo "$f: $line"
                fi
            done < <(head -20 "$src/$f" | grep -I -o 'Copyright.*' || true)
        done < "$build/own.list"
    } > "$out/tree/$doc/COPYRIGHT-HOLDERS.txt"
    say "copyright lines NOTICE.txt does not name: $(($(wc -l < "$out/tree/$doc/COPYRIGHT-HOLDERS.txt") - 3))"
    aarch64-linux-gnu-readelf -d "$out/tree/$lib" > "$build/readelf.txt"
    sed -n 's/.*(NEEDED).*\[\(.*\)\]$/\1/p' "$build/readelf.txt" > "$out/needed.txt"
    [[ -s "$out/needed.txt" ]] || fail "readelf found no NEEDED entry in $lib"
    echo "$id" > "$out/BUILDINFO"
    say "built: $(stat -c %s "$out/tree/$lib") bytes; ICD api_version 1.4.$hv; needs: $(tr '\n' ' ' < "$out/needed.txt")"
    exit 0
fi

# --install (tools image): the build into the staging tree; its files join the system-driver part.
staging=/cache/work/staging
[[ -d "$staging/usr" ]] || fail "no staging tree: run fetch-debian.sh first"
[[ -f "$out/BUILDINFO" && -d "$out/tree" ]] || fail "no mali-panvk build: run build-mali-panvk.sh"
parts=/cache/work/parts/system-driver.files
[[ -f "$parts" ]] || fail "no system-driver part ($parts): build-sysdrv.sh runs first (build.sh)"
cp -a "$out/tree/." "$staging/"
# The part's list stays sorted (pack.sh checks it); a run again adds nothing twice.
(cat "$parts"; cd "$out/tree" && find . -type f | sed 's|^\./||') | LC_ALL=C sort -u > "$parts.new"
mv "$parts.new" "$parts"
want="$USERLAND/$lib"
got="$(jq -r .ICD.library_path "$staging/$icd")"
[[ "$got" == "$want" ]] || fail "mali-panvk ICD library_path is $got, want $want"
for needed in $(cat "$out/needed.txt"); do
    found="$(find "$staging/lib/aarch64-linux-gnu" "$staging/usr/lib/aarch64-linux-gnu" -maxdepth 1 -name "$needed" -print -quit 2>/dev/null)"
    [[ -n "$found" ]] || fail "$lib needs $needed, which the staging tree does not have"
done
say "mali-panvk installed: ${MALI_PANVK_COMMIT:0:12}, ICD $icd -> $got, in the system-driver part"
