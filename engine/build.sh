#!/usr/bin/env bash
# Puts the PD2 Launcher engine the APK runs, the standalone ARM64 Launcher without its window, into
# the cache where ../build.sh takes it from (paths below are relative to the app's folder, this
# one's parent):
#   engine/build.sh                          builds the engine of this commit, into
#                                            $cache/engine-out/ ($cache/engine-release-out/ and a
#                                            release build with PD2_RELEASE_BUILD=1)
#   engine/build.sh --source                 prints the engine's folder only (runtime/prefix.sh
#                                            reads its XInput bridge there)
#   engine/build.sh --tree                   prints the engine's identity in this commit: the
#                                            SHA-256 of `git ls-tree` of its files (ENGINE_FILES)
#   engine/build.sh --image                  prints the name of the build image only
#   engine/build.sh --identity               prints what a build's engine comes from (its files,
#                                            the image, this script, ENGINE_NEEDED,
#                                            PD2_RELEASE_BUILD), without a build (the part's name)
#   engine/build.sh --attest [<folder>]      runs each engine of the folder (default: the build's
#                                            output folder) that has not attested its identity in
#                                            an arm64 Debian container ($DEBIAN_IMAGE), where it
#                                            attests its standalone ARM64 identity, and writes that
#                                            into its build evidence (package_identity)
# The engine is in the same commit as the app: in the private repository the Launcher's crate is
# at the repository's root (the app is in android/), and in the public Android repository the
# export puts the same files into core/ (the app is at the root). ENGINE_FILES lists them; the
# export's copy.bara.android.sky must list the same files, and scripts/export-android.sh checks
# that the exported core/ has exactly these files of the tag's commit. The build takes the
# committed files only: uncommitted changes of them are refused.
# It builds the Launcher without its window: `cargo build --release --no-default-features`, with
# the release-build feature for PD2_RELEASE_BUILD=1, in the base image
# (../containers/Containerfile.base): Debian 13's cross compiler for aarch64 with its glibc 2.41,
# the C library of the App userland, and the Rust toolchain that rust-toolchain.toml names, which
# rustup installs into $cache/engine. The engine may link only the libraries of ENGINE_NEEDED
# (userland/config.sh). The build runs no ARM64 code, so its evidence says
# package_identity=unattested; the engine attests its standalone ARM64 identity once, on an ARM64
# host or under qemu-user (binfmt_misc with the F flag): a build calls --attest itself where it
# can, and a release APK takes only an attested engine (../build.sh). The executable,
# pd2-launcher-<version>-<commit>-aarch64 (the first 12 hex digits of the commit it was built at),
# gets its build evidence beside it, with engine_tree, which ../build.sh compares with this
# commit's.
# $cache is PD2_ANDROID_CACHE or ~/.cache/pd2-android.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
die() { printf 'engine/build.sh: %s\n' "$*" >&2; exit 1; }

# --- inside the build image ------------------------------------------------------------------
# The sources are at /src (read-only), the toolchain and Cargo's caches in /cache, and the
# engine with its build evidence goes to /out. The environment names the engine (ENGINE_NAME),
# its commit, its identity (ENGINE_TREE), the commit's time, the image and the libraries the
# engine may link (ENGINE_NEEDED).
if [[ "${1:-}" == --compile ]]; then
    [[ "${PD2_ENGINE_IN_CONTAINER:-}" == 1 ]] || die "--compile runs only in the build image"
    : "${ENGINE_NAME:?}" "${GIT_COMMIT:?}" "${ENGINE_TREE:?}" "${SOURCE_DATE_EPOCH:?}" \
        "${BUILD_IMAGE:?}" "${ENGINE_NEEDED:?}"
    export RUSTUP_HOME=/cache/rustup CARGO_HOME=/cache/cargo CARGO_TARGET_DIR=/cache/target
    export PATH="$CARGO_HOME/bin:$PATH"
    if [[ ! -x "$CARGO_HOME/bin/rustup" ]]; then
        rustup-init -y --no-modify-path --profile minimal --default-toolchain none >/dev/null
    fi
    cd /src
    # The toolchain that rust-toolchain.toml names, with the standard library for ARM64. rustup
    # stays the pinned one (../containers/Containerfile.base): no self-update.
    rustup toolchain install --no-self-update >/dev/null
    rustup target add aarch64-unknown-linux-gnu >/dev/null
    export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc
    export CC_aarch64_unknown_linux_gnu=aarch64-linux-gnu-gcc AR_aarch64_unknown_linux_gnu=aarch64-linux-gnu-ar
    export PD2_LAUNCHER_ARTIFACT_IDENTITY=native-aarch64-binary
    features=()
    [[ -n "${PD2_RELEASE_BUILD:-}" ]] && features=(--features release-build)
    cargo build --locked --release --target aarch64-unknown-linux-gnu --no-default-features "${features[@]}"
    engine="/out/$ENGINE_NAME"
    install -m 0755 "$CARGO_TARGET_DIR/aarch64-unknown-linux-gnu/release/pd2-launcher" "$engine"
    touch -d "@$SOURCE_DATE_EPOCH" "$engine"
    needed="$(aarch64-linux-gnu-readelf -d "$engine" | sed -n 's/.*(NEEDED).*\[\(.*\)\]$/\1/p' | tr '\n' ' ')"
    for lib in $needed; do
        [[ " $ENGINE_NEEDED " == *" $lib "* ]] ||
            die "the engine links $lib, which the App userland does not provide (ENGINE_NEEDED in userland/config.sh)"
    done
    glibc="$(aarch64-linux-gnu-readelf -V --wide "$engine" | grep -oE 'GLIBC_[0-9]+(\.[0-9]+)+' |
        sed 's/^GLIBC_//' | sort -uV | tail -n 1)"
    # No ARM64 code runs here: --attest runs the engine later, on an ARM64 host or under qemu-user.
    identity=unattested
    cat > "$engine.build-evidence.txt" <<EVIDENCE
artifact=$ENGINE_NAME
sha256=$(sha256sum < "$engine" | cut -d' ' -f1)
git_commit=$GIT_COMMIT
engine_tree=$ENGINE_TREE
source_date_epoch=$SOURCE_DATE_EPOCH
source_tree=clean
architecture=aarch64
build_host=$(uname -m)
cargo_profile=release
cargo_features=--no-default-features${features[*]:+ ${features[*]}}
release_build=$([[ -n "${PD2_RELEASE_BUILD:-}" ]] && echo 1 || echo 0)
package_identity=$identity
needed=${needed% }
required_glibc=$glibc
rustc=$(rustc --version)
build_image=$BUILD_IMAGE
EVIDENCE
    exit 0
fi

mode=default
case "${1:-}" in
    "") ;;
    --source) mode=source ;;
    --tree) mode=tree ;;
    --image) mode=image ;;
    --identity) mode=identity ;;
    --attest) mode=attest; attest_dir="${2:-}" ;;
    *) echo "usage: engine/build.sh [--source | --tree | --image | --identity | --attest [<folder>]]" >&2; exit 2 ;;
esac

cache="${PD2_ANDROID_CACHE:-$HOME/.cache/pd2-android}"

# The build image: the base image (../containers/base-image.sh).
# shellcheck source=../userland/config.sh
. "$here/../userland/config.sh"
if [[ "$mode" == image ]]; then
    base_image_name
    exit 0
fi
out="$cache/engine-out"
[[ -n "${PD2_RELEASE_BUILD:-}" ]] && out="$cache/engine-release-out"

# Each engine of a folder whose build evidence does not say it attested its identity runs in an
# arm64 Debian container (Debian 13's glibc 2.41, the C library it was built against): natively
# on an aarch64 host, under qemu-user elsewhere. Its evidence then names that identity.
attest() { # folder
    local dir="$1" engine name evidence identity count=0 user_args=()
    dir="$(cd "$dir" && pwd)" || die "--attest: no folder $1"
    [[ "$(basename "$(container_engine)")" == docker ]] && user_args=(--user "$(id -u):$(id -g)")
    for engine in "$dir"/pd2-launcher-*-aarch64; do
        [[ -f "$engine" ]] || continue
        name="$(basename "$engine")"
        evidence="$engine.build-evidence.txt"
        [[ -f "$evidence" ]] || die "--attest: $name has no build evidence"
        grep -qx 'package_identity=native-aarch64-binary' "$evidence" && continue
        [[ "$(sed -n 's/^sha256=//p' "$evidence")" == "$(sha256sum < "$engine" | cut -d' ' -f1)" ]] ||
            die "--attest: $name is not the engine its build evidence names (sha256)"
        identity="$("$(container_engine)" run --rm --platform linux/arm64 --security-opt label=disable "${user_args[@]}" \
            -v "$dir:/e:ro" "$DEBIAN_IMAGE" "/e/$name" --verify-package-identity)" ||
            die "$name did not attest its identity (it runs on an aarch64 host, or under qemu-user for aarch64 in binfmt_misc with the F flag)"
        [[ "$identity" == native-aarch64-binary ]] || die "$name attests $identity, not native-aarch64-binary"
        sed -i 's/^package_identity=.*/package_identity=native-aarch64-binary/' "$evidence"
        echo "$name: attested $identity" >&2
        count=$((count + 1))
    done
    echo "attested: $count engine(s) in $dir" >&2
}
if [[ "$mode" == attest ]]; then
    attest "${attest_dir:-$out}"
    exit 0
fi

# The engine's files, relative to its folder: the Launcher's crate as the window-free build needs
# it, with tools/, the sources of the sealed helpers whose bytes src/ embeds.
ENGINE_FILES=(src tools Cargo.toml Cargo.lock rust-toolchain.toml .cargo/config.toml)
# The repository and the engine's folder in it: the root (the private repository) or core/ (the
# public Android repository).
top="$(git -C "$here" rev-parse --show-toplevel 2>/dev/null)" || die "the app's folder is not in a git checkout"
if [[ -f "$top/core/Cargo.toml" ]]; then
    prefix=core/
elif [[ -f "$top/Cargo.toml" && -d "$top/android" ]]; then
    prefix=""
else
    die "no engine in $top: neither core/Cargo.toml nor Cargo.toml beside android/"
fi
commit="$(git -C "$top" rev-parse HEAD)"
# The engine's files in that commit, as "<mode> <type> <object>\t<path>" lines with paths relative
# to the engine's folder: the same lines in both repositories when the files are the same.
engine_listing() { git -C "$top" ls-tree -r "HEAD:${prefix%/}" -- "${ENGINE_FILES[@]}"; }
[[ -z "$prefix" ]] && engine_listing() { git -C "$top" ls-tree -r HEAD -- "${ENGINE_FILES[@]}"; }
engine_tree="$(engine_listing | sha256sum | cut -d' ' -f1)"
[[ -n "$(engine_listing | head -n 1)" ]] || die "the commit $commit has no engine files in ${prefix:-the root}"

case "$mode" in
    source) echo "$top${prefix:+/${prefix%/}}"; exit 0 ;;
    tree) echo "$engine_tree"; exit 0 ;;
    identity)
        echo "engine_tree=$engine_tree"
        echo "image=$(base_image_name)"
        echo "engine/build.sh $(sha256sum < "$here/build.sh" | cut -d' ' -f1)"
        echo "engine_needed=${ENGINE_NEEDED[*]}"
        echo "release_build=$([[ -n "${PD2_RELEASE_BUILD:-}" ]] && echo 1 || echo 0)"
        exit 0 ;;
esac

[[ -z "$(git -C "$top" status --porcelain --untracked-files=normal -- "${ENGINE_FILES[@]/#/$prefix}")" ]] ||
    die "the engine's files have uncommitted changes: the engine is built from the commit (${ENGINE_FILES[*]} in ${prefix:-the root})"
echo "engine: ${prefix:-the root} of $top at ${commit:0:12} (engine_tree $engine_tree)" >&2
engine="$(container_engine)"
image="$(ensure_base_image)"
mkdir -p "$cache/engine/cargo" "$cache/engine/rustup" "$cache/engine/target" "$out"
# One build at a time uses the toolchain and Cargo's caches.
exec 9>"$cache/engine/build.lock"
flock 9
work="$(mktemp -d "$out/.build.XXXXXX")"
trap 'rm -rf "$work"' EXIT
# The build reads a copy of the commit's engine files, which later changes of the checkout cannot
# change.
mkdir "$work/src"
if [[ -n "$prefix" ]]; then
    git -C "$top" archive --format=tar "HEAD:${prefix%/}" -- "${ENGINE_FILES[@]}" | tar -x -C "$work/src"
else
    git -C "$top" archive --format=tar HEAD -- "${ENGINE_FILES[@]}" | tar -x -C "$work/src"
fi
# The engine's name has its version and its commit: builds of two commits of one version do not
# replace each other in the output folder.
version="$(sed -n 's/^version = "\(.*\)"$/\1/p' "$work/src/Cargo.toml" | head -n 1)"
[[ -n "$version" ]] || die "no package version in the engine's Cargo.toml at $commit"
artifact="pd2-launcher-$version-${commit:0:12}-aarch64"
# The files the build writes are the invoking user's: Docker runs the build as that user (rootless
# Podman maps the container's root to it already).
user_args=()
[[ "$(basename "$engine")" == docker ]] && user_args=(--user "$(id -u):$(id -g)")
"$engine" run --rm --platform linux/amd64 --security-opt label=disable "${user_args[@]}" \
    -v "$work/src:/src:ro" -v "$here:/engine:ro" -v "$work:/out" \
    -v "$cache/engine/cargo:/cache/cargo" -v "$cache/engine/rustup:/cache/rustup" \
    -v "$cache/engine/target:/cache/target" \
    -e PD2_ENGINE_IN_CONTAINER=1 -e HOME=/tmp -e "ENGINE_NAME=$artifact" -e "GIT_COMMIT=$commit" \
    -e "ENGINE_TREE=$engine_tree" -e "BUILD_IMAGE=$image" -e "ENGINE_NEEDED=${ENGINE_NEEDED[*]}" \
    -e "SOURCE_DATE_EPOCH=$(git -C "$top" show -s --format=%ct "$commit")" \
    -e "PD2_LAUNCHER_COMMIT=${commit:0:12}" \
    ${PD2_RELEASE_BUILD:+-e PD2_RELEASE_BUILD=1} \
    "$image" bash /engine/build.sh --compile >&2
mv -f "$work/$artifact.build-evidence.txt" "$out/"
mv -f "$work/$artifact" "$out/"
# ../build.sh takes the newest executable in the folder.
touch "$out/$artifact"
# The attest where ARM64 code can run here (an aarch64 host, or qemu-user with the F flag).
if [[ "$(uname -m)" == aarch64 ]] || grep -qs '^flags: .*F' /proc/sys/fs/binfmt_misc/qemu-aarch64; then
    attest "$out"
else
    echo "engine/build.sh: $artifact has not attested its identity: run engine/build.sh --attest on an ARM64 host" >&2
fi
echo "$out/$artifact"
