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

# The libraries the Launcher engine may link (its DT_NEEDED): glibc's and libgcc_s, as it has no
# window. pack.sh puts them and every library they need into the manifest's critical set, which the
# app checks again before each engine start; the app's build.sh refuses an engine that needs a
# library not listed here.
ENGINE_NEEDED=(ld-linux-aarch64.so.1 libc.so.6 libgcc_s.so.1 libm.so.6)

# debian:trixie-slim, multi-arch index digest (the same pin as ../containers/Containerfile).
DEBIAN_IMAGE=docker.io/library/debian@sha256:a29215f6a35e51e22adffa17f89e9d2ef06214e64a2bad10d765c46aea49f11f

# snapshot.debian.org timestamp for trixie, trixie-updates and trixie-security (Debian 13.7).
DEBIAN_SNAPSHOT=20261006T000000Z

# Mesa: one commit of Mesa's development branch (main) for everything we build from Mesa: Zink,
# GLX, EGL and Turnip (build-mesa.sh), PanVK (build-panvk.sh) and the system-driver mode's Venus
# ICD (build-sysdrv.sh; its GL stack is MESA_SYSDRV_* below). Main drives more GPUs than the
# releases (Adreno 710 and 720: 4db0b15c; Adreno 722: 803731f7) and has fixes before a release
# has them. mesa-update.sh moves the pin to a newer commit and refreshes our patches; each app
# release keeps the commit it was tested with (docs/BUILDING.md, "Moving the Mesa pin").
# MESA_SHA256 is that of GitLab's archive of the commit, recorded on first download (the archive's
# tar is byte-identical to `git archive --prefix=mesa-<commit>/ <commit>`); MESA_VERSION is the
# tree's VERSION file.
MESA_COMMIT=1c3a936490f9f0951188218e136cd03bca497386
MESA_SHA256=5bb135de0ff28b9a05ffb1f25f9320734bcd0c88ff02c3d41c12b919560cb1e8
MESA_VERSION=27.0.0-devel
MESA_URL="https://gitlab.freedesktop.org/mesa/mesa/-/archive/$MESA_COMMIT/mesa-$MESA_COMMIT.tar.gz"

# Mesa for the system-driver mode's GL stack (build-sysdrv.sh: Zink only, in its own folder): the
# last 25.1 release, whose Zink takes Vulkan 1.3 drivers without VK_KHR_maintenance5 (Samsung's
# Xclipse driver) and drivers without robustness2 or dynamic rendering (Arm's Mali drivers before
# r53). SHA-256 as in Mesa's docs/relnotes/25.1.9.rst.
MESA_SYSDRV_VERSION=25.1.9
MESA_SYSDRV_SHA256=412df33a1bb3c785ed698555a3972118a37c458e7accf6ae53f4bb87b3db454a
MESA_SYSDRV_URL="https://archive.mesa3d.org/mesa-$MESA_SYSDRV_VERSION.tar.xz"
# The patches of patches/mesa/ that build-sysdrv.sh also applies to that older Mesa (D2GL's
# sub-image bounds): mesa-update.sh checks that their refresh for a new pin still applies there.
MESA_SYSDRV_GL_SHARED_PATCHES=(0005-teximage-subimage-bounds-without-overflow.patch)

# The Venus protocol headers for the Venus ICD of MESA_COMMIT (build-sysdrv.sh): Mesa main takes them
# from the venus-protocol subproject (subprojects/venus-protocol.wrap, `== 1.1.3`), which
# build-sysdrv.sh unpacks into the wrap's folder, so meson needs no network. The same release as
# the server's (devdriver/build.sh VPROTO_*), so both sides speak one protocol. SHA-256 recorded on
# first download.
VENUS_PROTOCOL_VERSION=v1.1.3
VENUS_PROTOCOL_SHA256=c5a362ca28e0a35042db1baeadaf9183500b216841e49d72714b80bae50b4445
VENUS_PROTOCOL_URL="https://gitlab.freedesktop.org/virgl/venus-protocol/-/archive/$VENUS_PROTOCOL_VERSION/venus-protocol-$VENUS_PROTOCOL_VERSION.tar.gz"

# libhybris for the libhybris route (build-hybris.sh): the
# phone's own Vulkan driver (/vendor/lib64/hw/vulkan.*.so, bionic) loaded into the game's glibc
# process through libhybris's copy of bionic's linker, under our ICD (hybris/pd2hybris_icd.c).
# Upstream master of 2026-06-24 (the glibc side of Halium's halium-13.0 branch, which adds only
# Android-side compat/ layers), with patches/libhybris/ (stock-firmware TLS: TheKit's TLS access
# patcher, upstream draft PR #575, and eight commits of tawc's fork wmww/libhybris), and Halium's
# android-headers (halium-16.0) to build against. Apache-2.0 (hybris/common), BSD-2-Clause (the q
# linker). SHA-256s of GitHub's archives, recorded on first download
# (two downloads gave the same bytes).
LIBHYBRIS_COMMIT=7079712a42ea2754adf747e70c6cc75764c8596e
LIBHYBRIS_SHA256=5ea95bd6596bfac95414f911289dee8fbf8a7e076d274e07fa338d335b0205dc
LIBHYBRIS_URL="https://github.com/libhybris/libhybris/archive/$LIBHYBRIS_COMMIT.tar.gz"
HYBRIS_HEADERS_COMMIT=217a38e92cc64a96b4d1ac7469f30827d84485d3
HYBRIS_HEADERS_SHA256=14668ac24a4fde9f17211b8649b23e6755777f18af15c8a703e5034a4ebbadb6
HYBRIS_HEADERS_URL="https://github.com/Halium/android-headers/archive/$HYBRIS_HEADERS_COMMIT.tar.gz"

# RADV for Samsung's Xclipse GPUs (build-radv.sh; the system-driver part, the RADV route of
# DriverChoice): the community tree JimVulkan/radv-xclipse (MIT: its LICENSE; Mesa's files keep
# their own headers, its THIRD_PARTY_NOTICES.md), a Mesa 27.0.0-devel tree (Mesa main of
# 2026-10-19/20) with the changes Xclipse needs (a GFX11 shader core under a GFX10.3 command
# processor, the 940's register map, the sgpu kernel's family 147), built for Android's bionic by
# its authors and here for our glibc. Its own pin, not MESA_COMMIT: mesa-update.sh does not move it.
# GitHub's archive of the commit; SHA-256 recorded on first download.
RADV_XC_COMMIT=269238e8538c971cbe5195ed3e0013dea59f3bf5
RADV_XC_SHA256=84d1532d5657970431928b1a17e33237920efcf4af42cd6edc5fd81812fb49d9
RADV_XC_URL="https://github.com/JimVulkan/radv-xclipse/archive/$RADV_XC_COMMIT.tar.gz"
# libdrm for that RADV, linked into it statically (Mesa main needs libdrm_amdgpu 2.4.133; Debian 13
# has 2.4.124): the tree's subprojects/libdrm.wrap, which also applies the tree's two libdrm
# patches. The release archive of dri.freedesktop.org; the SHA-256 of that wrap (and of upstream
# Mesa's own). MIT.
RADV_XC_LIBDRM_VERSION=2.4.133
RADV_XC_LIBDRM_SHA256=fc68f9d0ba2ea63c9432a299e14fea09fad7a8a66e8039fcd7802ca59f77b4f5
RADV_XC_LIBDRM_URL="https://dri.freedesktop.org/libdrm/libdrm-$RADV_XC_LIBDRM_VERSION.tar.xz"
# Upstream Mesa's files that the tree leaves out and a glibc build with the X11 window system
# needs (MIT), unchanged, each checked by SHA-256: src/x11 (libloader_x11, which src/vulkan/wsi
# links), the headers that src/loader/loader.c includes, and those that vulkan.h includes for the
# X11 platforms (vulkan_beta.h for VK_ENABLE_BETA_EXTENSIONS, off here). From Mesa main
# 5e4bcb1c (its own pin: each file is the same blob at both ends of the tree's base range and
# there; the tree's vulkan_core.h and vulkan.h equal 5e4bcb1c's).
RADV_XC_UPSTREAM_COMMIT=5e4bcb1c791fcfa6e9257e3fef33314cf6f7b92a
RADV_XC_UPSTREAM_SHA256=2a96067f6dcd0b824cf16d09f3de9c5018a60ec564612e6255c899935d2167de
RADV_XC_UPSTREAM_URL="https://gitlab.freedesktop.org/mesa/mesa/-/archive/$RADV_XC_UPSTREAM_COMMIT/mesa-$RADV_XC_UPSTREAM_COMMIT.tar.gz"
RADV_XC_UPSTREAM_FILES=(
    "80e1a6c04f74923a171cdd4c6b4722c985d1978f6f07e1fa782cc02dfef1a382 src/x11/meson.build"
    "7dfc4b0917a422eb35b2e009c31d53d72afd7c1ff121f13df309879713251e8a src/x11/x11_display.c"
    "cef99b1f81ef0d1cc1ffad37f0bf928dc5e920c8f5da996860200afe9019f5bb src/x11/x11_display.h"
    "59143e2a8fa5a6140d4d2e20c3ea48cc646ef589de68a2896eae8327d0825ec3 src/x11/x11_dri3.c"
    "240e06e947c5968140de6e54b21256feb11c1138e2b80195091d1eff49e03e13 src/x11/x11_dri3.h"
    "caa14dca3538c78feb0fb5b0b151c107af7e5e2facbc265f67752faf46237d6b include/drm-uapi/nouveau_drm.h"
    "8c70284a774f1927030bb4ff62b0348924c072ec3ae96637e6c4ae6d13b962c8 include/pci_ids/i915_pci_ids.h"
    "403b0aba8533e423b6ec2beb6c65a593db1ea4f31f16b4dcd1f543a8df0e95ff include/pci_ids/crocus_pci_ids.h"
    "4e2e92a8b495c6d66f6c6e3189e5ec56bc05769a85e6170a87638be5cc9281e0 include/pci_ids/r300_pci_ids.h"
    "bdb01e52b27160e9edc088c1978d25af8d0457112d3d1f342b577be72e36c472 include/pci_ids/r600_pci_ids.h"
    "af593227d90c491dcd9045afed82ba09d3bef9714db03b5d3979977969240c8b include/pci_ids/virtio_gpu_pci_ids.h"
    "64fe3924184d37dc2c8be995d2dbd3251d1a2139519307b917b3c2c8e536e6bb include/pci_ids/vmwgfx_pci_ids.h"
    "3d49f5eb52090e72e1cf7cde545088225ac524a2ff1d1f35737e7d944474c1b7 include/vulkan/vulkan_xcb.h"
    "3c44e97d3f380eb912e01a79e9667457fdf5e18164f0b09b3c61431f421bc551 include/vulkan/vulkan_xlib.h"
    "188233d112d812cca1777cfa6c1585073f1f2b033fa08b2f11c8d0757fcc232e include/vulkan/vulkan_xlib_xrandr.h"
    "bb43577a445c357c3f0c03572b3dd3ebd9fca39914c0092f7d77e942af05d3fc include/vulkan/vulkan_beta.h"
)

# PanVK for Mali GPUs on the stock kbase driver (build-mali-panvk.sh; the system-driver part,
# beside our PanVK, which it never replaces): the community tree JimVulkan/mali-panvk (MIT: its
# LICENSE; Mesa's files keep their headers, its THIRD_PARTY_NOTICES.md), a Mesa 27.0.0-devel
# subset (Mesa main of 2026-10-17/19) with its own kbase backend for the job-manager Malis
# (Bifrost v6/v7, Valhall v9) and the CSF ones (v10 and later), built for Android's bionic by its
# authors and here for our glibc. It has radv-xclipse's util, Vulkan runtime and WSI. Its own pin,
# not MESA_COMMIT: mesa-update.sh does not move it. GitHub's archive of the commit; SHA-256
# recorded on first download (two downloads gave the same bytes).
MALI_PANVK_COMMIT=d896b0a9c10b92f0f9d53ef3373006b79ed709e8
MALI_PANVK_SHA256=23ba9a34f9a3a40fc4265d4a488bfd4acd3757e54fd82e548cc7b1d4fd5b6342
MALI_PANVK_URL="https://github.com/JimVulkan/mali-panvk/archive/$MALI_PANVK_COMMIT.tar.gz"
# Upstream Mesa's files that the subset leaves out and a glibc build with the X11 window system
# needs (MIT), unchanged, each checked by SHA-256: src/x11, src/loader, the headers that loader.c
# includes, and those that vulkan.h includes for the X11 platforms. From the tree's base: Mesa main
# f28352d1 (2026-10-19), the last commit of the range d1e369d8..f28352d1 at which 1525 of the
# tree's 1611 Mesa files are the same blob (no other commit of main has more).
MALI_PANVK_UPSTREAM_COMMIT=f28352d1179a4a85b4be286a883747bae2d3786b
MALI_PANVK_UPSTREAM_SHA256=b1dd44c4048ea7b510557f063d5cbd240174243bc45417c08b2bde83e71a483c
MALI_PANVK_UPSTREAM_URL="https://gitlab.freedesktop.org/mesa/mesa/-/archive/$MALI_PANVK_UPSTREAM_COMMIT/mesa-$MALI_PANVK_UPSTREAM_COMMIT.tar.gz"
MALI_PANVK_UPSTREAM_FILES=(
    "80e1a6c04f74923a171cdd4c6b4722c985d1978f6f07e1fa782cc02dfef1a382 src/x11/meson.build"
    "7dfc4b0917a422eb35b2e009c31d53d72afd7c1ff121f13df309879713251e8a src/x11/x11_display.c"
    "cef99b1f81ef0d1cc1ffad37f0bf928dc5e920c8f5da996860200afe9019f5bb src/x11/x11_display.h"
    "59143e2a8fa5a6140d4d2e20c3ea48cc646ef589de68a2896eae8327d0825ec3 src/x11/x11_dri3.c"
    "240e06e947c5968140de6e54b21256feb11c1138e2b80195091d1eff49e03e13 src/x11/x11_dri3.h"
    "1416f0e150e463a9d76f5c2028bc70e8fbb3a2ce2292af4544fa0501e6a77463 src/loader/loader.c"
    "5140d2656c9d5eced5a337b879250bfbc58de400026caeb380c4a618b849a839 src/loader/loader_dri_helper.c"
    "e6ef0659162e0d4b12a16bfeb3d7473d5845114527d4405849ed2533da46b5f9 src/loader/loader_dri_helper.h"
    "fe5559e336f17d13a814111583cbaa3b0302275db699c0d2e3dea9c07cc17480 src/loader/loader_dri_helper_screen.h"
    "290f243d90e9d13e1e4238218e90fcac453c288895aea24893e7fe1c2f6b23cf src/loader/loader.h"
    "dec592915f1826f43f5616d2b2f3cb20db7fa619ce451ab3b33772b5215c862f src/loader/loader_wayland_helper.c"
    "84d28496a93221984351a05be20b7d3205adb8a3c1051577b90f5ba73becdbc0 src/loader/loader_wayland_helper.h"
    "6ccca1fca9d6bdedb595f7f4b7d58dc269947ed205f675e612d8d524742eb271 src/loader/meson.build"
    "110cbc086bd0990d97ab61516249506eaf3f1325b0d5dd27e4743a893f1992d4 src/loader/pci_id_driver_map.h"
    "caa14dca3538c78feb0fb5b0b151c107af7e5e2facbc265f67752faf46237d6b include/drm-uapi/nouveau_drm.h"
    "8c70284a774f1927030bb4ff62b0348924c072ec3ae96637e6c4ae6d13b962c8 include/pci_ids/i915_pci_ids.h"
    "403b0aba8533e423b6ec2beb6c65a593db1ea4f31f16b4dcd1f543a8df0e95ff include/pci_ids/crocus_pci_ids.h"
    "4e2e92a8b495c6d66f6c6e3189e5ec56bc05769a85e6170a87638be5cc9281e0 include/pci_ids/r300_pci_ids.h"
    "bdb01e52b27160e9edc088c1978d25af8d0457112d3d1f342b577be72e36c472 include/pci_ids/r600_pci_ids.h"
    "af593227d90c491dcd9045afed82ba09d3bef9714db03b5d3979977969240c8b include/pci_ids/virtio_gpu_pci_ids.h"
    "64fe3924184d37dc2c8be995d2dbd3251d1a2139519307b917b3c2c8e536e6bb include/pci_ids/vmwgfx_pci_ids.h"
    "3d49f5eb52090e72e1cf7cde545088225ac524a2ff1d1f35737e7d944474c1b7 include/vulkan/vulkan_xcb.h"
    "3c44e97d3f380eb912e01a79e9667457fdf5e18164f0b09b3c61431f421bc551 include/vulkan/vulkan_xlib.h"
    "188233d112d812cca1777cfa6c1585073f1f2b033fa08b2f11c8d0757fcc232e include/vulkan/vulkan_xlib_xrandr.h"
    "bb43577a445c357c3f0c03572b3dd3ebd9fca39914c0092f7d77e942af05d3fc include/vulkan/vulkan_beta.h"
)
# Its sources' work folders and its output (build-mali-panvk.sh), apart from our PanVK's.
MALI_PANVK_CACHE="${PD2_MALI_PANVK_CACHE:-$HOME/.cache/pd2-android/mali-panvk}"

# PanVK on Mali (build-panvk.sh) builds from the same Mesa commit.
PANVK_MESA_COMMIT=$MESA_COMMIT
PANVK_MESA_SHA256=$MESA_SHA256
PANVK_MESA_URL=$MESA_URL

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
# The asset folder of the userland's parts that an APK can leave out (pack.sh), one
# userland-<name>/ each; the app build adds it unless built without the system-driver mode.
USERLAND_PART_ASSETS="$(dirname "$USERLAND_SRC")/app/system-driver/assets"

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

# Prints the identity of a component build inside a container (build-xvfb.sh, build-mesa.sh,
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

# Downloads a pinned source tarball into the cache once and checks its SHA-256. A cached file
# that does not match (for example the bytes of an archive that GitLab later compressed anew,
# after the pin's SHA-256 changed for the same file name) is downloaded once more.
fetch_source() { # url sha256 -> prints the local path
    local url="$1" sha="$2" file
    file="$USERLAND_CACHE/sources/$(basename "$url")"
    mkdir -p "$USERLAND_CACHE/sources"
    if [[ -f "$file" ]] && ! echo "$sha  $file" | sha256sum -c --quiet - >/dev/null 2>&1; then
        echo "cached $(basename "$file") does not match its SHA-256: downloading it again" >&2
        rm -f "$file"
    fi
    if [[ ! -f "$file" ]]; then
        curl -fsSL --retry 3 -o "$file.part" "$url"
        # Checked before it takes the cached name: a bad download is removed, never kept as good.
        echo "$sha  $file.part" | sha256sum -c --quiet - >&2 || { rm -f "$file.part"; echo "SHA-256 mismatch: $url" >&2; return 1; }
        mv "$file.part" "$file"
    fi
    # Both ways here checked the file: the cached one above, a download before its mv.
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

# The name of glibc/build.sh's image: the tag hashes its Containerfile and the two pins.
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
