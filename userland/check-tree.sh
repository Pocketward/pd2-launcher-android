#!/usr/bin/env bash
# Checks the packed App userland as the device will see it: unpacks out/userland.tar.zst and each
# part (out/parts/<name>/userland.tar.zst, pack.sh) into one scratch directory, as the app does,
# and verifies
#   - each part's archive holds exactly the files its manifest lists, the main archive none of
#     them, and the parts are built for the main manifest's placeholder and prefix; the checks
#     below take the main manifest with the parts' entries (work/check-manifest.json)
#   - no hard-link entries in the archive and no hard links in the unpacked tree
#   - no absolute symlinks
#   - every ELF with a program interpreter requests $USERLAND/lib/ld-linux-aarch64.so.1 (readelf -l),
#     and that loader is in the tree
#   - the Vulkan ICD JSON's library_path is the absolute $USERLAND path of a file in the tree
#   - usr/bin/x-input-relay is there and every library it needs is in the tree
#   - PanVK (build-panvk.sh) is there: its ICD JSON and libvulkan_panfrost.so, every library it
#     needs in the tree
#   - the system-driver mode (build-sysdrv.sh) is there: the Venus ICD JSON and libvulkan_virtio.so,
#     the second GL stack as glvnd vendor "pd2sysdrv" (libGLX_pd2sysdrv.so.0, also its soname),
#     every library they need in the tree
#   - the libhybris route (build-hybris.sh) is there: its ICD JSON, libvulkan_pd2hybris.so,
#     libhybris-common.so.1, its linker plugin libhybris/linker/q.so and its licence texts, every
#     library they need in the tree
#   - RADV for Xclipse (build-radv.sh) is there: its ICD JSON, libvulkan_radeon.so with no shared
#     libdrm and every library it needs in the tree, its licence and notices
#   - mali-panvk (build-mali-panvk.sh) is there: its ICD JSON outside icd.d, naming its library at the
#     prefix, every library it needs in the tree, its licence and notices
#   - the manifest matches the tree (every entry, type, mode, size, SHA-256)
#   - the placeholder (config.sh): the manifest names it and the prefix, and lists for every file
#     exactly the offsets where the tree holds it, rewritten as text exactly when the file is not
#     an ELF and has no NUL byte; no partial copy of it anywhere, no symlink to it
#   - the critical set: every path is a file of the tree as the loader or a program opens it,
#     the engine's libraries (ENGINE_NEEDED) are in it, and so is every library an ELF in it needs;
#     the absent paths are not in the tree
#   - when the manifest has a "glibc" record (PD2_USERLAND_WITH_GLIBC=1): every recorded file is in
#     the tree with its SHA-256; the record's patch, build.sh and Containerfile hashes are those
#     in glibc/; ld.so and libc.so.6 equal glibc/out/install's copies stripped as build-glibc.sh
#     strips them and carry our version string; both loader paths resolve to our loader; no regular file of Debian's
#     libc6 .deb is left in the tree (files in the record are ours, even where byte-identical to
#     Debian's, such as gconv-modules)
#     and etc/userland.env does not set LD_LIBRARY_PATH
#   - last, the tree rewritten for a scratch data folder as the app rewrites it (padded binaries,
#     exact text files) holds no placeholder, and with the host's qemu-aarch64 binfmt handler
#     Xvfb prints its usage and xkbcomp its version from there:
#     interpreter, run paths and the loader's own folders all at the padded path
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=config.sh
. "$here/config.sh"

if [[ "${PD2_USERLAND_IN_CONTAINER:-}" != 1 ]]; then
    # The host's binfmt_misc, which the container does not see: with qemu-aarch64 there (its F flag
    # makes it work in the container), the relocated tree's programs run below.
    binfmt=0
    grep -qsx enabled /proc/sys/fs/binfmt_misc/qemu-aarch64 && binfmt=1
    RUN_EXTRA=(-v "$USERLAND_OUT:/out:ro" -e "PD2_HOST_BINFMT_AARCH64=$binfmt")
    run_in_tools check-tree.sh "$@"
    exit
fi

archive=/out/userland.tar.zst
# A data folder as on the device: the tree in files/userland/<version>, current naming it.
data=/cache/work/check
tree="$data/files/userland/tree"
rm -rf "$data"
mkdir -p "$tree"
ln -s tree "$data/files/userland/current"
fail=0
bad() { echo "FAIL: $*" >&2; fail=1; }

hardlinks="$(zstd -dc "$archive" | tar -tv | grep -c '^h' || true)"
[[ "$hardlinks" == 0 ]] || bad "$hardlinks hard-link entries in the archive"
zstd -dc "$archive" | tar -x -p -C "$tree"

# The parts, unpacked over the main tree as the app does, and the manifest the checks below take.
manifest=/cache/work/check-manifest.json
rm -f "$manifest"
part_dirs=()
for d in /out/parts/*/; do [[ -f "$d/manifest.json" ]] && part_dirs+=("${d%/}"); done
for d in "${part_dirs[@]}"; do
    part="$(basename "$d")"
    n="$(zstd -dc "$d/userland.tar.zst" | tar -tv | grep -vc '^-' || true)"
    [[ "$n" == 0 ]] || bad "part $part: $n entries that are not regular files"
    diff <(zstd -dc "$d/userland.tar.zst" | tar -t | LC_ALL=C sort) <(jq -r '.entries[].path' "$d/manifest.json" | LC_ALL=C sort) >/dev/null \
        || bad "part $part: its archive does not hold exactly its manifest's files"
    # grep reads all of the listing (no -q): an early end would break the pipe and hide the match.
    if zstd -dc "$archive" | tar -t | grep -xFf <(jq -r '.entries[].path' "$d/manifest.json") >/dev/null; then
        bad "part $part: the main archive also holds its files"
    fi
    zstd -dc "$d/userland.tar.zst" | tar -x -p -C "$tree"
done
python3 - /out/manifest.json "$manifest" "${part_dirs[@]}" <<'EOF' || fail=1
import json, sys
main, merged, *parts = sys.argv[1:]
m = json.load(open(main))
paths = {e['path'] for e in m['entries']}
for d in parts:
    p = json.load(open(d + '/manifest.json'))
    if (p['placeholder'], p['prefix']) != (m['placeholder'], m['prefix']):
        sys.exit(f"FAIL: part {p.get('part')} is built for another placeholder or prefix")
    if p['critical'] or p['absent']:
        sys.exit(f"FAIL: part {p.get('part')} has critical or absent paths")
    both = paths & {e['path'] for e in p['entries']}
    if both:
        sys.exit(f"FAIL: part {p.get('part')} and the main manifest both list {sorted(both)[0]}")
    paths |= {e['path'] for e in p['entries']}
    m['entries'] += p['entries']
    print(f"part {p.get('part')} {p['version']}: {len(p['entries'])} files, unpacked over the main tree")
json.dump(m, open(merged, 'w'))
EOF
n="$(find "$tree" -type f -links +1 | wc -l)"
[[ "$n" == 0 ]] || bad "$n hard-linked files in the unpacked tree"

abs="$(find "$tree" -type l -lname '/*' | wc -l)"
[[ "$abs" == 0 ]] || bad "$abs absolute symlinks"

interp="$USERLAND/lib/ld-linux-aarch64.so.1"
elves=0; with_interp=0
while IFS= read -r -d '' f; do
    [[ "$(head -c4 "$f" | od -An -tx1 | tr -d ' \n')" == 7f454c46 ]] || continue
    elves=$((elves + 1))
    got="$(aarch64-linux-gnu-readelf -l "$f" 2>/dev/null | sed -n 's/.*Requesting program interpreter: \(.*\)\]$/\1/p')"
    [[ -z "$got" ]] && continue
    with_interp=$((with_interp + 1))
    [[ "$got" == "$interp" ]] || bad "${f#"$tree"/}: interpreter $got"
done < <(find "$tree" -type f -print0)
[[ -e "$tree/lib/ld-linux-aarch64.so.1" ]] || bad "loader missing at lib/ld-linux-aarch64.so.1"
echo "ELF files: $elves; with an interpreter: $with_interp (all must be $interp)"

for icd in "$tree"/usr/share/vulkan/icd.d/*.json; do
    lib="$(jq -r .ICD.library_path "$icd")"
    echo "ICD ${icd#"$tree"/}: library_path $lib"
    [[ "$lib" == "$USERLAND"/* && -f "$tree${lib#"$USERLAND"}" ]] || bad "ICD library_path $lib not in the tree"
done

relay="$tree/usr/bin/x-input-relay"
if [[ -f "$relay" ]]; then
    for lib in $(aarch64-linux-gnu-readelf -d "$relay" | sed -n 's/.*(NEEDED).*\[\(.*\)\]$/\1/p'); do
        [[ -e "$tree/usr/lib/aarch64-linux-gnu/$lib" || -e "$tree/lib/aarch64-linux-gnu/$lib" ]] \
            || bad "usr/bin/x-input-relay: $lib not in the tree"
    done
    echo "x-input-relay: every NEEDED library is in the tree"
else
    bad "usr/bin/x-input-relay missing"
fi

panvk="$tree/usr/lib/aarch64-linux-gnu/libvulkan_panfrost.so"
if [[ -f "$tree/usr/share/vulkan/icd.d/panfrost_icd.aarch64.json" && -f "$panvk" ]]; then
    for lib in $(aarch64-linux-gnu-readelf -d "$panvk" | sed -n 's/.*(NEEDED).*\[\(.*\)\]$/\1/p'); do
        [[ -e "$tree/usr/lib/aarch64-linux-gnu/$lib" || -e "$tree/lib/aarch64-linux-gnu/$lib" ]] \
            || bad "libvulkan_panfrost.so: $lib not in the tree"
    done
    echo "PanVK: ICD and libvulkan_panfrost.so present, every NEEDED library is in the tree"
else
    bad "PanVK missing (usr/share/vulkan/icd.d/panfrost_icd.aarch64.json, libvulkan_panfrost.so)"
fi

# The system-driver mode (build-sysdrv.sh): the Venus ICD, and the second GL stack as glvnd's vendor
# "pd2sysdrv" (libGLX_pd2sysdrv.so.0, its soname too) with the libgallium it needs, every library
# they need in the tree.
venus="$tree/usr/lib/aarch64-linux-gnu/libvulkan_virtio.so"
sysglx="$tree/usr/lib/aarch64-linux-gnu/libGLX_pd2sysdrv.so.0"
if [[ -f "$tree/usr/share/vulkan/icd.d/virtio_icd.aarch64.json" && -f "$venus" && -f "$sysglx" ]]; then
    aarch64-linux-gnu-readelf -d "$sysglx" | grep -q 'Library soname: \[libGLX_pd2sysdrv.so.0\]' \
        || bad "libGLX_pd2sysdrv.so.0: its soname is not libGLX_pd2sysdrv.so.0"
    for elf in "$venus" "$sysglx"; do
        for lib in $(aarch64-linux-gnu-readelf -d "$elf" | sed -n 's/.*(NEEDED).*\[\(.*\)\]$/\1/p'); do
            [[ -e "$tree/usr/lib/aarch64-linux-gnu/$lib" || -e "$tree/lib/aarch64-linux-gnu/$lib" ]] \
                || bad "${elf#"$tree/"}: $lib not in the tree"
        done
    done
    echo "system driver: Venus ICD and the pd2sysdrv GL vendor present, every NEEDED library is in the tree"
else
    bad "system driver missing (virtio_icd.aarch64.json, libvulkan_virtio.so, libGLX_pd2sysdrv.so.0)"
fi

# The libhybris route (build-hybris.sh): our ICD with its JSON, libhybris-common.so.1 and its linker
# plugin in its compiled-in folder, the licence texts; every library they need in the tree (the
# phone's own bionic libraries are not: libhybris loads those from /system and /vendor).
hybicd="$tree/usr/lib/aarch64-linux-gnu/libvulkan_pd2hybris.so"
hybcommon="$tree/usr/lib/aarch64-linux-gnu/libhybris-common.so.1"
hybq="$tree/usr/lib/aarch64-linux-gnu/libhybris/linker/q.so"
if [[ -f "$tree/usr/share/vulkan/icd.d/pd2hybris_icd.aarch64.json" && -f "$hybicd" && -f "$hybcommon" && -f "$hybq" ]]; then
    for elf in "$hybicd" "$hybcommon" "$hybq"; do
        for lib in $(aarch64-linux-gnu-readelf -d "$elf" | sed -n 's/.*(NEEDED).*\[\(.*\)\]$/\1/p'); do
            [[ -e "$tree/usr/lib/aarch64-linux-gnu/$lib" || -e "$tree/lib/aarch64-linux-gnu/$lib" ]] \
                || bad "${elf#"$tree/"}: $lib not in the tree"
        done
    done
    for l in README LICENSE.Apache2 LICENSE.BSD-2 LICENSE.ISC; do
        [[ -f "$tree/usr/share/licenses/pd2-libhybris/$l" ]] || bad "libhybris: no usr/share/licenses/pd2-libhybris/$l"
    done
    echo "libhybris route: ICD, libhybris-common.so.1, linker q.so and licences present, every NEEDED library is in the tree"
else
    bad "libhybris route missing (pd2hybris_icd.aarch64.json, libvulkan_pd2hybris.so, libhybris-common.so.1, libhybris/linker/q.so)"
fi

# RADV for Xclipse (build-radv.sh): its ICD and libvulkan_radeon.so, every library it needs in the
# tree, no shared libdrm (its libdrm 2.4.133 is in it), and its licence and notices.
radv="$tree/usr/lib/aarch64-linux-gnu/libvulkan_radeon.so"
if [[ -f "$tree/usr/share/vulkan/icd.d/radeon_icd.aarch64.json" && -f "$radv" ]]; then
    for lib in $(aarch64-linux-gnu-readelf -d "$radv" | sed -n 's/.*(NEEDED).*\[\(.*\)\]$/\1/p'); do
        [[ "$lib" != libdrm* ]] || bad "libvulkan_radeon.so needs a shared $lib"
        [[ -e "$tree/usr/lib/aarch64-linux-gnu/$lib" || -e "$tree/lib/aarch64-linux-gnu/$lib" ]] \
            || bad "libvulkan_radeon.so: $lib not in the tree"
    done
    for f in LICENSE NOTICE.txt THIRD_PARTY_NOTICES.md; do
        [[ -f "$tree/usr/share/licenses/radv-xclipse/$f" ]] || bad "RADV: usr/share/licenses/radv-xclipse/$f missing"
    done
    echo "RADV for Xclipse: ICD, libvulkan_radeon.so and its notices present, every NEEDED library is in the tree"
else
    bad "RADV for Xclipse missing (radeon_icd.aarch64.json, libvulkan_radeon.so)"
fi

# mali-panvk (build-mali-panvk.sh): its ICD (not in icd.d: only a VK_DRIVER_FILES that names it
# loads it) names its library at the prefix, every library it needs is in the tree, and its
# licence and notices are there.
mpanvk=usr/lib/aarch64-linux-gnu/mali-panvk/libvulkan_panfrost.so
mpanvk_icd="$tree/usr/share/mali-panvk/mali_panvk_icd.aarch64.json"
if [[ -f "$mpanvk_icd" && -f "$tree/$mpanvk" ]]; then
    [[ "$(jq -r .ICD.library_path "$mpanvk_icd")" == "$USERLAND/$mpanvk" ]] \
        || bad "mali-panvk ICD library_path is not $USERLAND/$mpanvk"
    [[ ! -e "$tree/usr/share/vulkan/icd.d/mali_panvk_icd.aarch64.json" ]] || bad "mali-panvk ICD in icd.d"
    for lib in $(aarch64-linux-gnu-readelf -d "$tree/$mpanvk" | sed -n 's/.*(NEEDED).*\[\(.*\)\]$/\1/p'); do
        [[ -e "$tree/usr/lib/aarch64-linux-gnu/$lib" || -e "$tree/lib/aarch64-linux-gnu/$lib" ]] \
            || bad "mali-panvk libvulkan_panfrost.so: $lib not in the tree"
    done
    for f in LICENSE NOTICE.txt THIRD_PARTY_NOTICES.md COPYRIGHT-HOLDERS.txt; do
        [[ -f "$tree/usr/share/licenses/mali-panvk/$f" ]] || bad "mali-panvk: usr/share/licenses/mali-panvk/$f missing"
    done
    echo "mali-panvk: ICD, its libvulkan_panfrost.so and its notices present, every NEEDED library is in the tree"
else
    bad "mali-panvk missing (usr/share/mali-panvk/mali_panvk_icd.aarch64.json, $mpanvk)"
fi

python3 - "$tree" "$manifest" <<'EOF' || fail=1
import hashlib, json, os, stat, sys
tree, manifest = sys.argv[1:]
m = json.load(open(manifest))
seen = set()
for e in m['entries']:
    p = os.path.join(tree, e['path']); seen.add(e['path'])
    st = os.lstat(p)
    if format(stat.S_IMODE(st.st_mode), '04o') != e['mode'] and e['type'] != 'symlink':
        sys.exit(f"FAIL: mode of {e['path']}")
    if e['type'] == 'symlink':
        assert os.readlink(p) == e['target'], e['path']
    elif e['type'] == 'file':
        assert st.st_size == e['size'], e['path']
        assert hashlib.sha256(open(p, 'rb').read()).hexdigest() == e['sha256'], e['path']
for dirpath, dirnames, filenames in os.walk(tree):
    for n in dirnames + filenames:
        rel = os.path.relpath(os.path.join(dirpath, n), tree)
        if rel not in seen:
            sys.exit(f"FAIL: {rel} not in the manifest")
print(f"manifest {m['version']}: {len(m['entries'])} entries match the unpacked tree")
EOF

python3 - "$tree" "$manifest" "$DATA_PLACEHOLDER" "$USERLAND" "${ENGINE_NEEDED[*]}" <<'EOF' || fail=1
import json, os, re, subprocess, sys
tree, manifest, placeholder, prefix, engine_needed = sys.argv[1:]
m = json.load(open(manifest))
P = placeholder.encode()
head = P[:32]
bad = []
if m.get('placeholder') != placeholder or m.get('prefix') != prefix:
    bad.append(f"manifest placeholder/prefix {m.get('placeholder')!r} {m.get('prefix')!r}, not config.sh's")
entries = {e['path']: e for e in m['entries']}
relocated = occurrences = 0
for path, e in entries.items():
    full = os.path.join(tree, path)
    if e['type'] == 'symlink':
        if head in e['target'].encode():
            bad.append(f'{path}: symlink to the placeholder')
        continue
    if e['type'] != 'file':
        continue
    data = open(full, 'rb').read()
    found, i = [], data.find(P)
    while i >= 0:
        found.append(i)
        i = data.find(P, i + 1)
    if data.count(head) != len(found):
        bad.append(f'{path}: a partial copy of the placeholder')
    if found != e.get('placeholders', []):
        bad.append(f'{path}: placeholder at {found}, manifest {e.get("placeholders", [])}')
    text = bool(found) and data[:4] != b'\x7fELF' and b'\0' not in data
    if e.get('text', False) != text:
        bad.append(f'{path}: text {e.get("text", False)}, should be {text}')
    if found:
        relocated += 1
        occurrences += len(found)

def resolve(path):
    """path's file entry, links followed through the manifest; None if it is not one."""
    parts, i, hops = path.split('/'), 0, 0
    while i < len(parts):
        e = entries.get('/'.join(parts[:i + 1]))
        if e is None:
            return None
        if e['type'] == 'symlink':
            hops += 1
            if hops > 16 or e['target'].startswith('/'):
                return None
            joined = []
            for part in parts[:i] + e['target'].split('/') + parts[i + 1:]:
                if part == '..':
                    if not joined:
                        return None
                    joined.pop()
                elif part not in ('', '.'):
                    joined.append(part)
            parts, i = joined, 0
            continue
        if e['type'] == 'file' and i != len(parts) - 1:
            return None
        i += 1
    e = entries.get('/'.join(parts))
    return e if e and e['type'] == 'file' else None

critical = m.get('critical', [])
for path in critical:
    if resolve(path) is None:
        bad.append(f'critical {path}: not a file of the tree')
for path in ('lib/ld-linux-aarch64.so.1', 'etc/userland.env', 'etc/nsswitch.conf', 'etc/ssl/certs/ca-certificates.crt'):
    if path not in critical:
        bad.append(f'{path}: not critical')
for path in m.get('absent', []):
    if path in entries:
        bad.append(f'{path}: must not be in the tree')
if 'etc/ld.so.cache' not in m.get('absent', []):
    bad.append('etc/ld.so.cache: not absent')
if m.get('engine_needed') != engine_needed.split():
    bad.append("engine_needed is not config.sh's ENGINE_NEEDED")
# Closed: every library the engine or a critical ELF needs is critical (in a folder the loader searches).
libs = lambda soname: {f'{d}/{soname}' for d in ('lib/aarch64-linux-gnu', 'usr/lib/aarch64-linux-gnu')}
needs = {(s, 'the engine') for s in engine_needed.split()}
for path in critical:
    e = resolve(path)
    if e is None:
        continue
    full = os.path.join(tree, e['path'])
    if open(full, 'rb').read(4) != b'\x7fELF':
        continue
    dyn = subprocess.run(['aarch64-linux-gnu-readelf', '-d', full], capture_output=True, text=True, check=True).stdout
    needs |= {(s, path) for s in re.findall(r'\(NEEDED\)\s+Shared library: \[(.+?)\]', dyn)}
for soname, by in sorted(needs):
    if soname != 'ld-linux-aarch64.so.1' and not libs(soname) & set(critical):
        bad.append(f'{soname} (needed by {by}) is not critical')
for b in bad:
    print('FAIL:', b, file=sys.stderr)
print(f'placeholder: in {relocated} files, {occurrences} times, as the manifest lists; '
      f'{len(critical)} critical paths, closed under NEEDED; absent: {", ".join(m.get("absent", []))}')
sys.exit(1 if bad else 0)
EOF

if jq -e .glibc /out/manifest.json >/dev/null; then
    scratch=/cache/work/check-glibc
    rm -rf "$scratch"
    mkdir -p "$scratch/libc6"
    for p in lib/ld-linux-aarch64.so.1 lib/aarch64-linux-gnu/libc.so.6; do
        cp "/src/glibc/out/install/$p" "$scratch/$(basename "$p")"
        aarch64-linux-gnu-strip --strip-debug "$scratch/$(basename "$p")"
    done
    deb="/cache/debs/$(basename "$(awk '$1 == "runtime" && $2 == "libc6" {print $7}' /src/packages.lock)")"
    dpkg-deb --fsys-tarfile "$deb" | tar -x -C "$scratch/libc6"
    python3 - "$tree" "$manifest" "$scratch" /src/glibc/patches <<'EOF' || fail=1
import hashlib, json, os, sys
tree, manifest, scratch, patchdir = sys.argv[1:]
g = json.load(open(manifest))['glibc']
sha = lambda p: hashlib.sha256(open(p, 'rb').read()).hexdigest()
bad = []
for rel, want in g['files'].items():
    p = os.path.join(tree, rel)
    if os.path.islink(p) or not os.path.isfile(p) or sha(p) != want:
        bad.append(f'{rel}: not the recorded file')
patches = {n: sha(os.path.join(patchdir, n)) for n in sorted(os.listdir(patchdir)) if n.endswith('.patch')}
if g['buildinfo']['patches'] != patches:
    bad.append('recorded patch hashes differ from glibc/patches')
inputs = {n: sha(os.path.join(os.path.dirname(patchdir), n)) for n in ('build.sh', 'Containerfile')}
if g['buildinfo'].get('inputs') != inputs:
    bad.append('recorded build.sh/Containerfile hashes differ from glibc/')
ldso, libc = 'usr/lib/ld-linux-aarch64.so.1', 'usr/lib/aarch64-linux-gnu/libc.so.6'
for rel, built in ((ldso, 'ld-linux-aarch64.so.1'), (libc, 'libc.so.6')):
    data = open(os.path.join(tree, rel), 'rb').read()
    if hashlib.sha256(data).hexdigest() != sha(os.path.join(scratch, built)):
        bad.append(f'{rel}: differs from glibc/out/install')
    if b'pd2launcher Android patches' not in data:
        bad.append(f'{rel}: no pd2launcher version string')
for rel in ('lib/ld-linux-aarch64.so.1', 'usr/lib/aarch64-linux-gnu/ld-linux-aarch64.so.1'):
    if os.path.realpath(os.path.join(tree, rel)) != os.path.realpath(os.path.join(tree, ldso)):
        bad.append(f'{rel}: does not resolve to our loader')
left = []
root = os.path.join(scratch, 'libc6')
for dirpath, _, files in os.walk(root):
    for n in files:
        src = os.path.join(dirpath, n)
        rel = os.path.relpath(src, root)
        p = os.path.join(tree, rel)
        # Recorded files are ours (checked above), even where identical to Debian's (gconv-modules).
        if rel in g['files'] or os.path.islink(src) or os.path.islink(p) or not os.path.isfile(p):
            continue
        if sha(p) == sha(src):
            left.append(rel)
            bad.append(f"{rel}: still Debian's libc6 file")
for b in bad:
    print('FAIL:', b, file=sys.stderr)
print(f"glibc {g['buildinfo']['glibc_debian_version']} + {len(patches)} patches: "
      f"{len(g['files'])} recorded files match; Debian libc6 files left: {', '.join(left) or 'none'}")
sys.exit(1 if bad else 0)
EOF
    rm -rf "$scratch"
    if grep -q '^LD_LIBRARY_PATH=' "$tree/etc/userland.env"; then bad "etc/userland.env sets LD_LIBRARY_PATH with our glibc"; fi
fi

echo "archive: $(stat -c %s "$archive") bytes; regular files: $(find "$tree" -type f | wc -l); unpacked: $(du -sb "$tree" | cut -f1) bytes"

# The tree rewritten for $data as the app rewrites it (UserlandInstaller), then run from there.
python3 - "$tree" "$manifest" "$data" <<'EOF' || fail=1
import json, os, sys
tree, manifest, data = sys.argv[1:]
m = json.load(open(manifest))
P = m['placeholder'].encode()
exact = data.encode()
padded = exact + b'/' * (len(P) - len(exact))
for e in m['entries']:
    if not e.get('placeholders'):
        continue
    path = os.path.join(tree, e['path'])
    old = open(path, 'rb').read()
    new, at = bytearray(), 0
    for o in e['placeholders']:
        assert old[o:o + len(P)] == P, e['path']
        new += old[at:o] + (exact if e.get('text') else padded)
        at = o + len(P)
    new += old[at:]
    mode = os.stat(path).st_mode
    os.chmod(path, mode | 0o200)
    open(path, 'wb').write(new)
    os.chmod(path, mode)
left = []
for dirpath, _, files in os.walk(tree):
    for n in files:
        f = os.path.join(dirpath, n)
        if not os.path.islink(f) and P[:32] in open(f, 'rb').read():
            left.append(os.path.relpath(f, tree))
if left:
    sys.exit(f'FAIL: the placeholder is left in {left}')
print(f'rewritten for {data}: no placeholder left')
EOF
if [[ "${PD2_HOST_BINFMT_AARCH64:-0}" == 1 ]]; then
    bin="$data/files/userland/current/usr/bin"
    # Each: the command, then what its output must hold.
    runs=("Xvfb -help|use: X" "xkbcomp -version|xkbcomp")
    for run in "${runs[@]}"; do
        command="${run%%|*}"
        # Interpreter and libraries only at the padded path: no library environment.
        if got="$( (env -i "$bin/${command%% *}" ${command#* } 2>&1 || true) | grep -m1 -E "${run#*|}")"; then
            echo "relocated $command: $got"
        else
            bad "relocated $command: $(env -i "$bin/${command%% *}" ${command#* } 2>&1 | head -n 3)"
        fi
    done
else
    echo "relocated runs skipped: no qemu-aarch64 binfmt handler on this host"
fi
rm -rf "$data"
[[ "$fail" == 0 ]] && echo "check-tree: OK"
exit "$fail"
