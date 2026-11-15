#!/usr/bin/env bash
# Rebuild one sealed Win32 helper and print its embedded identity.
#
#   tools/build-sealed-helper.sh tools/vendor-ui-helper.c src/install/vendor_ui_helper.hex
#   tools/build-sealed-helper.sh --verify tools/vendor-ui-helper.c src/install/vendor_ui_helper.hex
#   tools/build-sealed-helper.sh --container [--verify] tools/xinput-bridge.c src/install/xinput_bridge.hex
#
# The helper is a freestanding i686 PE with no C runtime: clang compiles it for
# i686-pc-windows-msvc, llvm-dlltool writes minimal import libraries for only
# the functions it names, and lld-link links it with /Brepro so the output has
# no timestamp. The PE is xz-compressed and hex-encoded for include_str!; the
# Rust side pins the decompressed size and SHA-256, so update those constants
# (and the frozen identity test) deliberately after reviewing a rebuild.
#
# --verify rebuilds the source and compares the PE with the one embedded in
# the committed .hex file (the decompressed bytes, so another xz version does
# not matter). It prints SKIP and succeeds when the toolchain is missing.
#
# --container runs the same build in the pinned toolchain of
# tools/Containerfile.sealed-helper (podman or docker); a helper whose identity
# was recorded from that container reproduces only there.
#
# A helper with a .def file next to its source (tools/xinput-bridge.def) is a
# DLL: linked with /dll, that .def's exports, and DllMain as its entry point.
set -euo pipefail

if [[ ${1:-} == --container ]]; then
    shift
    root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
    engine=$(command -v podman || command -v docker) || {
        echo "--container needs podman or docker" >&2
        exit 1
    }
    image="localhost/pd2-sealed-helper:$(sha256sum "$root/tools/Containerfile.sealed-helper" | cut -c1-16)"
    if ! "$engine" image exists "$image" 2>/dev/null; then
        "$engine" build --platform linux/amd64 -t "$image" \
            -f "$root/tools/Containerfile.sealed-helper" "$root/tools" >&2
    fi
    args=()
    for arg in "$@"; do
        if [[ $arg == --* ]]; then
            args+=("$arg")
        else
            args+=("$(realpath -m --relative-to="$root" "$arg")")
        fi
    done
    user=()
    [[ $engine == *podman ]] && user=(--userns=keep-id) || user=(--user "$(id -u):$(id -g)")
    exec "$engine" run --rm --platform linux/amd64 "${user[@]}" -v "$root:/src" -w /src \
        "$image" tools/build-sealed-helper.sh "${args[@]}"
fi

verify=0
if [[ ${1:-} == --verify ]]; then
    verify=1
    shift
fi
source=${1:?source .c file}
output=${2:?output .hex file}
exports=${source%.c}.def

tools=(clang llvm-dlltool lld-link xz xxd)
for tool in "${tools[@]}"; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        if ((verify)); then
            echo "SKIP: $tool unavailable; sealed helper $source not rebuilt"
            exit 0
        fi
        echo "$tool is required to build $source" >&2
        exit 1
    fi
done

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

cat >"$work/kernel32.def" <<'DEF'
LIBRARY KERNEL32.dll
EXPORTS
CloseHandle@4
CreateFileA@28
CreateFileMappingA@24
DisableThreadLibraryCalls@4
ExitProcess@4
GetEnvironmentVariableA@12
GetStdHandle@4
GetTickCount@0
MapViewOfFile@20
Sleep@4
OpenProcess@12
TerminateProcess@8
WaitForSingleObject@8
WideCharToMultiByte@32
WriteFile@20
DEF
cat >"$work/user32.def" <<'DEF'
LIBRARY USER32.dll
EXPORTS
CreateWindowExW@48
DestroyWindow@4
FindWindowW@8
FindWindowExW@16
GetDC@4
GetDlgItem@8
GetWindowTextW@12
GetWindowThreadProcessId@8
IsWindowEnabled@4
IsWindowVisible@4
PostMessageW@16
ReleaseDC@8
SendMessageW@16
DEF
cat >"$work/gdi32.def" <<'DEF'
LIBRARY GDI32.dll
EXPORTS
ChoosePixelFormat@8
SetPixelFormat@12
DEF
cat >"$work/opengl32.def" <<'DEF'
LIBRARY OPENGL32.dll
EXPORTS
glGetString@4
wglCreateContext@4
wglDeleteContext@4
wglMakeCurrent@8
DEF
# A helper links only the imports it names: unused libraries add nothing.
for library in kernel32 user32 gdi32 opengl32; do
    llvm-dlltool -m i386 -k -d "$work/$library.def" -l "$work/$library.lib"
done
clang --target=i686-pc-windows-msvc -Os -ffreestanding -fno-builtin \
    -fno-stack-protector -c "$source" -o "$work/helper.obj"
link=(/entry:entry)
if [[ -f $exports ]]; then
    link=(/dll /entry:DllMain "/def:$exports")
fi
lld-link /nologo "${link[@]}" /subsystem:windows /nodefaultlib /Brepro \
    /out:"$work/helper.exe" "$work/helper.obj" "$work/kernel32.lib" "$work/user32.lib" \
    "$work/gdi32.lib" "$work/opengl32.lib"

printf 'toolchain: %s; %s; %s; %s\n' \
    "$(clang --version | head -n 1)" \
    "$(lld-link --version | head -n 1)" \
    "$(llvm-dlltool --version 2>/dev/null | grep -m 1 -i version || echo 'llvm-dlltool (version unavailable)')" \
    "$(xz --version | head -n 1)"
rebuilt_sha=$(sha256sum "$work/helper.exe" | cut -d' ' -f1)

if ((verify)); then
    xxd -r -p "$output" | xz --decompress --stdout >"$work/embedded.exe"
    embedded_sha=$(sha256sum "$work/embedded.exe" | cut -d' ' -f1)
    if [[ $rebuilt_sha != "$embedded_sha" ]]; then
        echo "sealed helper $source does not reproduce $output: rebuilt $rebuilt_sha, embedded $embedded_sha" >&2
        exit 1
    fi
    printf 'sealed helper reproduced: %s sha256 %s\n' "$source" "$rebuilt_sha"
    exit 0
fi

xz --format=xz --check=crc64 -9e --stdout "$work/helper.exe" | xxd -p >"$output"
printf 'size %s\nsha256 %s\n' "$(stat -c %s "$work/helper.exe")" "$rebuilt_sha"
