#!/usr/bin/env bash
# The two Wine prefix templates of the runtime. PLAIN is made by the ARM64 Wine itself: `wineboot -i`
# and `reg import` run in an arm64 container (qemu-user through binfmt_misc) with the App userland's
# tree (userland/build.sh) mounted at its placeholder prefix and the full Wine (with wowbox64.dll
# as the x86 CPU DLL, so the 32-bit side is set up too). GOLDEN is PLAIN with the golden layer
# (golden.py): what the official installers create, except Blizzard's own files, with no installer
# run. The engine copies the template of the install source to a new prefix (PLAIN for Blizzard's
# installers, GOLDEN for Fast download and imports; no wineboot runs on the phone). Then:
#   - Game.exe gets Windows 7 (Wine 11's default makes Fog.dll's critical-section check fail);
#     OpenGL goes through GLX ("UseEGL"="N": Wine 11 takes EGL by default, which drew about a
#     quarter fewer frames on a Mali at 120 Hz), in every copy of the template, the prefix of an
#     update in place too; WPF draws in software (its hardware path through wined3d costs more
#     CPU). PLAIN has no value of the game: Blizzard's installers write them. GOLDEN has the values
#     of golden.reg (the official installers' values with fixed C:\ paths, no UninstallPath and no
#     Diablo II uninstall record), and the golden layer's files (golden/files.txt). The engine's
#     audit reads no registry value
#   - the prefix is never updated at a start (.update-timestamp "disable")
#   - each template carries its generation (.pd2-prefix-generation, config.sh's PREFIX_GENERATION)
#     and its kind (.pd2-prefix-template: "plain" or "golden"): the app makes a prefix of another
#     generation again from the template of its kind (LegacyPrefix)
#   - every file that is a copy of a Wine file becomes a relative link into the runtime's Wine as
#     the managed root lays it out (prefix-dedupe.py: prefix/ next to runner/wine-11.0-arm64/);
#     links to modules the reduced Wine leaves out dangle, as if the file were missing
#   - the game's xinput1_4.dll is the engine's sealed XInput bridge (src/install/xinput_bridge.hex
#     in the engine's sources, checked against config.sh's size and SHA-256)
#   - C:\Program Files (x86)\dotnet links to the app's .NET runtime (runner/dotnet-6.0.36-x86)
#   - a copy of GOLDEN must load in Wine: a read-only `reg query` of three golden keys runs on it
#     in the arm64 container
# Output: parts/prefix/prefix (PLAIN) and parts/prefix/golden (GOLDEN).
#
# Usage: prefix.sh [--identity]
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=config.sh
. "$here/config.sh"

work=$RUNTIME_WORK/prefix
identity() {
    part_identity "$here/prefix.sh" "$here/prefix-dedupe.py" "$here/stage-wineb.py" "$here/golden.py" \
        "$here/golden.reg" "$here/golden/files.txt" "$here/golden/cached-exe.txt" \
        "$here/golden/state-x64.rsm.hex" "$here/golden/state-x86.rsm.hex" \
        "$here/golden/project-diablo-2.lnk.hex" "$here/containers/Containerfile.tools" \
        "vcredist=$VCREDIST_X64_SHA256 $VCREDIST_X86_SHA256" "debian=$DEBIAN_IMAGE" \
        "wine $(part_hash wine)" "wowbox64 $(part_hash wowbox64)" \
        "userland $(cat "$USERLAND_SRC/packages.lock" "$USERLAND_SRC/build-mesa.sh" "$USERLAND_SRC/userland.env.in" | sha256sum | cut -d' ' -f1)" \
        "bridge=$XINPUT_BRIDGE_SIZE $XINPUT_BRIDGE_SHA256" "generation=$PREFIX_GENERATION"
}

case "${1:-}" in
    --identity)
        identity
        exit 0
        ;;
    --stage-wine) # in the Wine image: the full Wine for the prefix build
        python3 /src/stage-wineb.py /cache/parts/wine/full /cache/parts/wowbox64/wowbox64.dll \
            /cache/work/prefix/wine "$USERLAND/lib/ld-linux-aarch64.so.1" --xtajit
        exit 0
        ;;
    --wineboot) # in the arm64 container
        export HOME=/files/home WINEPREFIX=/files/prefix WINEDEBUG=-all BOX64_LOG=0 BOX64_NOBANNER=1
        export WINEDLLOVERRIDES="mscoree=;mshtml=;winemenubuilder.exe=;winedbg.exe=d"
        export FONTCONFIG_FILE="$USERLAND/etc/fonts/fonts.conf"
        mkdir -p "$HOME"
        /wine/bin/wine wineboot -i
        /wine/bin/wine reg import 'Z:\files\prefix.reg'
        /wine/bin/wineserver -w
        exit 0
        ;;
    --finish) # in the tools image
        p=/cache/work/prefix/files/prefix
        # wineboot ends with 0 even when a part of the setup failed: the 64-bit side, the 32-bit
        # side (through wowbox64.dll) and the imported values must all be there.
        for f in drive_c/windows/system32/kernel32.dll drive_c/windows/system32/xtajit.dll \
            drive_c/windows/syswow64/kernel32.dll drive_c/windows/syswow64/winebrowser.exe; do
            [[ -f "$p/$f" ]] || { echo "prefix: wineboot made no $f" >&2; exit 1; }
        done
        grep -q '^\[Software\\\\Wine\\\\AppDefaults\\\\Game.exe\]' "$p/user.reg" &&
            grep -q '^"DisableHWAcceleration"=dword:00000001' "$p/user.reg" &&
            grep -q '^\[Software\\\\Wine\\\\X11 Driver\]' "$p/user.reg" &&
            grep -q '^"UseEGL"="N"' "$p/user.reg" ||
            { echo "prefix: the registry values were not imported" >&2; exit 1; }
        rm -rf "$p/.wineserver" "$p/drive_c/windows/sysarm32"
        echo disable > "$p/.update-timestamp"
        echo "$PREFIX_GENERATION" > "$p/.pd2-prefix-generation"
        echo plain > "$p/.pd2-prefix-template"
        python3 /src/prefix-dedupe.py "$p" /cache/work/prefix/wine "runner/$APP_RUNTIME_DIRECTORY"
        bridge="$p/drive_c/windows/syswow64/xinput1_4.dll"
        rm -f "$bridge"
        cp /cache/work/prefix/files/xinput1_4.dll "$bridge"
        mkdir -p "$p/drive_c/Program Files (x86)"
        ln -sfn "../../../runner/$APP_DOTNET_DIRECTORY" "$p/drive_c/Program Files (x86)/dotnet"
        rm -rf /cache/parts/prefix/prefix
        mv "$p" /cache/parts/prefix/prefix
        echo "prefix: $(find /cache/parts/prefix/prefix -type f | wc -l) files, $(find /cache/parts/prefix/prefix -type l | wc -l) links, $(du -sh /cache/parts/prefix/prefix | cut -f1)"
        exit 0
        ;;
    --golden) # in the tools image
        python3 -I /src/golden.py /cache/parts/prefix/prefix /cache/parts/prefix/golden /src/golden \
            /src/golden.reg "/cache/dl/$(basename "$VCREDIST_X64_URL")" \
            "/cache/dl/$(basename "$VCREDIST_X86_URL")" /cache/work/prefix/golden
        exit 0
        ;;
    --golden-check) # in the arm64 container: a copy of GOLDEN at /files/prefix
        export HOME=/files/home WINEPREFIX=/files/prefix WINEDEBUG=-all BOX64_LOG=0 BOX64_NOBANNER=1
        export WINEDLLOVERRIDES="mscoree=;mshtml=;winemenubuilder.exe=;winedbg.exe=d"
        export FONTCONFIG_FILE="$USERLAND/etc/fonts/fonts.conf"
        # The template's links lead to the runtime's Wine as the managed root lays it out.
        mkdir -p "$HOME" /files/runner
        ln -sfn /wine "/files/runner/$APP_RUNTIME_DIRECTORY"
        for query in 'HKLM\Software\Wow6432Node\Blizzard Entertainment\Diablo II|InstallPath' \
            'HKLM\Software\Classes\MSComctlLib.TabStrip\CLSID|' 'HKCU\Software\ShadowFlare\WinMPQ|InstallPath'; do
            key=${query%|*} value=${query#*|}
            answer=$(/wine/bin/wine reg query "$key" ${value:+/v "$value"}) || answer=""
            [[ $answer == *REG_SZ* ]] ||
                { echo "prefix: Wine does not read $key in the GOLDEN template" >&2; exit 1; }
        done
        /wine/bin/wineserver -w
        echo "prefix: Wine reads the GOLDEN template's keys"
        exit 0
        ;;
esac

for part in wine wowbox64; do
    [[ -f "$RUNTIME_PARTS/$part/BUILDINFO" ]] || { echo "prefix: build the part $part first" >&2; exit 1; }
done
[[ -f "$USERLAND_TREE/etc/userland.env" ]] || { echo "prefix: build the App userland first (userland/build.sh)" >&2; exit 1; }
# Before the slow build: the app reads the mark as a whole number (Kotlin's Int), so nine digits at most.
[[ $PREFIX_GENERATION =~ ^[1-9][0-9]{0,8}$ ]] ||
    { echo "prefix: PREFIX_GENERATION must be a whole number from 1 to 999999999" >&2; exit 1; }
id="$(identity)"
if part_ready prefix "$id"; then
    echo "prefix: reusing parts/prefix"
    exit 0
fi
grep -qs '^flags: .*F' /proc/sys/fs/binfmt_misc/qemu-aarch64 ||
    { echo "prefix: needs qemu-user for aarch64 in binfmt_misc with the F flag (for example qemu-user-static)" >&2; exit 1; }
rm -rf "$work" "$RUNTIME_PARTS/prefix"
mkdir -p "$work/files" "$RUNTIME_PARTS/prefix"
# The engine's sealed XInput bridge: xz in hex in its sources.
engine_src="$("$RUNTIME_DIR/../engine/build.sh" --source)"
xxd -r -p "$engine_src/src/install/xinput_bridge.hex" | xz -dc > "$work/files/xinput1_4.dll"
[[ "$(stat -c %s "$work/files/xinput1_4.dll")" == "$XINPUT_BRIDGE_SIZE" &&
    "$(sha256sum < "$work/files/xinput1_4.dll" | cut -d' ' -f1)" == "$XINPUT_BRIDGE_SHA256" ]] ||
    { echo "prefix: the engine's XInput bridge is not the one config.sh pins" >&2; exit 1; }
run_in "$(runtime_image wine)" prefix.sh --stage-wine
cat > "$work/files/prefix.reg" <<'EOF'
Windows Registry Editor Version 5.00

[HKEY_CURRENT_USER\Software\Wine\AppDefaults\Game.exe]
"Version"="win7"

[HKEY_CURRENT_USER\Software\Wine\X11 Driver]
"UseEGL"="N"

[HKEY_CURRENT_USER\Software\Microsoft\Avalon.Graphics]
"DisableHWAcceleration"=dword:00000001
EOF
engine_run --platform linux/arm64 -v "$RUNTIME_DIR:/src:ro" -v "$USERLAND_SRC:/userland:ro" \
    -v "$USERLAND_TREE:$USERLAND:ro" -v "$work/wine:/wine:ro" -v "$work/files:/files" "$DEBIAN_IMAGE" \
    bash /src/prefix.sh --wineboot
run_in "$(runtime_image tools)" prefix.sh --finish
fetch "$VCREDIST_X64_URL" "$VCREDIST_X64_SHA256" >/dev/null
fetch "$VCREDIST_X86_URL" "$VCREDIST_X86_SHA256" >/dev/null
run_in "$(runtime_image tools)" prefix.sh --golden
# The check runs on a copy: Wine must not write into the template.
mkdir -p "$work/check"
cp -a "$RUNTIME_PARTS/prefix/golden" "$work/check/prefix"
engine_run --platform linux/arm64 -v "$RUNTIME_DIR:/src:ro" -v "$USERLAND_SRC:/userland:ro" \
    -v "$USERLAND_TREE:$USERLAND:ro" -v "$work/wine:/wine:ro" -v "$work/check:/files" "$DEBIAN_IMAGE" \
    bash /src/prefix.sh --golden-check
part_done prefix "$id"
rm -rf "$work"
