#!/usr/bin/env bash
# Cross-builds FEX-2609.1 with our Android patches (patches/*.patch) for aarch64, in a pinned container.
#
#   build.sh               FEX and FEXServer, without thunks
#   build.sh --thunks      also the thunk libraries (host aarch64, guest x86-64 and i686)
#
# Steps: fetch the FEX tag (commit checked), apply the patches, check the generated blocked-syscall
# table, build and run the resolver tests (amd64 and aarch64), cross-build FEX with clang/lld against
# an arm64 Debian 13 sysroot, install, and run `FEXServer --version` and `FEXGetConfig --version` under
# qemu-aarch64.
# The thunks' 32-bit guest libraries are built with toolchain-i686-guest.cmake (clang for i686).
# The prefix is ../config.sh's USERLAND.
# Output: out/ (gitignored): out/userland/ is the install tree relative to the prefix
# (out/userland/usr/bin/FEX ...), out/build.log the full log, out/RESULTS.txt the checks,
# out/BUILDINFO the build identity (tag, commit, thunks, pins, SHA-256 of build.sh, Containerfile,
# toolchain-i686-guest.cmake and each patch), written only when the build succeeds.
set -euo pipefail
export LC_ALL=C

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../config.sh
. "$here/../config.sh"

FEX_TAG=FEX-2609.1
FEX_COMMIT=9fbdc00bd6401aff3b32d79e78ff98b8a13e4dcf
FEX_REPO=https://github.com/FEX-Emu/FEX.git
# Submodules the build needs (the test binary repositories and tracy are left out).
FEX_SUBMODULES=(External/vixl Source/Common/cpp-optparse External/fmt External/drm-headers External/xxhash
    External/Catch2 External/Vulkan-Headers External/jemalloc_glibc External/range-v3 External/zydis
    External/unordered_dense External/rpmalloc)

# --- inside the build container -------------------------------------------------------------
if [[ "${PD2_FEX_IN_CONTAINER:-}" == 1 ]]; then
    thunks=OFF
    [[ "${1:-}" == --thunks ]] && thunks=ON
    out=/out
    results="$out/RESULTS.txt"
    rm -f "$out/BUILDINFO"
    : > "$results"
    record() { echo "$*" | tee -a "$results"; }

    # 1. Pristine FEX checkout, cached.
    pristine=/cache/fex/src-$FEX_TAG
    if [[ ! -d "$pristine/.git" ]]; then
        rm -rf "$pristine"
        git clone --quiet --depth 1 --branch "$FEX_TAG" "$FEX_REPO" "$pristine"
        git -C "$pristine" submodule update --quiet --init --depth 1 --jobs 8 "${FEX_SUBMODULES[@]}"
    fi
    commit="$(git -C "$pristine" rev-parse HEAD)"
    [[ "$commit" == "$FEX_COMMIT" ]] || { echo "refusing: $FEX_TAG is $commit, expected $FEX_COMMIT" >&2; exit 1; }
    record "FEX $FEX_TAG = $commit"

    # 2. Patched copy.
    src=/cache/fex/src-patched
    rm -rf "$src"
    cp -a "$pristine" "$src"
    git -C "$src" config user.name build && git -C "$src" config user.email build@localhost
    for patch in /src/fex/patches/*.patch; do
        git -C "$src" apply --whitespace=error-all "$patch"
        record "applied cleanly: $(basename "$patch")"
    done

    # 3. The table in patch 0001 must be exactly what the generator makes from blocked-host-syscalls.txt.
    python3 /src/fex/gen-blocked-host-syscalls.py --blocked /src/fex/blocked-host-syscalls.txt \
        --eperm /src/fex/blocked-host-syscalls-eperm.txt --fex "$src" \
        | diff -u - "$src/Source/Tools/LinuxEmulation/LinuxSyscalls/Android/BlockedHostSyscalls.inl"
    record "blocked-syscall table matches gen-blocked-host-syscalls.py ($(grep -c ANDROID_BLOCKED_HOST_SYSCALL \
        "$src/Source/Tools/LinuxEmulation/LinuxSyscalls/Android/BlockedHostSyscalls.inl") syscalls)"

    # 4. Resolver tests, native and aarch64 (the latter under qemu-user with the arm64 sysroot).
    rt=/cache/fex/resolver-test
    rm -rf "$rt"
    run_resolver_tests() { # <arch> <command...>
        local arch="$1"; shift
        if ! "$@" > "$rt/$arch.out" 2>&1; then
            cat "$rt/$arch.out"
            echo "refusing: resolver tests failed on $arch" >&2
            exit 1
        fi
        record "resolver tests $arch: $(grep 'test cases' "$rt/$arch.out")"
    }
    cmake -S /src/fex/resolver-test -B "$rt/amd64" -G Ninja -DCMAKE_CXX_COMPILER=clang++ -DFEX_SOURCE="$src"
    cmake --build "$rt/amd64"
    run_resolver_tests amd64 "$rt/amd64/InRootResolver"
    cmake -S /src/fex/resolver-test -B "$rt/arm64" -G Ninja -DCMAKE_CXX_COMPILER=clang++ \
        -DCMAKE_SYSTEM_NAME=Linux -DCMAKE_SYSTEM_PROCESSOR=aarch64 -DCMAKE_CXX_COMPILER_TARGET=aarch64-linux-gnu \
        -DCMAKE_SYSROOT=/sysroot -DCMAKE_FIND_ROOT_PATH_MODE_PROGRAM=NEVER -DCMAKE_EXE_LINKER_FLAGS=-fuse-ld=lld \
        -DFEX_SOURCE="$src"
    cmake --build "$rt/arm64"
    run_resolver_tests arm64 qemu-aarch64 -L /sysroot "$rt/arm64/InRootResolver"

    # 5. Cross build.
    build=/cache/fex/build-$thunks
    rm -rf "$build"
    # aarch64 programs run under qemu-user (binfmt, and qemu-aarch64 below) with the sysroot as prefix.
    # Their interpreter is the prefix's loader, so the sysroot gets that path too.
    export QEMU_LD_PREFIX=/sysroot
    mkdir -p "/sysroot$USERLAND/lib"
    ln -sfr /sysroot/usr/lib/aarch64-linux-gnu/ld-linux-aarch64.so.1 "/sysroot$USERLAND/lib/ld-linux-aarch64.so.1"
    thunk_args=()
    if [[ "$thunks" == ON ]]; then
        # thunkgen is built for aarch64 (against the sysroot's libclang) and runs during the build
        # through binfmt qemu-aarch64. The guest libraries use the i386/amd64 packages in / .
        export X86_SYSROOT=/
        thunk_args=(-DBUILD_THUNKS=ON -DX86_DEV_ROOTFS=/
            -DX86_32_TOOLCHAIN_FILE=/src/fex/toolchain-i686-guest.cmake
            -DCMAKE_PREFIX_PATH=/usr/lib/llvm-19/lib/cmake
            -DCMAKE_CROSSCOMPILING_EMULATOR=/usr/bin/qemu-aarch64)
    fi
    SYSROOT=/sysroot cmake -S "$src" -B "$build" -G Ninja \
        -DCMAKE_TOOLCHAIN_FILE="$src/Data/CMake/toolchain_aarch64.cmake" \
        -DCMAKE_BUILD_TYPE=Release \
        -DCMAKE_INSTALL_PREFIX="$USERLAND/usr" \
        -DCMAKE_EXE_LINKER_FLAGS="-fuse-ld=lld -Wl,--dynamic-linker=$USERLAND/lib/ld-linux-aarch64.so.1" \
        -DCMAKE_SHARED_LINKER_FLAGS=-fuse-ld=lld -DCMAKE_MODULE_LINKER_FLAGS=-fuse-ld=lld \
        -DUSE_LINKER=lld -DCMAKE_CXX_SCAN_FOR_MODULES=OFF \
        -DTUNE_CPU=none \
        -DOVERRIDE_VERSION="$FEX_TAG" -DOVERRIDE_HASH="$FEX_COMMIT" \
        -DBUILD_TESTING=OFF -DBUILD_FEXCONFIG=OFF -DENABLE_CCACHE=OFF \
        "${thunk_args[@]}"
    cmake --build "$build" -j "$(nproc)"
    rm -rf "$out/userland" /tmp/destdir
    DESTDIR=/tmp/destdir cmake --install "$build" >/dev/null
    mkdir -p "$out/userland"
    cp -a "/tmp/destdir$USERLAND/." "$out/userland/"
    record "built: thunks=$thunks; installed files:"
    (cd "$out/userland" && find . -type f | sort) | tee -a "$results"

    # 6. Checks on the result.
    fex="$out/userland/usr/bin/FEX"
    record "$(file -b "$fex")"
    record "FEXServer --version (qemu-aarch64): $(qemu-aarch64 "$out/userland/usr/bin/FEXServer" --version 2>&1 | head -3)"
    # FEX itself has no --version option: every argument goes to the guest program.
    record "FEXGetConfig --version (qemu-aarch64): $(qemu-aarch64 "$out/userland/usr/bin/FEXGetConfig" --version 2>&1 | head -1)"
    record "version string in FEX: $(grep -a -o -m1 'FEX-2609\.1' "$fex" | head -1)"

    # 7. Build identity, read by ../build-fex.sh to decide whether out/userland can be reused.
    {
        echo "fex_tag=$FEX_TAG"
        echo "fex_commit=$commit"
        echo "thunks=$thunks"
        echo "debian_image=$DEBIAN_IMAGE"
        echo "debian_snapshot=$DEBIAN_SNAPSHOT"
        echo "prefix=$USERLAND"
        echo "compiler=$(clang++ --version | head -n1)"
        for f in build.sh Containerfile toolchain-i686-guest.cmake; do
            echo "input=$f $(sha256sum < "/src/fex/$f" | cut -d' ' -f1)"
        done
        for p in /src/fex/patches/*.patch; do echo "patch=$(basename "$p") $(sha256sum < "$p" | cut -d' ' -f1)"; done
    } > "$out/BUILDINFO"
    exit 0
fi

# --- on the host ----------------------------------------------------------------------------
engine="$(container_engine)"
context="$(mktemp -d)"
trap 'rm -rf "$context"' EXIT
cp "$here/Containerfile" "$context/"
snapshot_sources > "$context/snapshot.sources"
image="$(fex_image_name)"
if ! have_image "$engine" "$image"; then
    "$engine" build --build-arg "DEBIAN_IMAGE=$DEBIAN_IMAGE" --build-arg "DEBIAN_SNAPSHOT=$DEBIAN_SNAPSHOT" \
        -t "$image" -f "$context/Containerfile" "$context" >&2
    archive_image "$engine" "$image"
fi

out="$here/out"
mkdir -p "$USERLAND_CACHE" "$out"
"$engine" run --rm --platform linux/amd64 \
    -v "$here/..:/src:ro" -v "$USERLAND_CACHE:/cache" -v "$out:/out" \
    -e PD2_FEX_IN_CONTAINER=1 \
    "$image" bash /src/fex/build.sh "$@" 2>&1 | tee "$out/build.log"
exit "${PIPESTATUS[0]}"
