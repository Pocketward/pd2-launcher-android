#!/usr/bin/env bash
# The slow parts of the build as data-only images on ghcr.io, like the base image (base-image.sh):
# ghcr.io/pocketward/pd2-part-<part>:<tag>, FROM scratch, with one file /part.tar.zst (the part's
# output folders, file modes and links as they are) and the OCI source label. The tag is the first
# 32 hex digits of the SHA-256 of the part's identity: the text that the part's script prints
# before any build (<script> --identity, userland/pack.sh --version), so the same inputs name the
# same image, and a changed input names a new one. The release workflow (build.yml) pulls a part
# whose image exists, and builds and pushes one that does not. A local build never needs this:
# the scripts keep their output folders and reuse them as before.
#
#   part-image.sh name <part>                the image's name (no network)
#   part-image.sh check <part>               the local output is the part's identity (its BUILDINFO)
#   part-image.sh query <part>               exit 0: the image exists; 1: it does not; 2: another error
#   part-image.sh fetch [--need] <part>...   pulls and unpacks each part whose image exists, and checks
#                                            it; prints the missing ones (and writes missing=<parts>
#                                            to $GITHUB_OUTPUT when set); --need: a missing one fails
#   part-image.sh publish [--need] <part>... pushes each part that is built and checked and has no
#                                            image yet; --need: a part that is not built fails
#   part-image.sh image <part>               only the local image, from the local output (no push)
#   part-image.sh unpack <part>              only the unpack, from the local image (no pull)
#   part-image.sh login                      logs the container engine in to ghcr.io with
#                                            $GITHUB_TOKEN as $GITHUB_ACTOR (the workflows)
#
# Parts: xvfb mesa panvk panvk-tools sysdrv hybris radv mali-panvk glibc (the userland's parts),
# userland (the packed assets), wine wowbox64 pd2audio prefix runtime, engine (the release build,
# before its attest), devdriver. The folders are those of the scripts (the same variables:
# PD2_ANDROID_CACHE, PD2_USERLAND_CACHE, PD2_PANVK_CACHE, PD2_MALI_PANVK_CACHE, PD2_RUNTIME_CACHE).
set -euo pipefail
shopt -s inherit_errexit

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
app_root="$(dirname "$here")"
# shellcheck source=../runtime/config.sh
. "$app_root/runtime/config.sh"
cache="${PD2_ANDROID_CACHE:-$HOME/.cache/pd2-android}"
REPO=ghcr.io/pocketward
SOURCE=https://github.com/Pocketward/pd2-launcher-android
PARTS=(xvfb mesa panvk panvk-tools sysdrv hybris radv mali-panvk glibc userland wine wowbox64 pd2audio prefix runtime
    engine devdriver)
say() { echo "part-image: $*" >&2; }
die() { say "$*"; exit 1; }
known() { [[ " ${PARTS[*]} " == *" $1 "* ]] || die "no part $1 (parts: ${PARTS[*]})"; }

# The part's identity, as its script prints it.
identity() { # part
    case "$1" in
        xvfb | mesa | panvk | sysdrv | hybris | radv | mali-panvk | glibc) "$USERLAND_SRC/build-$1.sh" --identity ;;
        panvk-tools) echo "image=$(base_image_name)"; echo "mesa=$PANVK_MESA_COMMIT" ;;
        userland) PD2_USERLAND_WITH_GLIBC=1 "$USERLAND_SRC/pack.sh" --version ;;
        wine | wowbox64 | pd2audio | prefix) "$RUNTIME_DIR/$1.sh" --identity ;;
        runtime) "$RUNTIME_DIR/build.sh" --identity ;;
        engine) PD2_RELEASE_BUILD=1 "$app_root/engine/build.sh" --identity ;;
        devdriver) "$app_root/devdriver/build.sh" --identity ;;
    esac
}

name() { # part
    local id
    id="$(identity "$1")"
    [[ -n "$id" ]] || die "$1: empty identity"
    echo "$REPO/pd2-part-$1:$(sha256sum <<<"$id" | cut -c1-32)"
}

# Where the part's output is: a root folder and the paths below it that the image holds.
where() { # part -> root path...
    case "$1" in
        xvfb | mesa | sysdrv | hybris | radv) echo "$USERLAND_CACHE $1-out" ;;
        panvk) echo "$PANVK_CACHE out" ;;
        panvk-tools) echo "$PANVK_CACHE/work host-tools-$PANVK_MESA_COMMIT" ;;
        mali-panvk) echo "$MALI_PANVK_CACHE mali-panvk-out" ;;
        glibc) echo "$USERLAND_SRC/glibc out" ;;
        userland) echo "$app_root app/src/main/assets/userland app/system-driver/assets" ;;
        wine | wowbox64 | pd2audio | prefix | runtime) echo "$RUNTIME_PARTS $1" ;;
        engine) echo "$cache engine-release-out" ;;
        devdriver) echo "$cache devdriver-out" ;;
    esac
}

# True when the local output is the part's identity: the rule by which the scripts reuse a part.
check() { # part
    local root paths dir evidence
    read -r root paths <<<"$(where "$1")"
    dir="$root/${paths%% *}"
    case "$1" in
        glibc)
            [[ -f "$dir/BUILDINFO" ]] && cmp -s <(identity glibc) \
                <(grep -E '^(glibc_debian_version|image|prefix|input|patch)=' "$dir/BUILDINFO") ;;
        panvk-tools)
            [[ "$(cat "$dir/IMAGE" 2>/dev/null)" == "$(base_image_name)" && -x "$dir/bin/mesa_clc" &&
                -x "$dir/bin/vtn_bindgen2" && -x "$dir/bin/panfrost_compile" ]] ;;
        userland)
            [[ -f "$root/app/system-driver/assets/userland-system-driver/manifest.json" ]] &&
                [[ "$(sed -n 's/^ "version": "\(.*\)",\{0,1\}$/\1/p' "$dir/manifest.json" 2>/dev/null | head -n 1)" == "$(identity userland)" ]] ;;
        engine)
            # The engine's evidence: this commit's engine files (engine_tree), a release build, the
            # base image, its own SHA-256 (no BUILDINFO).
            evidence="$(ls "$dir"/pd2-launcher-*-aarch64.build-evidence.txt 2>/dev/null)" || return 1
            [[ $(wc -l <<<"$evidence") == 1 ]] || return 1
            grep -qx "engine_tree=$("$app_root/engine/build.sh" --tree)" "$evidence" &&
                grep -qx 'release_build=1' "$evidence" &&
                grep -qx "build_image=$(base_image_name)" "$evidence" &&
                grep -qx "sha256=$(sha256sum < "${evidence%.build-evidence.txt}" | cut -d' ' -f1)" "$evidence" ;;
        devdriver)
            [[ -f "$dir/BUILDINFO" ]] && cmp -s <(identity devdriver) "$dir/BUILDINFO" &&
                (cd "$dir" && sha256sum -c --quiet SHA256SUMS >/dev/null 2>&1) ;;
        *)
            [[ -f "$dir/BUILDINFO" ]] && cmp -s <(identity "$1") "$dir/BUILDINFO" ;;
    esac
}

# 0: the image exists; 1: it does not; 2: any other answer (no access, no network).
query() { # part
    local image answer
    image="$(name "$1")"
    if command -v skopeo >/dev/null; then
        answer="$(skopeo inspect --raw "docker://$image" 2>&1)" && return 0
    else
        answer="$("$(container_engine)" manifest inspect "$image" 2>&1)" && return 0
    fi
    # Podman's manifest inspect reads a manifest list only: a single image's manifest (which it then
    # prints in its error) exists.
    grep -q 'schemaVersion' <<<"$answer" && return 0
    # Only the registry's own "not there" answers; anything else (no access, no network) stops.
    grep -qiE 'manifest unknown|manifest_unknown|name unknown|name_unknown|no such manifest' <<<"$answer" && return 1
    if grep -qiE 'denied|unauthorized|403' <<<"$answer"; then
        say "cannot read $image: $answer (the package must give this repository access: package settings, Manage Actions access)"
    else
        say "cannot read $image: $answer"
    fi
    return 2
}

image() { # part: the local data image from the local output
    local engine image root paths work
    check "$1" || die "$1: the local output is not the part's identity: not packed"
    engine="$(container_engine)"
    image="$(name "$1")"
    read -r root paths <<<"$(where "$1")"
    work="$(mktemp -d "${TMPDIR:-/tmp}/part-image.XXXXXX")"
    # shellcheck disable=SC2086 # the paths, separated by spaces
    tar --zstd -cf "$work/part.tar.zst" -C "$root" $paths
    printf 'FROM scratch\nCOPY part.tar.zst /part.tar.zst\nLABEL org.opencontainers.image.source=%s\n' "$SOURCE" > "$work/Containerfile"
    "$engine" build --platform linux/amd64 -q -t "$image" -f "$work/Containerfile" "$work" >/dev/null
    rm -rf "$work"
    say "$1: $image ($(cd "$root" && du -sh ${paths%% *} | cut -f1))"
}

unpack() { # part: the local output from the local image
    local engine image root paths path container work runtime
    engine="$(container_engine)"
    image="$(name "$1")"
    read -r root paths <<<"$(where "$1")"
    work="$(mktemp -d "${TMPDIR:-/tmp}/part-image.XXXXXX")"
    container="$("$engine" create --platform linux/amd64 "$image" /none)"
    "$engine" cp "$container:/part.tar.zst" "$work/part.tar.zst" >/dev/null
    "$engine" rm "$container" >/dev/null
    mkdir -p "$root"
    for path in $paths; do rm -rf "${root:?}/$path"; done
    tar --zstd -xpf "$work/part.tar.zst" -C "$root"
    rm -rf "$work"
    # The runtime's assets are a copy of parts/runtime (runtime/build.sh copies them too).
    if [[ "$1" == runtime ]]; then
        runtime="$RUNTIME_PARTS/runtime"
        mkdir -p "$RUNTIME_ASSETS"
        cp "$runtime/runtime.tar.zst" "$runtime/runtime.info" "$RUNTIME_ASSETS/"
    fi
    check "$1" || die "$1: $image does not hold the part's identity"
    say "$1: unpacked from $image"
}

fetch() { # [--need] part...
    local need=0 part missing=() status engine
    [[ "${1:-}" == --need ]] && { need=1; shift; }
    engine="$(container_engine)"
    for part in "$@"; do
        known "$part"
        status=0
        query "$part" || status=$?
        case "$status" in
            0)
                "$engine" pull -q --platform linux/amd64 "$(name "$part")" >/dev/null
                unpack "$part"
                "$engine" rmi "$(name "$part")" >/dev/null || true
                ;;
            1) say "$part: no image $(name "$part"): it must be built"; missing+=("$part") ;;
            *) exit 2 ;;
        esac
    done
    [[ -n "${GITHUB_OUTPUT:-}" ]] && echo "missing=${missing[*]}" >> "$GITHUB_OUTPUT"
    echo "missing: ${missing[*]:-none}"
    [[ "$need" == 0 || ${#missing[@]} == 0 ]] || die "needed but not on the registry: ${missing[*]}"
}

publish() { # [--need] part...
    local need=0 part status
    [[ "${1:-}" == --need ]] && { need=1; shift; }
    for part in "$@"; do
        known "$part"
        status=0
        query "$part" || status=$?
        case "$status" in
            0) say "$part: $(name "$part") exists already" ;;
            1)
                if check "$part"; then
                    image "$part"
                    "$(container_engine)" push -q "$(name "$part")"
                    echo "::notice title=Part pushed::$(name "$part")"
                elif [[ "$need" == 1 ]]; then
                    die "$part: not built, or its output is not its identity: not pushed"
                else
                    say "$part: not built, or its output is not its identity: not pushed"
                fi
                ;;
            *) exit 2 ;;
        esac
    done
}

case "${1:-}" in
    name | check | query | image | unpack)
        [[ $# == 2 ]] || die "usage: part-image.sh $1 <part>"
        known "$2"
        "$1" "$2"
        ;;
    fetch | publish) "$@" ;;
    login)
        : "${GITHUB_TOKEN:?}" "${GITHUB_ACTOR:?}"
        "$(container_engine)" login ghcr.io -u "$GITHUB_ACTOR" --password-stdin <<<"$GITHUB_TOKEN"
        ;;
    *) die "usage: part-image.sh name|check|query|fetch|publish|image|unpack|login ... (see the head of this file)" ;;
esac
