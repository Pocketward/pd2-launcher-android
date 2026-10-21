#!/usr/bin/env bash
# Puts the PD2 Launcher engine the APK runs, the standalone ARM64 Launcher without its window, into
# the cache where ../build.sh takes it from (paths below are relative to the app's folder, this
# one's parent):
#   engine/build.sh                          builds the engine from the sources engine.lock pins,
#                                            into $cache/engine-out/ ($cache/engine-release-out/
#                                            and a release build with PD2_RELEASE_BUILD=1)
#   engine/build.sh --source                 prints the engine's source folder only
#                                            (runtime/prefix.sh reads its XInput bridge there)
#   engine/build.sh --image                  prints the name of the build image only
#   engine/build.sh --pin <tag|commit> [url] sets url, tag, commit and source_sha256 in the pin
#                                            file from the repository; a commit pins no tag
# The engine's sources are not in this repository. They come from the desktop Launcher's public
# repository at the commit that engine.lock pins, fetched into $cache/engine/source by the pinned
# tag, which must name that commit, and checked against source_sha256: the SHA-256 of the lines
# that sha256sum prints for every file of the commit, sorted by path. A pin without a tag, and
# with PD2_ENGINE_UNRELEASED=1 a tag that does not exist yet, fetches the commit by its SHA. A
# private repository's sources are fetched with Git's credentials.
# The sources must have the Launcher's `gui` Cargo feature (its window), which the build leaves
# out: `cargo build --release --no-default-features`, with the release-build feature for
# PD2_RELEASE_BUILD=1. It builds a copy of the commit's files in the image of Containerfile (this
# folder): Debian 13's cross compiler for aarch64 with its glibc 2.41, the C library of the App
# userland, and the Rust toolchain that the sources name (rust-toolchain.toml), which rustup
# installs into $cache/engine. The engine may link only the libraries of ENGINE_NEEDED
# (userland/config.sh), and it must attest its standalone ARM64 identity once under qemu-user
# (binfmt_misc with the F flag). The executable, pd2-launcher-<version>-<commit>-aarch64 (the first
# 12 hex digits of the commit), gets its build evidence beside it.
# A pin of the earlier form, which names a release's executable (asset, sha256), is refused: that
# executable has the window, whose libraries the App userland does not have.
# PD2_ENGINE_SOURCE=<folder> builds a local checkout of the Launcher instead, for working on the
# engine: its committed HEAD, so uncommitted changes are refused.
# PD2_ENGINE_LOCK names another pin file. $cache is PD2_ANDROID_CACHE or ~/.cache/pd2-android.
# PD2_IMAGE_ARCHIVE keeps the build image as an archive in a folder (userland/config.sh).
set -euo pipefail
# Never wait for a password prompt.
export GIT_TERMINAL_PROMPT=0

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
die() { printf 'engine/build.sh: %s\n' "$*" >&2; exit 1; }

# --- inside the build image ------------------------------------------------------------------
# The sources are at /src (read-only), the toolchain and Cargo's caches in /cache, and the
# engine with its build evidence goes to /out. The environment names the engine (ENGINE_NAME),
# its commit, the sources' SHA-256 (SOURCES_SHA256), their time, the image and the libraries the
# engine may link (ENGINE_NEEDED).
if [[ "${1:-}" == --compile ]]; then
    [[ "${PD2_ENGINE_IN_CONTAINER:-}" == 1 ]] || die "--compile runs only in the build image"
    : "${ENGINE_NAME:?}" "${GIT_COMMIT:?}" "${SOURCES_SHA256:?}" "${SOURCE_DATE_EPOCH:?}" \
        "${BUILD_IMAGE:?}" "${ENGINE_NEEDED:?}"
    export RUSTUP_HOME=/cache/rustup CARGO_HOME=/cache/cargo CARGO_TARGET_DIR=/cache/target
    export PATH="$CARGO_HOME/bin:$PATH"
    if [[ ! -x "$CARGO_HOME/bin/rustup" ]]; then
        rustup-init -y --no-modify-path --profile minimal --default-toolchain none >/dev/null
    fi
    cd /src
    # The toolchain that rust-toolchain.toml names, with the standard library for ARM64. rustup
    # stays the pinned one (Containerfile): no self-update.
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
    # Run as is, with the cross compiler's glibc, the engine attests its identity by itself.
    identity="$(QEMU_LD_PREFIX=/usr/aarch64-linux-gnu "$engine" --verify-package-identity)" ||
        die "the engine did not attest its identity (it runs under qemu-user for aarch64 in binfmt_misc with the F flag)"
    [[ "$identity" == native-aarch64-binary ]] || die "the engine attests $identity, not native-aarch64-binary"
    cat > "$engine.build-evidence.txt" <<EVIDENCE
artifact=$ENGINE_NAME
sha256=$(sha256sum < "$engine" | cut -d' ' -f1)
git_commit=$GIT_COMMIT
source_sha256=$SOURCES_SHA256
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
    --image) mode=image ;;
    --pin) mode=pin; pin_ref="${2:-}"; pin_url="${3:-}" ;;
    *) echo "usage: engine/build.sh [--source | --image | --pin <tag|commit> [url]]" >&2; exit 2 ;;
esac

cache="${PD2_ANDROID_CACHE:-$HOME/.cache/pd2-android}"
lock="${PD2_ENGINE_LOCK:-$here/../engine.lock}"

# The build image: its tag hashes the Containerfile and the Debian pins.
# shellcheck source=../userland/config.sh
. "$here/../userland/config.sh"
image="localhost/pd2-engine-build:$( (cat "$here/Containerfile"; echo "$DEBIAN_IMAGE $DEBIAN_SNAPSHOT") |
    sha256sum | cut -c1-16)"
if [[ "$mode" == image ]]; then
    echo "$image"
    exit 0
fi

[[ -f "$lock" ]] || die "no pin file $lock"
pinned() { sed -n "s/^$1=//p" "$lock" | head -n 1; }
url="$(pinned url)"
tag="$(pinned tag)"
commit="$(pinned commit)"
source_sha256="$(pinned source_sha256)"
asset="$(pinned asset)"
repository="$(sed -E 's#^https://github\.com/##; s#\.git$##' <<<"$url")"

# The SHA-256 of a folder's files (a checkout's without .git): of the lines that sha256sum prints
# for each, sorted by path.
tree_hash() { # folder
    (cd "$1" && find . -path ./.git -prune -o -type f -printf '%P\0' | LC_ALL=C sort -z |
        xargs -0 -r sha256sum -- | sha256sum | cut -d' ' -f1)
}

# The folder holding the engine's sources, checked out at the pin unless PD2_ENGINE_SOURCE names
# one; checked against source_sha256 when the pin has one.
source_folder() {
    if [[ -n "${PD2_ENGINE_SOURCE:-}" ]]; then
        local dir
        dir="$(cd "$PD2_ENGINE_SOURCE" && pwd -P)" || die "PD2_ENGINE_SOURCE: no folder $PD2_ENGINE_SOURCE"
        [[ -f "$dir/Cargo.toml" && -f "$dir/src/main.rs" && -f "$dir/rust-toolchain.toml" ]] ||
            die "PD2_ENGINE_SOURCE=$dir is not a checkout of the PD2 Launcher"
        echo "$dir"
        return
    fi
    if [[ -z "$url" || ! "$commit" =~ ^[0-9a-f]{40}$ ]]; then
        die "$lock pins no engine sources yet (url and commit): pin them with
  engine/build.sh --pin <tag>
or build from a local checkout of the PD2 Launcher with PD2_ENGINE_SOURCE=<folder>"
    fi
    local src="$cache/engine/source"
    mkdir -p "$src"
    [[ -d "$src/.git" ]] || git init -q "$src"
    # Fetch only when the checkout is not already the pinned commit, under the pinned tag.
    if [[ "$(git -C "$src" rev-parse -q --verify HEAD 2>/dev/null)" != "$commit" ||
        -n "$(git -C "$src" status --porcelain --untracked-files=all)" ||
        (-n "$tag" && "$(git -C "$src" rev-parse -q --verify "refs/tags/$tag^{commit}" 2>/dev/null)" != "$commit") ]]; then
        echo "fetching the engine: $url ${tag:-$commit}" >&2
        if [[ -n "$tag" ]] && git -C "$src" fetch -q --force --no-tags --depth 1 "$url" "refs/tags/$tag:refs/tags/$tag"; then
            local named
            named="$(git -C "$src" rev-parse "refs/tags/$tag^{commit}")"
            [[ "$named" == "$commit" ]] || die "tag $tag of $url names $named, not the pinned $commit"
        elif [[ -z "$tag" || -n "${PD2_ENGINE_UNRELEASED:-}" ]]; then
            [[ -z "$tag" ]] || echo "fetching the engine: $url has no tag $tag, so the pinned commit $commit" >&2
            git -C "$src" fetch -q --force --no-tags --depth 1 "$url" "$commit" ||
                die "cannot fetch commit $commit from $url"
        else
            die "cannot fetch tag $tag from $url"
        fi
        git -C "$src" checkout -q --detach --force "$commit"
        git -C "$src" clean -q -ffdx
    fi
    [[ "$(git -C "$src" rev-parse HEAD)" == "$commit" ]] || die "the engine checkout is not at $commit"
    if [[ -n "$source_sha256" && "$(tree_hash "$src")" != "$source_sha256" ]]; then
        die "the sources of $commit are not the pinned ones: their SHA-256 is not source_sha256 in $lock"
    fi
    echo "$src"
}

if [[ "$mode" == pin ]]; then
    [[ -n "$pin_ref" ]] || die "usage: engine/build.sh --pin <tag|commit> [url]"
    if [[ -n "$pin_url" ]]; then
        [[ "$pin_url" =~ ^https://[^/]+/[^/]+/[^/]+\.git$ ]] ||
            die "$pin_url is not a repository URL (https://github.com/<owner>/<repository>.git)"
        url="$pin_url"
    fi
    [[ -n "$url" ]] || die "$lock names no url"
    repository="$(sed -E 's#^https://github\.com/##; s#\.git$##' <<<"$url")"
    if [[ "$pin_ref" =~ ^[0-9a-f]{40}$ ]]; then
        tag=""
        commit="$pin_ref"
    else
        tag="$pin_ref"
        refs="$(git ls-remote --tags "$url" "$tag" "$tag^{}")" || die "cannot list the tags of $url"
        # An annotated tag's peeled line (^{}) names its commit; a lightweight tag names it directly.
        commit="$(awk -v t="refs/tags/$tag^{}" '$2 == t { print $1 }' <<<"$refs")"
        [[ -n "$commit" ]] || commit="$(awk -v t="refs/tags/$tag" '$2 == t { print $1 }' <<<"$refs")"
        # A draft release has no tag yet: the commit it targets, which the GitHub CLI shows its writers.
        if [[ -z "$commit" && -n "${PD2_ENGINE_UNRELEASED:-}" ]]; then
            commit="$(gh release view "$tag" --repo "$repository" --json targetCommitish --jq .targetCommitish)" ||
                die "$url has no tag $tag, and the GitHub CLI finds no draft release $tag there"
            [[ "$commit" =~ ^[0-9a-f]{40}$ ]] ||
                die "the draft release $tag of $url targets ${commit:-nothing}, not a commit (gh release create --target <commit>)"
        fi
        [[ "$commit" =~ ^[0-9a-f]{40}$ ]] || die "$url has no tag $tag"
    fi
    # The sources as a build fetches them, with no hash to check yet.
    source_sha256=""
    unset PD2_ENGINE_SOURCE
    src="$(source_folder)"
    grep -Eq '^gui *=' "$src/Cargo.toml" ||
        die "the sources of $commit have no gui Cargo feature, so a build cannot leave out the window: pin a release that has it"
    source_sha256="$(tree_hash "$src")"
    cat > "$lock.new" <<EOF
# The PD2 Launcher engine that the APK runs: engine/build.sh builds it, without its window, from
# the sources of the desktop Launcher's public repository (url) at the commit below. The tag names
# that commit's release; a pin without a tag names a commit only. source_sha256 is the SHA-256 of
# the lines that sha256sum prints for every file of the commit, sorted by path; a build checks it
# first. \`engine/build.sh --pin <tag>\` sets tag, commit and source_sha256 from the repository.
url=$url
tag=$tag
commit=$commit
source_sha256=$source_sha256
EOF
    mv "$lock.new" "$lock"
    echo "pinned $url ${tag:-(no tag)} at $commit, sources $source_sha256, in $lock"
    exit 0
fi

if [[ "$mode" == default && -z "${PD2_ENGINE_SOURCE:-}" && -z "$source_sha256" && -n "$asset" ]]; then
    die "$lock pins the release executable $asset (the earlier form), which has the window, whose \
libraries the App userland does not have: pin the sources with engine/build.sh --pin <tag>"
fi

src="$(source_folder)"
if [[ "$mode" == source ]]; then
    echo "$src"
    exit 0
fi

if [[ -n "${PD2_ENGINE_SOURCE:-}" ]]; then
    [[ -z "$(git -C "$src" status --porcelain --untracked-files=normal)" ]] ||
        die "PD2_ENGINE_SOURCE=$src has uncommitted changes: the engine is built from its committed HEAD"
    echo "engine: the local checkout $src at $(git -C "$src" rev-parse --short=12 HEAD)" >&2
else
    echo "engine: $url ${tag:-(no tag)} ($commit)" >&2
fi
grep -Eq '^gui *=' "$src/Cargo.toml" ||
    die "the engine sources at $(git -C "$src" rev-parse --short=12 HEAD) have no gui Cargo feature, so the build \
cannot leave out the window: pin a release that has it"
out="$cache/engine-out"
[[ -n "${PD2_RELEASE_BUILD:-}" ]] && out="$cache/engine-release-out"
engine="$(container_engine)"
if ! have_image "$engine" "$image"; then
    "$engine" build --platform linux/amd64 \
        --build-arg "DEBIAN_IMAGE=$DEBIAN_IMAGE" --build-arg "DEBIAN_SNAPSHOT=$DEBIAN_SNAPSHOT" \
        -t "$image" -f "$here/Containerfile" "$here" >&2
    archive_image "$engine" "$image"
fi
mkdir -p "$cache/engine/cargo" "$cache/engine/rustup" "$cache/engine/target" "$out"
# One build at a time uses the toolchain and Cargo's caches.
exec 9>"$cache/engine/build.lock"
flock 9
work="$(mktemp -d "$out/.build.XXXXXX")"
trap 'rm -rf "$work"' EXIT
# The build reads a copy of the commit's files, which a later fetch into the shared source folder
# cannot change, and the evidence names that copy's SHA-256.
built="$(git -C "$src" rev-parse HEAD)"
mkdir "$work/src"
git -C "$src" archive --format=tar "$built" | tar -x -C "$work/src"
copied="$(tree_hash "$work/src")"
[[ -n "${PD2_ENGINE_SOURCE:-}" || -z "$source_sha256" || "$copied" == "$source_sha256" ]] ||
    die "the copy of $built is not the pinned sources: its SHA-256 is not source_sha256 in $lock"
# The engine's name has its version and its commit: builds of two commits of one version do not
# replace each other in the output folder.
version="$(sed -n 's/^version = "\(.*\)"$/\1/p' "$work/src/Cargo.toml" | head -n 1)"
[[ -n "$version" ]] || die "no package version in the Cargo.toml of $built"
artifact="pd2-launcher-$version-${built:0:12}-aarch64"
# The files the build writes are the invoking user's: Docker runs the build as that user (rootless
# Podman maps the container's root to it already).
user_args=()
[[ "$(basename "$engine")" == docker ]] && user_args=(--user "$(id -u):$(id -g)")
"$engine" run --rm --platform linux/amd64 --security-opt label=disable "${user_args[@]}" \
    -v "$work/src:/src:ro" -v "$here:/engine:ro" -v "$work:/out" \
    -v "$cache/engine/cargo:/cache/cargo" -v "$cache/engine/rustup:/cache/rustup" \
    -v "$cache/engine/target:/cache/target" \
    -e PD2_ENGINE_IN_CONTAINER=1 -e HOME=/tmp -e "ENGINE_NAME=$artifact" -e "GIT_COMMIT=$built" \
    -e "SOURCES_SHA256=$copied" -e "BUILD_IMAGE=$image" -e "ENGINE_NEEDED=${ENGINE_NEEDED[*]}" \
    -e "SOURCE_DATE_EPOCH=$(git -C "$src" show -s --format=%ct "$built")" \
    -e "PD2_LAUNCHER_COMMIT=$(git -C "$src" rev-parse --short=12 "$built")" \
    ${PD2_RELEASE_BUILD:+-e PD2_RELEASE_BUILD=1} \
    "$image" bash /engine/build.sh --compile >&2
mv -f "$work/$artifact.build-evidence.txt" "$out/"
mv -f "$work/$artifact" "$out/"
# ../build.sh takes the newest executable in the folder.
touch "$out/$artifact"
echo "$out/$artifact"
