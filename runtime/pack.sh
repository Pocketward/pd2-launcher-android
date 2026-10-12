#!/usr/bin/env bash
# Packs the runtime the app provides into the APK's assets (../app/src/main/assets/runtime/,
# gitignored): everything the Launcher would otherwise download for its x86 side, laid out under
# the Launcher's managed root as the Launcher keeps it, so the engine finds it installed and its
# Download fetches only Blizzard's wrappers and PD2:
#   fex/rootfs/Ubuntu_24_04    FEX's Ubuntu 24.04 RootFS (the engine's `ROOTFS_*` pins), unpacked
#                              with /tmp, /var/tmp, /dev/shm 1777 and the empty proc and sys removed,
#                              without what rootfs-trim.txt lists (guest Mesa, debuggers, build
#                              tools, interpreters, apt/dpkg/systemd, codecs, docs)
#   fex/{config,data,cache}/   FEX's folders
#   fex/installed.json         the record naming App-provided FEX (the engine's `AppRecord`)
#   runner/<identity>/         Wine-GE 8-26 (the engine's `WINE_GE_8_26`), its archive's top
#                              folder stripped, with the Launcher's receipt (archive SHA-512),
#                              without share/wine/gecko: the archive bundles Gecko 2.47.3, but this
#                              Wine's mshtml.dll looks only for wine-gecko-2.47.4 folders, so the
#                              bundled copy is never loaded; Gecko 2.47.4 comes from the MSI below;
#                              and without share/wine/mono: the archive bundles Wine Mono 8.1.0,
#                              but the Mono 11.3.0 MSI below is installed into the prefix, and
#                              mscoree loads C:\windows\mono\mono-2.0 before it looks in Wine's
#                              share folder, so the bundled copy is never loaded
#   cache/artifacts/           Wine Gecko, Wine Mono and the .NET 6 Desktop Runtime x86 installer
#                              (for the official PD2 launcher), verified, under the Launcher's cache
#                              names (the engine's `cache_name`)
# Output: runtime.tar.zst (GNU tar, sorted, owner 0, mtime 0; zstd) and runtime.info (line 1: the
# version, a SHA-256 prefix of the pins; line 2: the tar's size in bytes, for progress).
# The pins are read at run time from the engine's sources, which ../engine/build.sh --source
# names (the pinned release, or PD2_ENGINE_SOURCE). Downloads and unpacked trees are cached in
# ~/.cache/pd2-android/guest/.
# Needs curl, tar, zstd, sha256sum/sha512sum and unsquashfs on the host.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cache="$HOME/.cache/pd2-android/guest"
assets="$here/../app/src/main/assets/runtime"
die() { printf 'runtime/pack.sh: %s\n' "$*" >&2; exit 1; }
say() { printf '== %s\n' "$*"; }

engine="$("$here/../engine/build.sh" --source)"
fex_src="$engine/src/builtin_fex.rs"
rt_src="$engine/src/runtime.rs"
next_string() { sed -n "/$1/{n;s/.*\"\(.*\)\".*/\1/p;}" "$2" | head -1; }
rootfs_url="$(next_string 'const ROOTFS_URL' "$fex_src")"
rootfs_sha256="$(next_string 'const ROOTFS_SHA256' "$fex_src")"
rootfs_size="$(sed -n 's/.*const ROOTFS_SIZE: u64 = \([0-9_]*\);.*/\1/p' "$fex_src" | tr -d _)"
wine_block="$(sed -n '/const WINE_GE_8_26: ManagedRuntime/,/^};/p' "$rt_src")"
field() { sed -n "s/.*$1: \"\{0,1\}\([^\",]*\)\"\{0,1\},.*/\1/p" | head -1; }
wine_identity="$(field identity <<<"$wine_block")"
wine_url="$(field archive_url <<<"$wine_block")"
wine_size="$(field archive_size <<<"$wine_block" | tr -d _)"
wine_sha512="$(field archive_sha512 <<<"$wine_block")"
gecko_url="$(grep -o 'https://dl.winehq.org/wine/wine-gecko/[^"]*\.msi' "$engine/src/install.rs" | head -1)"
gecko_sha256="$(grep -A8 "$gecko_url" "$engine/src/install.rs" | sed -n 's/.*"\([0-9a-f]\{64\}\)".*/\1/p' | head -1)"
gecko_size="$(grep -A8 "$gecko_url" "$engine/src/install.rs" | sed -n 's/.*SizePolicy::Exact(\([0-9_]*\)).*/\1/p' | head -1 | tr -d _)"
mono_url="$(sed -n 's/.*const WINE_MONO_URL: &str = "\(.*\)";.*/\1/p' "$engine/src/install.rs")"
mono_sha256="$(next_string 'pub const WINE_MONO_MSI_SHA256' "$engine/src/audit.rs")"
mono_size="$(grep -A4 'request_url: Cow::Borrowed(WINE_MONO_URL)' "$engine/src/install.rs" | sed -n 's/.*SizePolicy::Exact(\([0-9_]*\)).*/\1/p' | tr -d _)"
gecko_cache="$(sed -n 's/.*ArtifactKind::Gecko => "\(.*\)",/\1/p' "$engine/src/install/adapters.rs" | head -1)"
mono_cache="$(sed -n 's/.*ArtifactKind::WineMono => "\(.*\)",/\1/p' "$engine/src/install/adapters.rs" | head -1)"
dotnet_url="$(sed -n 's/.*const DOTNET_DESKTOP_URL: &str = "\(.*\)";.*/\1/p' "$engine/src/install.rs")"
dotnet_spec="$(grep -A10 'request_url: Cow::Borrowed(DOTNET_DESKTOP_URL)' "$engine/src/install.rs")"
dotnet_size="$(sed -n 's/.*SizePolicy::Exact(\([0-9_]*\)).*/\1/p' <<<"$dotnet_spec" | head -1 | tr -d _)"
dotnet_sha512="$(sed -n 's/.*"\([0-9a-f]\{128\}\)".*/\1/p' <<<"$dotnet_spec" | head -1)"
dotnet_cache="$(sed -n 's/.*ArtifactKind::DotnetDesktop => "\(.*\.verified\)",/\1/p' "$engine/src/install/adapters.rs" | head -1)"
for v in rootfs_url rootfs_sha256 rootfs_size wine_identity wine_url wine_size wine_sha512 \
    gecko_url gecko_sha256 gecko_size mono_url mono_sha256 mono_size gecko_cache mono_cache \
    dotnet_url dotnet_size dotnet_sha512 dotnet_cache; do
    [[ -n "${!v}" ]] || die "pin $v not found in $engine/src/ (did a pin's format change?)"
done

mkdir -p "$cache"
fetch() { # url file size algo hash
    local url=$1 file=$2 size=$3 algo=$4 hash=$5
    if [[ ! -f "$file" ]]; then
        say "downloading $url"
        curl -fsSL --retry 3 -o "$file.part" "$url"
        mv "$file.part" "$file"
    fi
    [[ "$(stat -c %s "$file")" == "$size" ]] || die "$file: size differs from the pin $size"
    echo "$hash  $file" | "${algo}sum" -c --quiet - || die "$file: $algo differs from the pin"
}

rootfs_file="$cache/Ubuntu_24_04-$rootfs_sha256.sqsh"
rootfs_tree="$cache/rootfs-$rootfs_sha256/tree.tar.gz"
if [[ ! -f "$rootfs_tree" ]]; then
    fetch "$rootfs_url" "$rootfs_file" "$rootfs_size" sha256 "$rootfs_sha256"
    tree="$(dirname "$rootfs_tree")/tree"
    rm -rf "$tree" && mkdir -p "$(dirname "$tree")"
    # As an ordinary user: device nodes cannot be made and are skipped, as the Launcher does.
    unsquashfs -q -n -no-xattrs -ignore-errors -d "$tree" "$rootfs_file" >/dev/null 2>&1 || true
    [[ -x "$tree/usr/bin/bash" ]] || die "RootFS unpack failed"
    find "$tree" \( -type c -o -type b -o -type p -o -type s \) -delete
    chmod -R ug-s "$tree"
    find "$tree" -type d -exec chmod u+rwx {} +
    tar --hard-dereference --numeric-owner -C "$tree" -czf "$rootfs_tree.part" .
    mv "$rootfs_tree.part" "$rootfs_tree"
    rm -rf "$tree"
fi
wine_file="$cache/$(basename "$wine_url")"
wine_tree="$cache/wine-${wine_sha512:0:16}/tree.tar.gz"
if [[ ! -f "$wine_tree" ]]; then
    fetch "$wine_url" "$wine_file" "$wine_size" sha512 "$wine_sha512"
    tree="$(dirname "$wine_tree")/tree"
    rm -rf "$tree" && mkdir -p "$tree"
    tar -C "$tree" --strip-components=1 -xJf "$wine_file"
    tar --hard-dereference --numeric-owner -C "$tree" -czf "$wine_tree.part" .
    mv "$wine_tree.part" "$wine_tree"
    rm -rf "$tree"
fi
gecko_file="$cache/$(basename "$gecko_url")"
mono_file="$cache/$(basename "$mono_url")"
fetch "$gecko_url" "$gecko_file" "$gecko_size" sha256 "$gecko_sha256"
fetch "$mono_url" "$mono_file" "$mono_size" sha256 "$mono_sha256"
dotnet_file="$cache/$(basename "$dotnet_url")"
fetch "$dotnet_url" "$dotnet_file" "$dotnet_size" sha512 "$dotnet_sha512"

version="$(printf '%s\n' "$rootfs_sha256" "$wine_sha512" "$gecko_sha256" "$mono_sha256" \
    "$dotnet_sha512" "$wine_identity" "$gecko_cache" "$mono_cache" "$dotnet_cache" "$(sha256sum "$0" | cut -d' ' -f1)" \
    "$(sha256sum "$here/rootfs-trim.txt" | cut -d' ' -f1)" | sha256sum | cut -c1-16)"
if [[ -f "$assets/runtime.info" && "$(head -1 "$assets/runtime.info")" == "$version" && -f "$assets/runtime.tar.zst" ]]; then
    say "runtime $version already packed"
    exit 0
fi

staging="$cache/runtime-staging"
rm -rf "$staging"
mkdir -p "$staging/fex/rootfs/Ubuntu_24_04" "$staging/fex/config" "$staging/fex/data" "$staging/fex/cache" \
    "$staging/runner/$wine_identity" "$staging/cache/artifacts"
say "staging the RootFS"
tar -C "$staging/fex/rootfs/Ubuntu_24_04" -xzf "$rootfs_tree"
rootfs="$staging/fex/rootfs/Ubuntu_24_04"
for d in proc sys; do rmdir "$rootfs/$d" 2>/dev/null || true; done
say "staging Wine-GE"
wine="$staging/runner/$wine_identity"
tar -C "$wine" -xzf "$wine_tree"
rm -rf "$wine/share/wine/gecko" "$wine/share/wine/mono"
printf '%s' "$wine_sha512" > "$wine/.pd2launcher-archive.sha512"
# readelf fails on the files that are not ELF; only its output counts. /dev/null first, so that
# readelf names every file ("File: ...") however find batches them.
# "<ELF class> <name>" for every DT_NEEDED entry of the ELF files under the given folders.
needed() {
    { find "$@" -type f -exec readelf -hdW /dev/null {} + 2>/dev/null || true; } | awk '
        /^  Class:/ { class = $2 }
        /\(NEEDED\)/ { n = $0; sub(/.*\[/, "", n); sub(/\]$/, "", n); print class, n }' | sort -u
}
# "<ELF class> <file name>" for every shared library under the given folders.
provided() {
    { find "$@" -name '*.so*' \( -type f -o -xtype f \) -exec readelf -hW /dev/null {} + 2>/dev/null \
        || true; } | awk '
        /^File: / { k = split($0, p, "/"); name = p[k] }
        /^  Class:/ { print $2, name }' | sort -u
}
# The DT_NEEDED names, per class (i386 and x86_64 apart), that no kept library of that class
# provides: the RootFS's programs and libraries and Wine-GE's own libraries (its lib and lib64,
# where its FFmpeg and GStreamer pieces link RootFS libraries) against the RootFS's library folders
# and Wine-GE's. The RootFS ships a few unresolved names already; only new ones count.
unresolved() {
    comm -23 <(needed "$rootfs/usr/bin" "$rootfs/usr/sbin" "$rootfs/usr/lib" "$rootfs/usr/libexec" \
        "$wine/lib" "$wine/lib64") \
        <(provided "$rootfs/usr/lib" "$rootfs/usr/lib64" "$rootfs/usr/lib32" "$wine/lib" "$wine/lib64")
}
say "trimming the RootFS (rootfs-trim.txt)"
before="$(unresolved)"
shopt -s globstar nullglob
while read -r pattern; do
    pattern="${pattern%%#*}"; pattern="${pattern%"${pattern##*[![:space:]]}"}"
    [[ -n "$pattern" ]] || continue
    [[ "$pattern" == L/* ]] && pattern="usr/lib/*-linux-gnu/${pattern#L/}"
    # Unquoted: the pattern is a glob.
    for path in "$rootfs"/$pattern; do rm -rf -- "$path"; done
done < "$here/rootfs-trim.txt"
shopt -u globstar nullglob
broken="$(comm -13 <(printf '%s\n' "$before") <(unresolved))"
[[ -z "$broken" ]] || die "rootfs-trim.txt removes libraries the RootFS or Wine-GE still links (class, name): $(tr '\n' ' ' <<<"$broken")"
mkdir -p "$rootfs/tmp" "$rootfs/var/tmp" "$rootfs/dev/shm"
chmod 1777 "$rootfs/tmp" "$rootfs/var/tmp" "$rootfs/dev/shm"
printf '{"schema":1,"fex":"app-provided","rootfs_sha256":"%s","fex_probe_passed":false}' "$rootfs_sha256" \
    > "$staging/fex/installed.json"
cp "$gecko_file" "$staging/cache/artifacts/$gecko_cache"
cp "$mono_file" "$staging/cache/artifacts/$mono_cache"
cp "$dotnet_file" "$staging/cache/artifacts/$dotnet_cache"
chmod 0644 "$staging/cache/artifacts/"*
find "$staging/fex/config" "$staging/fex/data" "$staging/fex/cache" "$staging/runner" "$staging/cache" -maxdepth 0 -exec chmod 0700 {} +

say "packing"
mkdir -p "$assets"
tar --sort=name --format=gnu --numeric-owner --owner=0 --group=0 --mtime=@0 --hard-dereference \
    -C "$staging" -cf "$cache/runtime.tar" .
bytes="$(stat -c %s "$cache/runtime.tar")"
zstd -q -f -T0 -12 "$cache/runtime.tar" -o "$assets/runtime.tar.zst"
printf '%s\n%s\n' "$version" "$bytes" > "$assets/runtime.info"
rm -rf "$staging" "$cache/runtime.tar"
say "runtime $version: $((bytes >> 20)) MiB, $(( $(stat -c %s "$assets/runtime.tar.zst") >> 20 )) MiB compressed, in $assets"
