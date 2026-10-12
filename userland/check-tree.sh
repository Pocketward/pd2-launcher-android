#!/usr/bin/env bash
# Checks the packed App userland as the device will see it: unpacks out/userland.tar.zst (out-debug/
# for the debug variant, config.sh; glibc and FEX likewise from their out-debug/) into a
# scratch directory and verifies
#   - no hard-link entries in the archive and no hard links in the unpacked tree
#   - no absolute symlinks
#   - every ELF with a program interpreter requests $USERLAND/lib/ld-linux-aarch64.so.1 (readelf -l),
#     and that loader is in the tree
#   - the Vulkan ICD JSON's library_path is the absolute $USERLAND path of a file in the tree
#   - usr/bin/x-input-relay is there and every library it needs is in the tree
#   - PanVK (build-panvk.sh) is there: its ICD JSON and libvulkan_panfrost.so, every library it
#     needs in the tree
#   - the manifest matches the tree (every entry, type, mode, size, SHA-256)
#   - when the manifest has a "glibc" record (PD2_USERLAND_WITH_GLIBC=1): every recorded file is in
#     the tree with its SHA-256; the record's patch hashes are those of glibc/patches; ld.so and
#     libc.so.6 equal glibc/out/install's copies stripped as build-glibc.sh strips them and carry
#     our version string; both loader paths resolve to our loader; no regular file of Debian's
#     libc6 .deb is left in the tree (files in the record are ours, even where byte-identical to
#     Debian's, such as gconv-modules)
#     and etc/userland.env does not set LD_LIBRARY_PATH
#   - when the manifest has a "fex" record (PD2_USERLAND_WITH_FEX=1): every recorded file is in the
#     tree with its SHA-256 and equals fex/out/userland's copy; the record's patch, build.sh and
#     Containerfile hashes are those in fex/; thunks were built; FEX and FEXServer carry FEX-2609.1;
#     every NEEDED library of the aarch64 FEX files and each library a host thunk dlopen()s is in
#     lib/aarch64-linux-gnu or usr/lib/aarch64-linux-gnu (ld.so's default path), and none has a
#     run path
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=config.sh
. "$here/config.sh"

if [[ "${PD2_USERLAND_IN_CONTAINER:-}" != 1 ]]; then
    RUN_EXTRA=(-v "$USERLAND_OUT:/out:ro")
    run_in_tools check-tree.sh "$@"
    exit
fi

archive=/out/userland.tar.zst
tree=/cache/work/check
rm -rf "$tree"
mkdir -p "$tree"
fail=0
bad() { echo "FAIL: $*" >&2; fail=1; }

hardlinks="$(zstd -dc "$archive" | tar -tv | grep -c '^h' || true)"
[[ "$hardlinks" == 0 ]] || bad "$hardlinks hard-link entries in the archive"
zstd -dc "$archive" | tar -x -p -C "$tree"
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

python3 - "$tree" /out/manifest.json <<'EOF' || fail=1
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

if jq -e .glibc /out/manifest.json >/dev/null; then
    scratch=/cache/work/check-glibc
    rm -rf "$scratch"
    mkdir -p "$scratch/libc6"
    for p in lib/ld-linux-aarch64.so.1 lib/aarch64-linux-gnu/libc.so.6; do
        cp "/src/glibc/out$VARIANT_SUFFIX/install/$p" "$scratch/$(basename "$p")"
        aarch64-linux-gnu-strip --strip-debug "$scratch/$(basename "$p")"
    done
    deb="/cache/debs/$(basename "$(awk '$1 == "runtime" && $2 == "libc6" {print $7}' /src/packages.lock)")"
    dpkg-deb --fsys-tarfile "$deb" | tar -x -C "$scratch/libc6"
    python3 - "$tree" /out/manifest.json "$scratch" /src/glibc/patches <<'EOF' || fail=1
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

if jq -e .fex /out/manifest.json >/dev/null; then
    python3 - "$tree" /out/manifest.json /src/fex "/src/fex/out$VARIANT_SUFFIX/userland" <<'EOF' || fail=1
import hashlib, json, os, re, subprocess, sys
tree, manifest, fexdir, fexout = sys.argv[1:]
x = json.load(open(manifest))['fex']
sha = lambda p: hashlib.sha256(open(p, 'rb').read()).hexdigest()
bad = []
for rel, want in x['files'].items():
    p = os.path.join(tree, rel)
    if os.path.islink(p) or not os.path.isfile(p) or sha(p) != want:
        bad.append(f'{rel}: not the recorded file')
    elif sha(os.path.join(fexout, rel)) != want:
        bad.append(f'{rel}: differs from fex/out/userland')
pdir = os.path.join(fexdir, 'patches')
patches = {n: sha(os.path.join(pdir, n)) for n in sorted(os.listdir(pdir)) if n.endswith('.patch')}
if x['buildinfo']['patches'] != patches:
    bad.append('recorded patch hashes differ from fex/patches')
inputs = {n: sha(os.path.join(fexdir, n)) for n in ('build.sh', 'Containerfile', 'toolchain-i686-guest.cmake')}
if x['buildinfo']['inputs'] != inputs:
    bad.append('recorded build.sh/Containerfile/toolchain hashes differ from fex/')
if x['buildinfo'].get('thunks') != 'ON':
    bad.append('FEX built without thunks')
for exe in ('usr/bin/FEX', 'usr/bin/FEXServer'):
    if exe not in x['files'] or b'FEX-2609.1' not in open(os.path.join(tree, exe), 'rb').read():
        bad.append(f'{exe}: missing or no FEX-2609.1 version string')
# Every aarch64 ELF's DT_NEEDED, and each host thunk's dlopen()ed library (lib*.so.N strings),
# must be in ld.so's default directories (our glibc: slibdir and libdir, no LD_LIBRARY_PATH).
search = ['lib/aarch64-linux-gnu', 'usr/lib/aarch64-linux-gnu']
found = lambda lib: any(os.path.exists(os.path.join(tree, d, lib)) for d in search)
checked = 0
for rel in x['files']:
    data = open(os.path.join(tree, rel), 'rb').read()
    if data[:4] != b'\x7fELF' or int.from_bytes(data[18:20], 'little') != 183:  # EM_AARCH64
        continue
    dyn = subprocess.run(['aarch64-linux-gnu-readelf', '-d', os.path.join(tree, rel)],
                         capture_output=True, text=True, check=True).stdout
    libs = set(re.findall(r'\(NEEDED\)\s+Shared library: \[(.+?)\]', dyn))
    if '/HostThunks' in rel:
        libs |= {m.decode() for m in re.findall(rb'lib[A-Za-z0-9_+-]+\.so\.[0-9]+', data)}
    for lib in sorted(libs):
        checked += 1
        if not found(lib):
            bad.append(f'{rel}: {lib} not found in {" or ".join(search)}')
    if re.search(r'\((RPATH|RUNPATH)\)', dyn):
        bad.append(f'{rel}: has a run path')
for b in bad:
    print('FAIL:', b, file=sys.stderr)
b = x['buildinfo']
print(f"FEX {b['fex_tag']} ({b['fex_commit'][:12]}) + {len(patches)} patches, thunks={b['thunks']}: "
      f"{len(x['files'])} recorded files match fex/out/userland; {checked} needed/dlopen libraries resolve")
sys.exit(1 if bad else 0)
EOF
fi

echo "archive: $(stat -c %s "$archive") bytes; regular files: $(find "$tree" -type f | wc -l); unpacked: $(du -sb "$tree" | cut -f1) bytes"
rm -rf "$tree"
[[ "$fail" == 0 ]] && echo "check-tree: OK"
exit "$fail"
