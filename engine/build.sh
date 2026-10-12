#!/usr/bin/env bash
# Puts the PD2 Launcher engine the APK runs, the standalone ARM64 Launcher, into the cache where
# ../build.sh takes it from (paths below are relative to the app's folder, this one's parent):
#   engine/build.sh                      the pinned release's ARM64 executable, downloaded and checked
#                                        against engine.lock, into $cache/engine-out/
#                                        ($cache/engine-release-out/ with PD2_RELEASE_BUILD=1)
#   engine/build.sh --build              builds that release from its sources instead, with
#                                        PD2_RELEASE_BUILD=1 a release build (also the default
#                                        when the pin names no executable)
#   engine/build.sh --source             prints the engine's source folder only
#                                        (runtime/pack.sh reads its pins there)
#   engine/build.sh --pin <tag> [asset]  sets tag, commit and the executable's SHA-256 in the pin
#                                        file from the pinned repository and its release
# The engine's sources are not in this repository. They come from the desktop Launcher's public
# repository at the tag and commit that engine.lock pins, fetched into
# $cache/engine/source and checked against the pin. Each release of that repository carries its
# ARM64 executable, a release build of exactly that commit made by that repository's
# scripts/build-appimage-container.sh <out> aarch64 binary (Podman or Docker; see its README):
# engine.lock names that file (asset) and its SHA-256; the download must match both, and its
# build evidence the pinned commit. A release of a private repository is downloaded with
# the GitHub CLI (its login, or GH_TOKEN), its sources with Git's credentials.
# PD2_ENGINE_SOURCE=<folder> builds a local checkout of the Launcher instead, for working on the
# engine: its committed HEAD, since the container build refuses uncommitted changes.
# PD2_ENGINE_LOCK names another pin file. $cache is PD2_ANDROID_CACHE or ~/.cache/pd2-android.
# PD2_ENGINE_UNRELEASED=1 accepts a pin of a release that is still a draft, without its tag:
# the sources are fetched by the pinned commit, and --pin takes the commit the draft targets.
set -euo pipefail
# Never wait for a password prompt.
export GIT_TERMINAL_PROMPT=0

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cache="${PD2_ANDROID_CACHE:-$HOME/.cache/pd2-android}"
lock="${PD2_ENGINE_LOCK:-$here/../engine.lock}"
die() { printf 'engine/build.sh: %s\n' "$*" >&2; exit 1; }

mode=default
case "${1:-}" in
    "") ;;
    --build) mode=build ;;
    --source) mode=source ;;
    --pin) mode=pin; pin_tag="${2:-}"; pin_asset="${3:-}" ;;
    *) echo "usage: engine/build.sh [--build | --source | --pin <tag> [asset]]" >&2; exit 2 ;;
esac

[[ -f "$lock" ]] || die "no pin file $lock"
pinned() { sed -n "s/^$1=//p" "$lock" | head -n 1; }
url="$(pinned url)"
tag="$(pinned tag)"
commit="$(pinned commit)"
asset="$(pinned asset)"
sha256="$(pinned sha256)"
repository="$(sed -E 's#^https://github\.com/##; s#\.git$##' <<<"$url")"

# Downloads one file of a release of the pinned repository into a folder: anonymously (a public
# repository), else through the GitHub CLI (a private one).
download() { # release-tag file folder
    curl -fsSL --retry 3 -o "$3/$2" "${url%.git}/releases/download/$1/$2" 2>/dev/null && return
    rm -f "$3/$2"
    command -v gh >/dev/null &&
        gh release download "$1" --repo "$repository" --pattern "$2" --dir "$3" --clobber >/dev/null 2>&1 &&
        [[ -f "$3/$2" ]]
}

if [[ "$mode" == pin ]]; then
    [[ -n "$pin_tag" ]] || die "usage: engine/build.sh --pin <tag> [asset]"
    [[ -n "$url" ]] || die "$lock names no url"
    refs="$(git ls-remote --tags "$url" "$pin_tag" "$pin_tag^{}")" || die "cannot list the tags of $url"
    # An annotated tag's peeled line (^{}) names its commit; a lightweight tag names it directly.
    found="$(awk -v t="refs/tags/$pin_tag^{}" '$2 == t { print $1 }' <<<"$refs")"
    [[ -n "$found" ]] || found="$(awk -v t="refs/tags/$pin_tag" '$2 == t { print $1 }' <<<"$refs")"
    # A draft release has no tag yet: the commit it targets, which the GitHub CLI shows its writers.
    if [[ -z "$found" && -n "${PD2_ENGINE_UNRELEASED:-}" ]]; then
        found="$(gh release view "$pin_tag" --repo "$repository" --json targetCommitish --jq .targetCommitish)" ||
            die "$url has no tag $pin_tag, and the GitHub CLI finds no draft release $pin_tag there"
        [[ "$found" =~ ^[0-9a-f]{40}$ ]] ||
            die "the draft release $pin_tag of $url targets ${found:-nothing}, not a commit (gh release create --target <commit>)"
    fi
    [[ "$found" =~ ^[0-9a-f]{40}$ ]] || die "$url has no tag $pin_tag"
    # The release's ARM64 executable: its SHA-256 is computed from the download and must match
    # the release's own .sha256 file.
    pin_asset="${pin_asset:-${asset:-pd2-launcher-0.1.0-aarch64}}"
    work="$(mktemp -d)"
    trap 'rm -rf "$work"' EXIT
    download "$pin_tag" "$pin_asset" "$work" || die "the release $pin_tag of $url has no $pin_asset"
    download "$pin_tag" "$pin_asset.sha256" "$work" || die "the release $pin_tag of $url has no $pin_asset.sha256"
    pin_sha256="$(sha256sum < "$work/$pin_asset" | cut -d' ' -f1)"
    [[ "$(cut -d' ' -f1 "$work/$pin_asset.sha256")" == "$pin_sha256" ]] ||
        die "$pin_asset of $pin_tag does not match its .sha256 file"
    sed -i -e "s|^tag=.*|tag=$pin_tag|" -e "s|^commit=.*|commit=$found|" -e '/^asset=/d' -e '/^sha256=/d' "$lock"
    printf 'asset=%s\nsha256=%s\n' "$pin_asset" "$pin_sha256" >> "$lock"
    echo "pinned $url $pin_tag at $found, $pin_asset $pin_sha256, in $lock"
    exit 0
fi

out="$cache/engine-out"
[[ -n "${PD2_RELEASE_BUILD:-}" ]] && out="$cache/engine-release-out"

# The pinned release's executable, unless a local checkout or a build from source is asked for.
if [[ "$mode" == default && -z "${PD2_ENGINE_SOURCE:-}" && -n "$asset" ]]; then
    [[ "$commit" =~ ^[0-9a-f]{40}$ && "$sha256" =~ ^[0-9a-f]{64}$ ]] ||
        die "$lock names the executable $asset without a commit and SHA-256: pin it with engine/build.sh --pin <tag>"
    evidence() { sed -n "s/^$1=//p" "$2" | head -n 1; }
    checked() { # binary: whether it is the pinned executable, with build evidence naming the pin
        [[ -f "$1" && -f "$1.build-evidence.txt" && "$(sha256sum < "$1" | cut -d' ' -f1)" == "$sha256" &&
            "$(evidence sha256 "$1.build-evidence.txt")" == "$sha256" &&
            "$(evidence git_commit "$1.build-evidence.txt")" == "$commit" &&
            "$(evidence architecture "$1.build-evidence.txt")" == aarch64 &&
            "$(evidence release_build "$1.build-evidence.txt")" == 1 ]]
    }
    mkdir -p "$out"
    if checked "$out/$asset"; then
        echo "engine: $url $tag, $asset already downloaded" >&2
    else
        echo "engine: downloading $asset of $url $tag" >&2
        work="$(mktemp -d "$out/.download.XXXXXX")"
        trap 'rm -rf "$work"' EXIT
        download "$tag" "$asset" "$work" || die "cannot download $asset of the release $tag of $url"
        download "$tag" "$asset.build-evidence.txt" "$work" ||
            die "cannot download $asset.build-evidence.txt of the release $tag of $url"
        checked "$work/$asset" || die "$asset of $tag is not the pinned executable: its SHA-256 or its \
build evidence (commit $commit, aarch64, a release build) differs from $lock"
        chmod 0755 "$work/$asset"
        mv -f "$work/$asset.build-evidence.txt" "$out/"
        mv -f "$work/$asset" "$out/"
    fi
    # ../build.sh takes the newest executable in the folder.
    touch "$out/$asset"
    echo "$out/$asset"
    exit 0
fi

# The folder holding the engine's sources, checked out at the pin unless PD2_ENGINE_SOURCE names one.
source_folder() {
    if [[ -n "${PD2_ENGINE_SOURCE:-}" ]]; then
        local dir
        dir="$(cd "$PD2_ENGINE_SOURCE" && pwd -P)" || die "PD2_ENGINE_SOURCE: no folder $PD2_ENGINE_SOURCE"
        [[ -f "$dir/Cargo.toml" && -x "$dir/scripts/build-appimage-container.sh" ]] ||
            die "PD2_ENGINE_SOURCE=$dir is not a checkout of the PD2 Launcher"
        echo "$dir"
        return
    fi
    if [[ -z "$url" || -z "$tag" || ! "$commit" =~ ^[0-9a-f]{40}$ ]]; then
        die "$lock pins no engine release yet (url, tag and commit): pin one with
  engine/build.sh --pin <tag>
or build from a local checkout of the PD2 Launcher with PD2_ENGINE_SOURCE=<folder>"
    fi
    local src="$cache/engine/source"
    mkdir -p "$src"
    [[ -d "$src/.git" ]] || git init -q "$src"
    # Fetch only when the checkout is not already the pinned commit under the pinned tag.
    if [[ "$(git -C "$src" rev-parse -q --verify "refs/tags/$tag^{commit}" 2>/dev/null)" != "$commit" ||
        "$(git -C "$src" rev-parse -q --verify HEAD 2>/dev/null)" != "$commit" ||
        -n "$(git -C "$src" status --porcelain --untracked-files=all)" ]]; then
        echo "fetching the engine: $url $tag" >&2
        if git -C "$src" fetch -q --force --no-tags "$url" "refs/tags/$tag:refs/tags/$tag"; then
            local named
            named="$(git -C "$src" rev-parse "refs/tags/$tag^{commit}")"
            [[ "$named" == "$commit" ]] || die "tag $tag of $url names $named, not the pinned $commit"
        elif [[ -n "${PD2_ENGINE_UNRELEASED:-}" ]]; then
            # A draft release has no tag yet: its commit, by its SHA.
            echo "fetching the engine: $url has no tag $tag, so the pinned commit $commit" >&2
            git -C "$src" fetch -q --force --no-tags "$url" "$commit" ||
                die "cannot fetch commit $commit from $url"
        else
            die "cannot fetch tag $tag from $url"
        fi
        git -C "$src" checkout -q --detach --force "$commit"
        git -C "$src" clean -q -ffdx
    fi
    [[ "$(git -C "$src" rev-parse HEAD)" == "$commit" ]] || die "the engine checkout is not at $commit"
    echo "$src"
}

src="$(source_folder)"
if [[ "$mode" == source ]]; then
    echo "$src"
    exit 0
fi

if [[ -n "${PD2_ENGINE_SOURCE:-}" ]]; then
    echo "engine: the local checkout $src at $(git -C "$src" rev-parse --short=12 HEAD)" >&2
else
    echo "engine: $url $tag ($commit)" >&2
fi
"$src/scripts/build-appimage-container.sh" "$out" aarch64 binary
