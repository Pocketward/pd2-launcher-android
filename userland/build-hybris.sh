#!/usr/bin/env bash
# Builds the userland parts of the libhybris route (GraphicsRoute.LIBHYBRIS): the game's GL goes to
# the product Mesa's Zink, the Khronos loader and our ICD libvulkan_pd2hybris.so (hybris/), which
# loads the phone's own Vulkan driver (/vendor/lib64/hw/vulkan.*.so, bionic) into the game's glibc
# process through libhybris.
# One cross build for aarch64 in the libhybris image (containers/Containerfile.hybris, from the
# Mesa image):
#   1. libhybris at $LIBHYBRIS_COMMIT with patches/libhybris/*.patch (stock firmware: the TLS access
#      patcher, the bionic TLS area, the slot-1 shadow, ELF TLS, CFI stubs, BoringSSL), against
#      Halium's android-headers at $HYBRIS_HEADERS_COMMIT; only hybris/common: libhybris-common.so.1
#      (the hooks) and its linker plugin q.so (bionic's linker of Android 10, which reads the
#      phone's /linkerconfig). Built for the prefix, so its own linker folder is the userland's
#      usr/lib/aarch64-linux-gnu/libhybris/linker; the route also names it (HYBRIS_LINKER_DIR).
#   2. libvulkan_pd2hybris.so from hybris/pd2hybris_icd.c (our code), linked -z nodelete, and its
#      ICD JSON pd2hybris_icd.aarch64.json.
# The licence texts and a README of what is in it go to usr/share/licenses/pd2-libhybris/.
# The installed files join the userland's part "system-driver" (work/parts/, pack.sh), which an
# APK can leave out (build.sh PD2_SYSTEM_DRIVER=0): run after build-sysdrv.sh, whose --install
# writes that list anew.
# Outputs in the cache's hybris-out/ with BUILDINFO (the image, the pins, the prefix, this script,
# the patches, the ICD's sources); a later run with the same identity installs without building.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=config.sh
. "$here/config.sh"

mesa_image_name() {
    echo "localhost/pd2-userland-mesa:$( (cat "$USERLAND_SRC/containers/Containerfile.mesa"
        echo "$DEBIAN_IMAGE $DEBIAN_SNAPSHOT") | sha256sum | cut -c1-16)"
}

# The libhybris image: the Mesa image plus autotools and the arm64 Vulkan headers; the tag hashes
# Containerfile.hybris and the Mesa image's name.
hybris_image() {
    local engine mesa image
    engine="$(container_engine)"
    mesa="$(mesa_image_name)"
    image="localhost/pd2-userland-hybris:$( (cat "$USERLAND_SRC/containers/Containerfile.hybris"; echo "$mesa") | sha256sum | cut -c1-16)"
    if ! have_image "$engine" "$image"; then
        if ! have_image "$engine" "$mesa"; then
            "$engine" build --platform linux/amd64 \
                --build-arg "DEBIAN_IMAGE=$DEBIAN_IMAGE" --build-arg "DEBIAN_SNAPSHOT=$DEBIAN_SNAPSHOT" \
                -t "$mesa" -f "$USERLAND_SRC/containers/Containerfile.mesa" "$USERLAND_SRC/containers" >&2
            archive_image "$engine" "$mesa"
        fi
        "$engine" build --platform linux/amd64 --build-arg "MESA_IMAGE=$mesa" \
            -t "$image" -f "$USERLAND_SRC/containers/Containerfile.hybris" "$USERLAND_SRC/containers" >&2
        archive_image "$engine" "$image"
    fi
    echo "$image"
}

if [[ "${PD2_USERLAND_IN_CONTAINER:-}" != 1 ]]; then
    fetch_source "$LIBHYBRIS_URL" "$LIBHYBRIS_SHA256" >/dev/null
    fetch_source "$HYBRIS_HEADERS_URL" "$HYBRIS_HEADERS_SHA256" >/dev/null
    image="$(hybris_image)"
    engine="$(container_engine)"
    mkdir -p "$USERLAND_CACHE/work"
    "$engine" run --rm --platform linux/amd64 \
        -v "$USERLAND_SRC:/src:ro" -v "$USERLAND_CACHE:/cache" \
        -e PD2_USERLAND_IN_CONTAINER=1 -e PD2_USERLAND_CACHE=/cache -e "PD2_IMAGE=$image" \
        "$image" bash /src/build-hybris.sh --build
    run_in_tools build-hybris.sh --install
    exit
fi

out=/cache/hybris-out
libdir=lib/aarch64-linux-gnu
licenses=usr/share/licenses/pd2-libhybris
identity() {
    echo "image=${PD2_IMAGE:-}"
    echo "libhybris=$LIBHYBRIS_COMMIT $LIBHYBRIS_SHA256"
    echo "android-headers=$HYBRIS_HEADERS_COMMIT $HYBRIS_HEADERS_SHA256"
    echo "prefix=$USERLAND"
    for f in /src/build-hybris.sh /src/patches/libhybris/*.patch /src/hybris/*; do
        echo "${f#/src/} $(sha256sum < "$f" | cut -d' ' -f1)"
    done
}

if [[ "${1:-}" == --build ]]; then
    id="$(identity)"
    if [[ -d "$out/tree" && "$(cat "$out/BUILDINFO" 2>/dev/null)" == "$id" ]]; then
        echo "reusing hybris-out (BUILDINFO matches)"
        exit 0
    fi
    work=/cache/work/hybris
    rm -rf "$work" "$out"
    mkdir -p "$work/libhybris" "$work/android-headers" "$out/tree"
    tar -xzf "/cache/sources/$(basename "$LIBHYBRIS_URL")" -C "$work/libhybris" --strip-components=1
    tar -xzf "/cache/sources/$(basename "$HYBRIS_HEADERS_URL")" -C "$work/android-headers" --strip-components=1
    for p in /src/patches/libhybris/*.patch; do
        patch -d "$work/libhybris" -p1 --forward --fuzz=0 --quiet < "$p"
        echo "libhybris: applied ${p#/src/patches/}"
    done

    # --- 1. libhybris: hybris/common only -------------------------------------------------------
    cd "$work/libhybris/hybris"
    NOCONFIGURE=1 ./autogen.sh > "$work/autogen.log" 2>&1 || { tail -40 "$work/autogen.log"; exit 1; }
    # The default library path is the stock Android 16 layout, used only where the q linker reads
    # no /linkerconfig (it takes the phone's own namespaces where it does).
    ./configure --host=aarch64-linux-gnu --enable-arch=arm64 \
        --with-android-headers="$work/android-headers" \
        --prefix="$USERLAND/usr" --libdir="$USERLAND/usr/$libdir" --with-default-egl-platform=null \
        --with-default-hybris-ld-library-path=/vendor/lib64/hw:/vendor/lib64:/system/lib64:/apex/com.android.runtime/lib64/bionic:/apex/com.android.runtime/lib64 \
        CFLAGS="-O2 -g" CXXFLAGS="-O2 -g" > "$work/configure.log" 2>&1 || { tail -60 "$work/configure.log"; exit 1; }
    make -C common -j"$(nproc)" > "$work/make.log" 2>&1 || { grep -i -E "error" "$work/make.log" | head -40; exit 1; }
    install -D -m 0755 common/.libs/libhybris-common.so.1 "$out/tree/usr/$libdir/libhybris-common.so.1"
    install -D -m 0755 common/q/.libs/q.so "$out/tree/usr/$libdir/libhybris/linker/q.so"
    aarch64-linux-gnu-strip --strip-unneeded "$out/tree/usr/$libdir/libhybris-common.so.1" "$out/tree/usr/$libdir/libhybris/linker/q.so"
    # The linker folder compiled in (hooks.c LINKER_PLUGIN_DIR) is the userland's own.
    grep -a -q "$USERLAND/usr/$libdir/libhybris/linker" "$out/tree/usr/$libdir/libhybris-common.so.1" ||
        { echo "libhybris-common.so.1 does not name $USERLAND/usr/$libdir/libhybris/linker" >&2; exit 1; }

    # --- 2. our ICD -----------------------------------------------------------------------------
    aarch64-linux-gnu-gcc -O2 -g -Wall -Wextra -Wno-unused-parameter -fPIC -shared -fvisibility=hidden \
        -Wl,-soname,libvulkan_pd2hybris.so -Wl,--no-undefined -Wl,-z,nodelete \
        -o "$out/tree/usr/$libdir/libvulkan_pd2hybris.so" /src/hybris/pd2hybris_icd.c \
        -L"$out/tree/usr/$libdir" -l:libhybris-common.so.1 -lX11-xcb -lX11 -lxcb -lpthread -ldl
    aarch64-linux-gnu-strip --strip-unneeded "$out/tree/usr/$libdir/libvulkan_pd2hybris.so"
    mkdir -p "$out/tree/usr/share/vulkan/icd.d"
    printf '{\n    "file_format_version": "1.0.1",\n    "ICD": {\n        "library_path": "%s",\n        "api_version": "1.4.303"\n    }\n}\n' \
        "$USERLAND/usr/$libdir/libvulkan_pd2hybris.so" > "$out/tree/usr/share/vulkan/icd.d/pd2hybris_icd.aarch64.json"

    # --- licences -------------------------------------------------------------------------------
    mkdir -p "$out/tree/$licenses"
    for l in LICENSE.Apache2 LICENSE.BSD-2 LICENSE.ISC; do
        install -m 0644 "$work/libhybris/$l" "$out/tree/$licenses/$l"
    done
    {
        echo "libhybris in this app's userland (the libhybris route: the phone's own Vulkan driver)."
        echo
        echo "Source: https://github.com/libhybris/libhybris commit $LIBHYBRIS_COMMIT"
        echo "with these changes (userland/patches/libhybris in the app's source):"
        for p in /src/patches/libhybris/*.patch; do
            echo "  ${p##*/}: $(sed -n 's/^From: //p' "$p" | head -n1)"
        done
        echo "Built against Halium's android-headers commit $HYBRIS_HEADERS_COMMIT (headers only)."
        echo
        echo "Files: libhybris-common.so.1 (hybris/common: Apache-2.0, LICENSE.Apache2; strlcpy.c and"
        echo "strlcat.c: ISC, LICENSE.ISC), libhybris/linker/q.so (hybris/common/q, a copy of the"
        echo "Android Open Source Project's bionic linker: BSD-2-Clause, LICENSE.BSD-2). The"
        echo "copyright notices of the source files built:"
        grep -h -o -E "Copyright (\(c\)|\(C\)|©)? ?[0-9][0-9, -]*[^*]*" \
            "$work"/libhybris/hybris/common/*.c "$work"/libhybris/hybris/common/*.cpp \
            "$work"/libhybris/hybris/common/q/*.cpp "$work"/libhybris/hybris/common/q/*.h \
            "$work"/libhybris/hybris/common/legacy_properties/*.c 2>/dev/null |
            sed 's/[[:space:]]*$//' | LC_ALL=C sort -u | sed 's/^/  /'
        echo
        echo "libvulkan_pd2hybris.so and pd2hybris_icd.aarch64.json: this app's own code (its licence)."
    } > "$out/tree/$licenses/README"

    rm -rf "$work"
    echo "$id" > "$out/BUILDINFO"
    exit 0
fi

# --install (tools image): the build into the staging tree, as part of "system-driver".
staging=/cache/work/staging
[[ -d "$staging/usr" ]] || { echo "no staging tree: run fetch-debian.sh first" >&2; exit 1; }
[[ -f "$out/BUILDINFO" && -d "$out/tree" ]] || { echo "no libhybris build: run build-hybris.sh" >&2; exit 1; }
cp -a "$out/tree/." "$staging/"
mkdir -p /cache/work/parts
list=/cache/work/parts/system-driver.files
{ [[ -f "$list" ]] && cat "$list"; (cd "$out/tree" && find . -type f | sed 's|^\./||'); } | LC_ALL=C sort -u > "$list.new"
mv "$list.new" "$list"
icd="$staging/usr/share/vulkan/icd.d/pd2hybris_icd.aarch64.json"
want="$USERLAND/usr/$libdir/libvulkan_pd2hybris.so"
got="$(jq -r .ICD.library_path "$icd")"
[[ "$got" == "$want" ]] || { echo "libhybris ICD library_path is $got, want $want" >&2; exit 1; }
aarch64-linux-gnu-readelf -d "$staging/usr/$libdir/libvulkan_pd2hybris.so" | grep -q 'Shared library: \[libhybris-common.so.1\]' ||
    { echo "libvulkan_pd2hybris.so does not need libhybris-common.so.1" >&2; exit 1; }
echo "libhybris installed: ${LIBHYBRIS_COMMIT:0:12} + $(ls /src/patches/libhybris | wc -l) patches, ICD -> $got, linker $USERLAND/usr/$libdir/libhybris/linker/q.so"
