# Pins and paths shared by every App userland script. Sourced, never executed.
# shellcheck shell=bash

# The app's data folder as every userland program is built for it: a placeholder of 200 bytes,
# longer than any data folder Android gives an app (/mnt/expand/<uuid>/user/<n>/<package> is about
# 100). The app replaces it with its real data folder as it unpacks the tree (UserlandInstaller):
# in place in a binary file, the real path padded with '/' to the same length (repeated slashes
# name the same path), and freely in a text file. So one userland serves the release app and the
# development build, for any user and on any storage. pack.sh records where the placeholder is.
DATA_PLACEHOLDER="/pd2launcher-data-placeholder-$(printf '%0170d' 0)"
# The prefix every program is built for: the userland's place in the data folder.
USERLAND=$DATA_PLACEHOLDER/files/userland/current

# The libraries the Launcher engine links (its DT_NEEDED). pack.sh puts them and every library they
# need into the manifest's critical set, which the app checks again before each engine start; the
# app's build.sh refuses an engine that needs a library not listed here.
ENGINE_NEEDED=(ld-linux-aarch64.so.1 libc.so.6 libgcc_s.so.1 libm.so.6 libxcb.so.1 libxkbcommon.so.0
    libxkbcommon-x11.so.0)

# debian:trixie-slim, multi-arch index digest (the same pin as ../containers/Containerfile).
DEBIAN_IMAGE=docker.io/library/debian@sha256:a29215f6a35e51e22adffa17f89e9d2ef06214e64a2bad10d765c46aea49f11f

# snapshot.debian.org timestamp for trixie, trixie-updates and trixie-security (Debian 13.7).
DEBIAN_SNAPSHOT=20261006T000000Z

# Mesa release for Turnip (KGSL build); SHA-256 matches docs.mesa3d.org/relnotes/26.2.3.html
MESA_VERSION=26.2.3
MESA_SHA256=1628058a8d2c0615975de5a15ab7bbb9638c50000b5bed9456ff423ea034a81f
MESA_URL="https://archive.mesa3d.org/mesa-$MESA_VERSION.tar.xz"

# Mesa for PanVK on Mali (build-panvk.sh), separate from Turnip's release above: main after the v11
# models (MR !41803), until the 26.3 branch exists; then its releases.
# SHA-256 of GitLab's archive of that commit, recorded on first download.
PANVK_MESA_COMMIT=5bd05bf437123e11ae194df89494a3ab9da1e2ac
PANVK_MESA_SHA256=607946081ba8d9b29b4d02f1fb1e6fae1193ca787f18a840747243f9b3e49755
PANVK_MESA_URL="https://gitlab.freedesktop.org/mesa/mesa/-/archive/$PANVK_MESA_COMMIT/mesa-$PANVK_MESA_COMMIT.tar.gz"

# Arm's kbase uAPI headers (CSF 1.38) for our PanVK backend: fetched
# unchanged by build-panvk.sh from Sky1-Linux/cix-gpu-kmd at this commit (MALI_RELEASE_NAME
# r54p1-11eac0; its CSF headers are byte-identical to the r54p2 driver's) into the Mesa
# tree's include/kbase-uapi/. Each file's SHA-256 was recorded on first download.
KBASE_UAPI_COMMIT=83fd82b869315c1880d34607d14988c2feb7789e
KBASE_UAPI_URL="https://raw.githubusercontent.com/Sky1-Linux/cix-gpu-kmd/$KBASE_UAPI_COMMIT/include/uapi/gpu/arm/midgard"
KBASE_UAPI_FILES=(
    "0627eac3f45fe91791dd6b6f5f729c64639155b991109d00f397038643f231ea csf/mali_base_csf_kernel.h"
    "a724154c6a15a4d207d36d74e5913446d8005ddfb97905a0a907a2aea2646096 csf/mali_kbase_csf_ioctl.h"
    "3423e718a36ec2fc106a5252fa5ba9f588ce6d1b017d27ed720b3648c9234354 csf/mali_kbase_csf_mem_flags.h"
    "be46ef9ac1141a6bd9ec8f564ee20991441abb625c152547cabf16fef666234e gpu/mali_kbase_gpu_coherency.h"
    "926b2676fae62d7d83e16e142c6597a4bfef6d8f77a3a8ce1d9e2a322cc0d916 gpu/mali_kbase_gpu_id.h"
    "b0ffea96fd3607a89c3654e0be2a5eb1b12a7e35c3bf4c9ccc11b5d5f4dbfe59 mali_base_common_kernel.h"
    "8fed617df47772527c71f9b2cb32913211b37e3e8a534be7ff2a3cea8c52c9ac mali_base_kernel.h"
    "b90f768e44f4f8a2943ac860d17806fb01a76b857b20b8186d5d832c37f2d080 mali_base_mem_priv.h"
    "325ee5ed88d4dbf6268b211b71c441a3f9b0f651e5175ffd9925164d418621c3 mali_gpu_props.h"
    "e53ede4fc9760d2fd7af9fbfadf60d9ed390169d7f58f3a6339d96d3325e9839 mali_kbase_ioctl.h"
    "99d7d2d123c7dda45d4d7cf4c8b85336822353a3638738ecc3282334d6ae3d63 mali_kbase_mem_flags.h"
)

# X server for Xvfb, https://www.x.org/releases/: a 21.1 release with X.Org's security fixes
# (Debian 13 ships 21.1.16 with its own backports). The SHA-256 is the one X.Org's release
# announcement publishes.
XSERVER_VERSION=21.1.25
XSERVER_SHA256=6ad4e3c7b59a309b32c92e5c15ce1267110f9e13f1ac78da62361a628da1a0eb
XSERVER_URL="https://www.x.org/releases/individual/xserver/xorg-server-$XSERVER_VERSION.tar.xz"

# Android's own shell, used by Xvfb to run xkbcomp (patches/xserver/0002).
ANDROID_SHELL=/system/bin/sh

# Directory the app creates for compiled keymaps (Xvfb's compile-time XKM output dir).
XKM_OUTPUT_DIR=$DATA_PLACEHOLDER/files/tmp/xkb

# Persistent cache outside the repository.
USERLAND_CACHE="${PD2_USERLAND_CACHE:-$HOME/.cache/pd2-android/userland}"
# PanVK's sources, host tools and build directories (build-panvk.sh), kept apart from the userland's.
PANVK_CACHE="${PD2_PANVK_CACHE:-$HOME/.cache/pd2-android/kbase}"

# Writes the apt sources for the pinned snapshot (deb822) to stdout.
snapshot_sources() {
    cat <<EOF
Types: deb
URIs: http://snapshot.debian.org/archive/debian/$DEBIAN_SNAPSHOT
Suites: trixie trixie-updates
Components: main
Signed-By: /usr/share/keyrings/debian-archive-keyring.gpg
Check-Valid-Until: no

Types: deb
URIs: http://snapshot.debian.org/archive/debian-security/$DEBIAN_SNAPSHOT
Suites: trixie-security
Components: main
Signed-By: /usr/share/keyrings/debian-archive-keyring.gpg
Check-Valid-Until: no
EOF
}

container_engine() {
    if command -v podman >/dev/null 2>&1; then
        echo podman
    elif command -v docker >/dev/null 2>&1; then
        echo docker
    else
        echo "refusing: no container engine (podman or docker) found" >&2
        return 1
    fi
}

USERLAND_SRC="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# The packed userland (pack.sh) and where every app build takes it from.
USERLAND_OUT="$USERLAND_SRC/out"
USERLAND_ASSETS="$(dirname "$USERLAND_SRC")/app/src/main/assets/userland"

# --- helpers used inside the tools container ----------------------------------------------

# Rewrites every absolute symlink under a root into a relative one (resolved against that root),
# so the tree works at any mount point: the sysroot at build time, $USERLAND on the device.
relativize_symlinks() {
    local root="$1" link target dir
    while IFS= read -r -d '' link; do
        target="$(readlink "$link")"
        [[ "$target" == /* ]] || continue
        dir="$(dirname "${link#"$root"}")"
        ln -sfn "$(realpath -m -s --relative-to="$dir" "$target")" "$link"
    done < <(find "$root" -type l -print0)
}

# Writes a meson cross file for aarch64 against the staged sysroot. Extra C arguments optional.
write_cross_file() { # out-file sysroot [extra c_args...]
    local out="$1" sysroot="$2"; shift 2
    local extra="" a
    for a in "$@"; do extra+=", '$a'"; done
    cat > "$out" <<EOF
[binaries]
c = 'aarch64-linux-gnu-gcc'
cpp = 'aarch64-linux-gnu-g++'
ar = 'aarch64-linux-gnu-ar'
nm = 'aarch64-linux-gnu-nm'
strip = 'aarch64-linux-gnu-strip'
pkg-config = 'pkgconf'

[properties]
sys_root = '$sysroot'
pkg_config_libdir = ['$sysroot/usr/lib/aarch64-linux-gnu/pkgconfig', '$sysroot/usr/share/pkgconfig', '$sysroot/usr/lib/pkgconfig']

[built-in options]
c_args = ['--sysroot=$sysroot'$extra]
cpp_args = ['--sysroot=$sysroot']
c_link_args = ['--sysroot=$sysroot']
cpp_link_args = ['--sysroot=$sysroot']

[host_machine]
system = 'linux'
cpu_family = 'aarch64'
cpu = 'aarch64'
endian = 'little'
EOF
}

# Prints the identity of a component build inside a container (build-xvfb.sh, build-turnip.sh,
# build-panvk.sh): the image it builds in (PD2_IMAGE, which names the toolchain and its pins), the
# package lock (the sysroot), the prefix, the cross file, then each argument: a pin as given, a
# file as its path under /src and SHA-256. A build whose recorded identity (its BUILDINFO) equals
# this one is reused instead of built again.
build_identity() { # pin-or-file...
    local a
    echo "image=${PD2_IMAGE:?}"
    echo "packages.lock $(sha256sum < /src/packages.lock | cut -d' ' -f1)"
    echo "prefix=$USERLAND"
    echo "cross-file $(declare -f write_cross_file | sha256sum | cut -d' ' -f1)"
    for a in "$@"; do
        if [[ "$a" == /src/* ]]; then
            echo "${a#/src/} $(sha256sum < "$a" | cut -d' ' -f1)"
        else
            echo "$a"
        fi
    done
}

# Downloads a pinned source tarball into the cache once and checks its SHA-256.
fetch_source() { # url sha256 -> prints the local path
    local url="$1" sha="$2" file
    file="$USERLAND_CACHE/sources/$(basename "$url")"
    mkdir -p "$USERLAND_CACHE/sources"
    if [[ ! -f "$file" ]]; then
        curl -fsSL --retry 3 -o "$file.part" "$url"
        # Checked before it takes the cached name: a bad download is removed, never kept as good.
        echo "$sha  $file.part" | sha256sum -c --quiet - >&2 || { rm -f "$file.part"; echo "SHA-256 mismatch: $url" >&2; return 1; }
        mv "$file.part" "$file"
    fi
    echo "$sha  $file" | sha256sum -c --quiet - >&2 || { echo "SHA-256 mismatch: $file" >&2; return 1; }
    echo "$file"
}

# Image archives: with PD2_IMAGE_ARCHIVE set to a folder (the release workflow keeps it between
# runs), an image that is missing is loaded from its archive there, and one built here is archived
# there: <name, with / and : as _>.tar.zst.
image_archive() { # image -> its archive's path
    echo "$PD2_IMAGE_ARCHIVE/$(tr '/:' '__' <<<"$1").tar.zst"
}

# True when the engine has the image, loading it from its archive if there is one. `image inspect`,
# not Podman's own `image exists`, so Docker works too.
have_image() { # engine image
    "$1" image inspect "$2" >/dev/null 2>&1 && return 0
    [[ -n "${PD2_IMAGE_ARCHIVE:-}" && -f "$(image_archive "$2")" ]] || return 1
    echo "loading $2 from $(image_archive "$2")" >&2
    zstd -dc "$(image_archive "$2")" | "$1" load >&2 && "$1" image inspect "$2" >/dev/null 2>&1
}

# Archives an image just built, when PD2_IMAGE_ARCHIVE is set. Only a saving: a failure leaves no
# archive and the build goes on.
archive_image() { # engine image
    local file
    [[ -n "${PD2_IMAGE_ARCHIVE:-}" ]] || return 0
    file="$(image_archive "$2")"
    if mkdir -p "$PD2_IMAGE_ARCHIVE" && "$1" save "$2" | zstd -T0 -q -f -o "$file.part" &&
        mv "$file.part" "$file"; then
        echo "archived $2 in $file" >&2
    else
        rm -f "$file.part"
        echo "warning: could not archive $2" >&2
    fi
}

# The tools image's name; the tag hashes the Containerfile and its two pins.
tools_image_name() {
    echo "localhost/pd2-userland-tools:$( (cat "$USERLAND_SRC/containers/Containerfile.tools"
        echo "$DEBIAN_IMAGE $DEBIAN_SNAPSHOT") | sha256sum | cut -c1-16)"
}

# Builds (once) and prints the tools image.
tools_image() {
    local engine image
    engine="$(container_engine)"
    image="$(tools_image_name)"
    if ! have_image "$engine" "$image"; then
        "$engine" build --platform linux/amd64 \
            --build-arg "DEBIAN_IMAGE=$DEBIAN_IMAGE" --build-arg "DEBIAN_SNAPSHOT=$DEBIAN_SNAPSHOT" \
            -t "$image" -f "$USERLAND_SRC/containers/Containerfile.tools" "$USERLAND_SRC/containers" >&2
        archive_image "$engine" "$image"
    fi
    echo "$image"
}

# Re-runs the calling script inside the tools image with the sources read-only at /src and
# the cache at /cache (its work folder, /cache/work, holds the staging tree, the sysroot, the build
# folders and records). Extra engine arguments go before the image via RUN_EXTRA (array).
run_in_tools() {
    local script="$1"; shift
    local engine image
    engine="$(container_engine)"
    image="$(tools_image)"
    mkdir -p "$USERLAND_CACHE/work"
    "$engine" run --rm --platform linux/amd64 \
        -v "$USERLAND_SRC:/src:ro" -v "$USERLAND_CACHE:/cache" \
        -e PD2_USERLAND_IN_CONTAINER=1 -e PD2_USERLAND_CACHE=/cache \
        -e "PD2_IMAGE=$image" "${RUN_EXTRA[@]}" "$image" bash "/src/$script" "$@"
}
RUN_EXTRA=()

# The PanVK image's name: the tools image plus LLVM/Clang/libclc for Mesa's host tools
# (containers/Containerfile.panvk). The tag hashes that file and the tools image's name.
panvk_image_name() {
    echo "localhost/pd2-userland-panvk:$( (cat "$USERLAND_SRC/containers/Containerfile.panvk"
        tools_image_name) | sha256sum | cut -c1-16)"
}

# Builds (once) and prints the PanVK image.
panvk_image() {
    local engine tools image
    engine="$(container_engine)"
    tools="$(tools_image)"
    image="$(panvk_image_name)"
    if ! have_image "$engine" "$image"; then
        "$engine" build --platform linux/amd64 --build-arg "TOOLS_IMAGE=$tools" \
            -t "$image" -f "$USERLAND_SRC/containers/Containerfile.panvk" "$USERLAND_SRC/containers" >&2
        archive_image "$engine" "$image"
    fi
    echo "$image"
}

# The names of fex/build.sh's and glibc/build.sh's images: the tags hash their Containerfiles and
# the two pins.
fex_image_name() {
    echo "localhost/pd2-userland-fex:$( (cat "$USERLAND_SRC/fex/Containerfile"
        echo "$DEBIAN_IMAGE $DEBIAN_SNAPSHOT") | sha256sum | cut -c1-16)"
}
glibc_image_name() {
    echo "localhost/pd2-glibc-build:$( (cat "$USERLAND_SRC/glibc/Containerfile"
        echo "$DEBIAN_IMAGE $DEBIAN_SNAPSHOT") | sha256sum | cut -c1-16)"
}

# Like run_in_tools, in the PanVK image, with PANVK_CACHE at /kbase as well.
run_in_panvk() {
    local script="$1"; shift
    local engine image
    engine="$(container_engine)"
    image="$(panvk_image)"
    mkdir -p "$USERLAND_CACHE/work" "$PANVK_CACHE"
    "$engine" run --rm --platform linux/amd64 \
        -v "$USERLAND_SRC:/src:ro" -v "$USERLAND_CACHE:/cache" -v "$PANVK_CACHE:/kbase" \
        -e PD2_USERLAND_IN_CONTAINER=1 -e PD2_USERLAND_CACHE=/cache -e PD2_PANVK_CACHE=/kbase \
        -e "PD2_IMAGE=$image" \
        "${RUN_EXTRA[@]}" "$image" bash "/src/$script" "$@"
}
