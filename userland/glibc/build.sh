#!/usr/bin/env bash
# Builds our glibc for the App userland: Debian 13's glibc source package with Debian's own
# patch series, then our Android patches (patches/*.patch), cross-compiled for aarch64 in a
# pinned container and installed into out/install (the contents of <U>).
#
# Usage: userland/glibc/build.sh
# Output: userland/glibc/out/install/{lib,usr,etc}, out/BUILDINFO, out/build.log; for the
# debug variant (PD2_USERLAND_VARIANT=debug, ../config.sh) out-debug/ instead of out/.
set -euo pipefail

# Fixed install prefix on the device: ../config.sh's USERLAND for
# the variant, which the host passes into the container.
if [ "${PD2_GLIBC_IN_CONTAINER:-}" != 1 ]; then
    # shellcheck source=../config.sh
    . "$(dirname "${BASH_SOURCE[0]}")/../config.sh"
    U=$USERLAND
else
    U=$PD2_GLIBC_PREFIX
fi

# debian:trixie-slim multi-arch index digest and snapshot (same pins as ../config.sh).
DEBIAN_IMAGE=docker.io/library/debian@sha256:a29215f6a35e51e22adffa17f89e9d2ef06214e64a2bad10d765c46aea49f11f
DEBIAN_SNAPSHOT=20261006T000000Z

# Debian 13 glibc source package; the SHA-256 sums were recorded when it was first fetched
# from the snapshot above and are verified in every build.
GLIBC_DEB_VERSION=2.41-12+deb13u4
GLIBC_DSC=glibc_2.41-12+deb13u4.dsc
GLIBC_DSC_SHA256=0915324aa646bb99abfaa9aa6ecb734b30babd28454a3e51f3152912370fbd3e
GLIBC_ORIG=glibc_2.41.orig.tar.xz
GLIBC_ORIG_SHA256=f24aa441021121a79266f0d75242706cab8843a47901fefe74527491807f1998
GLIBC_DEBIAN=glibc_2.41-12+deb13u4.debian.tar.xz
GLIBC_DEBIAN_SHA256=dda4153511bfd543502d18e5bc9323110996fe9c531f14ae3fad6eb17f027c7d

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

if [ "${PD2_GLIBC_IN_CONTAINER:-}" != 1 ]; then
    if command -v podman >/dev/null 2>&1; then engine=podman
    elif command -v docker >/dev/null 2>&1; then engine=docker
    else echo "refusing: no container engine (podman or docker) found" >&2; exit 1; fi
    cache="${PD2_USERLAND_CACHE:-$HOME/.cache/pd2-android/userland}/glibc"
    hash="$( (cat "$HERE/Containerfile"; echo "$DEBIAN_IMAGE $DEBIAN_SNAPSHOT") | sha256sum | cut -c1-16)"
    image="localhost/pd2-glibc-build:$hash"
    if ! "$engine" image exists "$image" 2>/dev/null; then
        "$engine" build --platform linux/amd64 \
            --build-arg "DEBIAN_IMAGE=$DEBIAN_IMAGE" --build-arg "DEBIAN_SNAPSHOT=$DEBIAN_SNAPSHOT" \
            -t "$image" -f "$HERE/Containerfile" "$HERE" >&2
    fi
    mkdir -p "$cache/src" "$HERE/out$VARIANT_SUFFIX"
    exec "$engine" run --rm --platform linux/amd64 \
        -v "$HERE:/src:ro" -v "$cache:/cache" -v "$HERE/out$VARIANT_SUFFIX:/out" \
        -e PD2_GLIBC_IN_CONTAINER=1 -e "PD2_GLIBC_PREFIX=$U" -e "JOBS=${JOBS:-$(nproc)}" \
        "$image" bash /src/build.sh
fi

# ---- inside the build container ----
exec > >(tee /out/build.log) 2>&1
echo "== glibc $GLIBC_DEB_VERSION for $U"

src=/cache/src
if [ ! -f "$src/$GLIBC_DSC" ]; then
    echo "== fetching the source package from snapshot.debian.org ($DEBIAN_SNAPSHOT)"
    apt-get update -qq
    (cd "$src" && apt-get source --download-only "glibc=$GLIBC_DEB_VERSION")
fi
echo "== verifying source checksums"
(cd "$src" && sha256sum -c - <<EOF
$GLIBC_DSC_SHA256  $GLIBC_DSC
$GLIBC_ORIG_SHA256  $GLIBC_ORIG
$GLIBC_DEBIAN_SHA256  $GLIBC_DEBIAN
EOF
)

echo "== unpacking (dpkg-source applies Debian's debian/patches/series)"
rm -rf /build && mkdir -p /build
cp "$src/$GLIBC_DSC" "$src/$GLIBC_ORIG" "$src/$GLIBC_DEBIAN" /build/
(cd /build && dpkg-source --no-check -x "$GLIBC_DSC" glibc >/dev/null)
echo "Debian patches: $(grep -cv '^#\|^$' /build/glibc/debian/patches/series) in series, $(wc -l < /build/glibc/.pc/applied-patches) applied"

echo "== applying our patches"
for p in /src/patches/*.patch; do
    echo "-- $(basename "$p")"
    patch -d /build/glibc -p1 --forward --no-backup-if-mismatch --fuzz=0 < "$p"
done

# Kernel headers only (as Debian's debian/sysdeps/linux.mk does), from the cross
# linux-libc-dev, so the cross libc6-dev headers next to them are not seen.
mkdir -p /build/kheaders
for h in linux asm asm-generic; do ln -s "/usr/aarch64-linux-gnu/include/$h" "/build/kheaders/$h"; done

# Directory layout: Debian's multiarch layout below <U> (debian/rules: rtlddir=/lib,
# slibdir=/lib/$(DEB_HOST_MULTIARCH), libdir=/usr/lib/$(DEB_HOST_MULTIARCH)), so the
# Debian 13 arm64 libraries unpacked into <U> are on ld.so's default search path.
mkdir -p /build/obj
cat > /build/obj/configparms <<EOF
BUILD_CC = x86_64-linux-gnu-gcc
BUILD_CXX = x86_64-linux-gnu-g++
CFLAGS = -pipe -O2 -g -fdebug-prefix-map=/build/glibc=.
ASFLAGS = -pipe -O2 -g -fdebug-prefix-map=/build/glibc=.
BUILD_CFLAGS = -O2 -g
LDFLAGS =
BASH := /bin/bash
KSH := /bin/bash
SHELL := /bin/bash
LIBGD = no
bindir = $U/usr/bin
datadir = $U/usr/share
complocaledir = $U/usr/lib/locale
sysconfdir = $U/etc
libexecdir = $U/usr/libexec
rootsbindir = $U/sbin
includedir = $U/usr/include
docdir = $U/usr/share/doc
mandir = $U/usr/share/man
sbindir = $U/usr/sbin
vardbdir = $U/var/lib/misc
libdir = $U/usr/lib/aarch64-linux-gnu
slibdir = $U/lib/aarch64-linux-gnu
rtlddir = $U/lib
extra_libdir = $U/lib:$U/usr/lib
install_root = /build/stage
EOF

echo "== configure"
# Same options as Debian's debian/rules.d/build.mk and debian/sysdeps/arm64.mk, except:
# prefix/directories (above), no --with-selinux (only nscd uses it), and no --enable-kernel:
# the aarch64 minimum 3.7.0 (sysdeps/unix/sysv/linux/aarch64/configure.ac:4) applies, which
# is also what Debian's --enable-kernel=3.2 resolves to on arm64.
cd /build/obj
CC="aarch64-linux-gnu-gcc-14 -mbranch-protection=standard -U_FILE_OFFSET_BITS -U_TIME_BITS" \
CXX="aarch64-linux-gnu-g++-14 -mbranch-protection=standard -U_FILE_OFFSET_BITS -U_TIME_BITS" \
AUTOCONF=false MAKEINFO=: \
/build/glibc/configure \
    --host=aarch64-linux-gnu --build=x86_64-linux-gnu \
    --prefix="$U/usr" \
    --without-selinux \
    --disable-crypt \
    --enable-bind-now \
    --enable-fortify-source \
    --enable-stackguard-randomization \
    --enable-stack-protector=strong \
    --enable-multi-arch \
    --enable-memory-tagging \
    --with-pkgversion="Debian GLIBC $GLIBC_DEB_VERSION + pd2launcher Android patches" \
    --with-headers=/build/kheaders \
    | tail -n 5
grep -E '^(arch_minimum_kernel|minimum_kernel)|__LINUX_KERNEL_VERSION' config.make config.h 2>/dev/null || true

echo "== make -j$JOBS"
make -j"$JOBS" > /build/make.log 2>&1 || { tail -n 60 /build/make.log; exit 1; }
tail -n 3 /build/make.log

echo "== install"
make install install_root=/build/stage > /build/install.log 2>&1 || { tail -n 60 /build/install.log; exit 1; }
rm -rf /out/install
mv "/build/stage$U" /out/install
mkdir -p /out/install/etc
# The syscall numbers the tests map the Android lists onto.
cp /build/glibc/sysdeps/unix/sysv/linux/aarch64/arch-syscall.h /out/arch-syscall.h

{
    echo "glibc_debian_version=$GLIBC_DEB_VERSION"
    echo "debian_image=$DEBIAN_IMAGE"
    echo "debian_snapshot=$DEBIAN_SNAPSHOT"
    echo "prefix=$U"
    echo "compiler=$(aarch64-linux-gnu-gcc-14 --version | head -n1)"
    echo "kernel_headers=$(dpkg-query -W -f '${Version}' linux-libc-dev-arm64-cross)"
    for p in /src/patches/*.patch; do echo "patch=$(basename "$p") $(sha256sum < "$p" | cut -d' ' -f1)"; done
} > /out/BUILDINFO
echo "== installed into out/install:"
ls -l /out/install/lib /out/install/lib/aarch64-linux-gnu | head -n 20
file -L /out/install/lib/ld-linux-aarch64.so.1 /out/install/lib/aarch64-linux-gnu/libc.so.6
