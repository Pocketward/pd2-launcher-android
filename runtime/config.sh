# Pins, paths and helpers of the runtime build (build.sh and the scripts it runs). Sourced, never
# executed. The Debian pins, the data-folder placeholder, the userland's prefix and the container
# helpers are the App userland's (../userland/config.sh).
# shellcheck shell=bash
# shellcheck disable=SC2034 # the pins are read by the scripts that source this file

# An error in a command substitution stops the script too (set -e alone does not reach there).
shopt -s inherit_errexit

RUNTIME_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../userland/config.sh
. "$RUNTIME_DIR/../userland/config.sh"

# The build cache, outside the repository: dl/ the pinned downloads, work/ sources and build
# trees, parts/ the finished parts (each with the BUILDINFO of its inputs; a part whose inputs did
# not change is used again).
RUNTIME_CACHE="${PD2_RUNTIME_CACHE:-$HOME/.cache/pd2-android/runtime}"
RUNTIME_DL="$RUNTIME_CACHE/dl"
RUNTIME_WORK="$RUNTIME_CACHE/work"
RUNTIME_PARTS="$RUNTIME_CACHE/parts"
# The relocated App userland tree (userland/build.sh) the prefix template is made with.
USERLAND_TREE="$USERLAND_CACHE/work/staging"
# Where the APK takes the runtime from (gitignored).
RUNTIME_ASSETS="$RUNTIME_DIR/../app/src/main/assets/runtime"

# Wine stable release; SHA-256 of dl.winehq.org's tarball (signed with wine-11.0.tar.xz.sign).
WINE_VERSION=11.0
WINE_SHA256=c07a6857933c1fc60dff5448d79f39c92481c1e9db5aa628db9d0358446e0701
WINE_URL="https://dl.winehq.org/wine/source/11.0/wine-$WINE_VERSION.tar.xz"
# The install prefix Wine is built for. Wine finds its own files relative to its loader, so the
# tree can move; this name only shows in logs.
WINE_PREFIX_DIR=/opt/wine

# llvm-mingw (github.com/mstorsjo/llvm-mingw) for Wine's PE side and wowbox64.dll. SHA-256
# recorded on first download.
LLVM_MINGW_VERSION=20261006
LLVM_MINGW_NAME=llvm-mingw-$LLVM_MINGW_VERSION-ucrt-ubuntu-22.04-x86_64
LLVM_MINGW_SHA256=5f9c6ed95b2d4bdb2869a488c5fd5857fbdabcf288a0aa3eb1da43f6a08d8ab4
LLVM_MINGW_URL="https://github.com/mstorsjo/llvm-mingw/releases/download/$LLVM_MINGW_VERSION/$LLVM_MINGW_NAME.tar.xz"

# Box64 for wowbox64.dll, the x86 CPU DLL of the ARM64 Wine: a commit after 0.4.4 that gives the
# x86 program the real x86 state when a fault happens in generated code (.NET turns its null
# checks into NullReferenceException only then). SHA-256 of GitHub's archive of the commit,
# recorded on first download.
BOX64_COMMIT=a489cfe7d03f6d57aa38580c157711fbfc69ed7a
BOX64_SHA256=234dedad2dd4ff396bedf93a67510e5ce772298ae108b09f47e805e94dffd0a7
BOX64_URL="https://github.com/ptitSeb/box64/archive/$BOX64_COMMIT.tar.gz"
# The build date wowbox64.dll shows (SOURCE_DATE_EPOCH): 2026-10-12, the day of the commit.
BOX64_DATE_EPOCH=1791763200

# .NET 6 x86 (the runtime and the Windows Desktop runtime) for the official PD2 launcher, a .NET 6
# WPF program: Microsoft's zip archives with the SHA-512 of its release metadata
# (dotnet/release-metadata/6.0/releases.json, 6.0.36).
DOTNET_VERSION=6.0.36
DOTNET_RUNTIME_URL="https://builds.dotnet.microsoft.com/dotnet/Runtime/$DOTNET_VERSION/dotnet-runtime-$DOTNET_VERSION-win-x86.zip"
DOTNET_RUNTIME_SHA512=1d7ecca39d4d14c27b067358c433ebf90dde716e5c4bdc244861dc7268b23f172d61bf910a4e16b46e4d629f23774b8311ba8d5a45085d995da4ef25b74c75ea
DOTNET_DESKTOP_URL="https://builds.dotnet.microsoft.com/dotnet/WindowsDesktop/$DOTNET_VERSION/windowsdesktop-runtime-$DOTNET_VERSION-win-x86.zip"
DOTNET_DESKTOP_SHA512=c137de6e19760936986fee027696c824e904b4094d5fc882a22593819c8d08715597d89004dd97042b8e89e4e49eb1052c5d79cb0002f0f5fd057f198686d0c9

# The XInput bridge the prefix template carries as the game's xinput1_4.dll: the engine's sealed
# one (src/install/xinput_bridge.hex in the engine's sources), which must be exactly this file.
XINPUT_BRIDGE_SIZE=4096
XINPUT_BRIDGE_SHA256=b78963150d55de280266418095e79ebeb4d754571f49076c77f8924b34ee70c7

# The managed-root names the engine gives the app's runtime (src/runtime.rs).
APP_RUNTIME_DIRECTORY=wine-11.0-arm64
APP_PREFIX_TEMPLATE=wine-11.0-arm64-prefix
APP_DOTNET_DIRECTORY=dotnet-6.0.36-x86

# The generation of the prefix template, which prefix.sh writes into it (.pd2-prefix-generation): a
# whole number from 1 to 999999999. Raise it by hand when existing prefixes must get a change of the
# template: another Wine version, or other registry values. At a Wine change, raise it, and put the
# old Wine folder and the old template into AppRuntime.LEGACY. The app moves each prefix of another
# generation to a copy of the new template, with the game, its records and its registry keys
# (LegacyPrefix); a prefix without the mark is generation 1. No Wine prefix update runs on the
# phone: most files of the prefix are links into the runtime's Wine, and wineboot -u would write
# through them.
PREFIX_GENERATION=1

# --- helpers --------------------------------------------------------------------------------

# Runs a container with the engine. SELinux labels are off for the bind mounts (an enforcing host
# would refuse the build its own folders otherwise).
engine_run() { # engine-run-arguments...
    "$(container_engine)" run --rm --security-opt label=disable "$@"
}

# Builds (once) and prints the image of containers/Containerfile.<name>. Its tag hashes the
# Containerfile and the Debian pins it is built with.
runtime_image() { # name
    local engine image file="$RUNTIME_DIR/containers/Containerfile.$1"
    engine="$(container_engine)"
    image="localhost/pd2-runtime-$1:$( (cat "$file"; echo "$DEBIAN_IMAGE $DEBIAN_SNAPSHOT") | sha256sum | cut -c1-16)"
    if ! have_image "$engine" "$image"; then
        "$engine" build --platform linux/amd64 \
            --build-arg "DEBIAN_IMAGE=$DEBIAN_IMAGE" --build-arg "DEBIAN_SNAPSHOT=$DEBIAN_SNAPSHOT" \
            -t "$image" -f "$file" "$RUNTIME_DIR/containers" >&2 || return 1
        archive_image "$engine" "$image"
    fi
    echo "$image"
}

# Runs a script of this folder again inside an image, with the folder read-only at /src and the
# cache at /cache (PD2_RUNTIME_CACHE there). Extra engine arguments go in RUN_EXTRA (array).
run_in() { # image script [args...]
    local image="$1" script="$2"; shift 2
    [[ -n "$image" ]] || { echo "no image for $script" >&2; return 1; }
    mkdir -p "$RUNTIME_CACHE" "$RUNTIME_WORK"
    # The userland's folder too, at /userland: config.sh sources its config.sh (../userland).
    engine_run --platform linux/amd64 -v "$RUNTIME_DIR:/src:ro" -v "$USERLAND_SRC:/userland:ro" \
        -v "$RUNTIME_CACHE:/cache" \
        -e PD2_RUNTIME_IN_CONTAINER=1 -e PD2_RUNTIME_CACHE=/cache -e "PD2_IMAGE=$image" \
        "${RUN_EXTRA[@]}" "$image" bash "/src/$script" "$@"
}
RUN_EXTRA=()

# True inside a container that run_in started.
in_container() { [[ "${PD2_RUNTIME_IN_CONTAINER:-}" == 1 ]]; }

# Downloads a pinned file into dl/ and checks it (SHA-256, or SHA-512 for a 128-digit sum); a file
# there that does not match (an older pin of the same name) is downloaded again. A bad download is
# removed, never kept under the file's name. Prints the local path.
fetch() { # url sum [file name]
    local url="$1" sum="$2" file tool=sha256sum
    file="$RUNTIME_DL/${3:-$(basename "$url")}"
    [[ ${#sum} == 128 ]] && tool=sha512sum
    mkdir -p "$RUNTIME_DL"
    if [[ -f "$file" ]] && echo "$sum  $file" | "$tool" -c --quiet - >/dev/null 2>&1; then
        echo "$file"
        return 0
    fi
    rm -f "$file"
    curl -fsSL --retry 3 -o "$file.part" "$url"
    echo "$sum  $file.part" | "$tool" -c --quiet - >&2 || { rm -f "$file.part"; echo "checksum mismatch: $url" >&2; return 1; }
    mv "$file.part" "$file"
    echo "$file"
}

# Inside a container: unpacks the pinned llvm-mingw (fetched into dl/) into work/ once, recorded
# with its SHA-256, and prints its folder.
llvm_mingw() {
    local dir="/cache/work/$LLVM_MINGW_NAME"
    if [[ "$(cat "$dir/.unpacked" 2>/dev/null)" != "$LLVM_MINGW_SHA256" ]]; then
        rm -rf "$dir"
        tar -xJf "/cache/dl/$LLVM_MINGW_NAME.tar.xz" -C /cache/work
        echo "$LLVM_MINGW_SHA256" > "$dir/.unpacked"
    fi
    echo "$dir"
}

# The identity of a part: one line per input, a file of this folder as its path below the folder
# and its SHA-256 (a missing one is an error), anything else (a pin, another part's identity) as
# given.
part_identity() { # input...
    local a
    for a in "$@"; do
        if [[ "$a" == "$RUNTIME_DIR"/* ]]; then
            [[ -f "$a" ]] || { echo "missing input: $a" >&2; return 1; }
            echo "${a#"$RUNTIME_DIR/"} $(sha256sum < "$a" | cut -d' ' -f1)"
        else
            echo "$a"
        fi
    done
}

# True when parts/<name> was built from inputs with this identity.
part_ready() { # name identity
    [[ -f "$RUNTIME_PARTS/$1/BUILDINFO" && "$(cat "$RUNTIME_PARTS/$1/BUILDINFO")" == "$2" ]]
}

# The SHA-256 of a finished part's BUILDINFO: what a later part records of it.
part_hash() { # name
    sha256sum < "$RUNTIME_PARTS/$1/BUILDINFO" | cut -d' ' -f1
}

# Records a part as finished: written last, so a part that stopped half way is built again.
part_done() { # name identity
    printf '%s\n' "$2" > "$RUNTIME_PARTS/$1/BUILDINFO"
}
