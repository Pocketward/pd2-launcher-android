#!/usr/bin/env bash
# Packs the relocated staging tree into out/userland.tar.zst plus out/manifest.json and copies
# both into the APK's assets (../app/src/main/assets/userland/, gitignored).
#   archive   GNU tar, entries sorted by name, mtime 0, owner/group 0, hard links dereferenced
#             (--hard-dereference), compressed with zstd (pinned in the tools image), level 19
#   manifest  {"version", "placeholder", "prefix", "archive": {name, size, sha256}, "entries":
#             [{path, type, mode, size, sha256 | target}], "critical", "absent", "engine_needed"},
#             entries sorted by path
#   placeholder  config.sh's placeholder data folder, which the app rewrites to its own; a file
#             that holds it lists the byte offset of each copy ("placeholders"), and "text": true
#             when it is rewritten as text (not an ELF and no NUL byte: the real path exactly)
#             rather than in place (the real path padded with '/' to the placeholder's length).
#             A partial copy of the placeholder anywhere is refused.
#   critical  the paths the app checks again before every engine start, as programs open them:
#             the loader, etc/userland.env, etc/nsswitch.conf, the CA bundle, what userland.env
#             names as LD_PRELOAD, and every library the engine (ENGINE_NEEDED in config.sh) and
#             those need, directly or through another library, as the loader finds it
#   absent    paths that must not exist: glibc reads etc/ld.so.cache from its sysconfdir
#   version   the first 16 hex digits of a SHA-256 over this directory's inputs (scripts except
#             mesa-update.sh, pins, package lock, patches, our PanVK backend's files in mesa-kbase/,
#             the opt-in switch, and with glibc its patches and build files), so equal inputs give
#             an equal version
#   glibc     with PD2_USERLAND_WITH_GLIBC=1, the manifest's "glibc" key is build-glibc.sh's record
#             (BUILDINFO fields, patch SHA-256s, SHA-256 of every installed file)
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=config.sh
. "$here/config.sh"

if [[ "${PD2_USERLAND_IN_CONTAINER:-}" != 1 ]]; then
    mkdir -p "$USERLAND_OUT"
    RUN_EXTRA=(-v "$USERLAND_OUT:/out" -e "PD2_USERLAND_WITH_GLIBC=${PD2_USERLAND_WITH_GLIBC:-0}")
    run_in_tools pack.sh "$@"
    mkdir -p "$USERLAND_ASSETS"
    cp "$USERLAND_OUT/userland.tar.zst" "$USERLAND_OUT/manifest.json" "$USERLAND_ASSETS/"
    echo "copied to $USERLAND_ASSETS"
    exit
fi

root=/cache/work/staging
out=/out
[[ -f "$root/etc/userland.env" ]] || { echo "tree not relocated: run relocate.sh first" >&2; exit 1; }

version="$(
    cd /src
    {
        # mesa-update.sh only moves the pin (config.sh and the patches, which count); it builds nothing.
        find . -maxdepth 1 -type f \( -name '*.sh' -o -name '*.in' -o -name '*.c' -o -name 'packages.*' \) \
            ! -name mesa-update.sh
        find ./containers ./patches ./mesa-kbase -type f
        if [[ "${PD2_USERLAND_WITH_GLIBC:-0}" == 1 ]]; then
            find ./glibc/patches ./glibc/build.sh ./glibc/Containerfile -type f
        fi
    } | LC_ALL=C sort | xargs sha256sum
    echo "glibc=${PD2_USERLAND_WITH_GLIBC:-0}"
)"
version="$(sha256sum <<<"$version" | cut -c1-16)"

glibc_record=""
if [[ "${PD2_USERLAND_WITH_GLIBC:-0}" == 1 ]]; then
    glibc_record=/cache/work/glibc.json
    [[ -f "$glibc_record" ]] || { echo "no glibc record: run build-glibc.sh first" >&2; exit 1; }
fi

rm -f "$out/userland.tar.zst" "$out/manifest.json"
mapfile -t top < <(cd "$root" && ls -A | LC_ALL=C sort)
tar --create --file=- --directory="$root" --format=gnu --sort=name \
    --mtime=@0 --owner=0 --group=0 --numeric-owner --hard-dereference "${top[@]}" \
    | zstd -19 -T0 -q -o "$out/userland.tar.zst"

python3 - "$root" "$out" "$version" "$DATA_PLACEHOLDER" "$USERLAND" "$glibc_record" \
    "${ENGINE_NEEDED[*]}" <<'EOF'
import collections, hashlib, json, os, re, stat, subprocess, sys
root, out, version, placeholder, prefix, glibc_record, engine_needed = sys.argv[1:]
P = placeholder.encode()
# The placeholder's first part: a copy of it that is not the whole placeholder would be left
# half rewritten.
head = P[:32]
assert prefix.startswith(placeholder + '/') and len(P) == 200 and b'//' not in P

def sha256(path):
    h = hashlib.sha256()
    with open(path, 'rb') as f:
        for chunk in iter(lambda: f.read(1 << 20), b''):
            h.update(chunk)
    return h.hexdigest()

entries = []
for dirpath, dirnames, filenames in os.walk(root):
    for name in dirnames + filenames:
        full = os.path.join(dirpath, name)
        rel = os.path.relpath(full, root)
        st = os.lstat(full)
        e = {'path': rel, 'mode': format(stat.S_IMODE(st.st_mode), '04o')}
        if stat.S_ISLNK(st.st_mode):
            target = os.readlink(full)
            if head in target.encode():
                sys.exit(f'{rel}: a symlink to the placeholder ({target})')
            e.update(type='symlink', target=target)
        elif stat.S_ISDIR(st.st_mode):
            e.update(type='dir')
        elif stat.S_ISREG(st.st_mode):
            data = open(full, 'rb').read()
            e.update(type='file', size=st.st_size, sha256=hashlib.sha256(data).hexdigest())
            offsets = []
            i = data.find(P)
            while i >= 0:
                offsets.append(i)
                i = data.find(P, i + len(P))
            if data.count(head) != len(offsets):
                sys.exit(f'{rel}: {data.count(head) - len(offsets)} partial copies of the placeholder')
            if offsets:
                e['placeholders'] = offsets
                if data[:4] != b'\x7fELF' and b'\0' not in data:
                    e['text'] = True
        else:
            sys.exit('unsupported file type: ' + rel)
        entries.append(e)
entries.sort(key=lambda e: e['path'].encode())
by_path = {e['path']: e for e in entries}

# --- the critical set -------------------------------------------------------------------------
def inside(path):
    """path's real file relative to the tree, links followed; refused if it leaves the tree."""
    real = os.path.realpath(os.path.join(root, path))
    if not real.startswith(root + '/'):
        sys.exit(f'{path} leads out of the tree')
    return os.path.relpath(real, root)

def needed(path):
    dyn = subprocess.run(['aarch64-linux-gnu-readelf', '-d', os.path.join(root, path)],
                         capture_output=True, text=True, check=True).stdout
    # With a run path the loader would search more folders than the ones below.
    if re.search(r'\((RPATH|RUNPATH)\)', dyn):
        sys.exit(f'{path}: has a run path')
    return re.findall(r'\(NEEDED\)\s+Shared library: \[(.+?)\]', dyn)

env = {}
for line in open(os.path.join(root, 'etc/userland.env')):
    m = re.match(r'^([A-Za-z_][A-Za-z0-9_]*)=(.*)$', line.strip())
    if m:
        env[m.group(1)] = m.group(2)
in_tree = lambda value: [v[len(prefix) + 1:] for v in re.split('[ :]', value) if v.startswith(prefix + '/')]
# Where the loader looks for a library: LD_LIBRARY_PATH (Debian's glibc only), then the default
# folders of ours (slibdir, libdir).
search = in_tree(env.get('LD_LIBRARY_PATH', '')) + ['lib/aarch64-linux-gnu', 'usr/lib/aarch64-linux-gnu']
loader = 'lib/ld-linux-aarch64.so.1'

def find(soname):
    for d in search:
        if os.path.isfile(os.path.join(root, d, soname)):
            return f'{d}/{soname}'
    sys.exit(f'{soname} is not in the tree')

critical = [loader, 'etc/userland.env', 'etc/nsswitch.conf', 'etc/ssl/certs/ca-certificates.crt']
programs = in_tree(env.get('LD_PRELOAD', ''))
queue = collections.deque(engine_needed.split())
for p in dict.fromkeys(programs):
    critical.append(p)
    queue.extend(needed(inside(p)))
seen = set()
while queue:
    soname = queue.popleft()
    # The loader itself satisfies a NEEDED of its own name.
    if soname in seen or soname == 'ld-linux-aarch64.so.1':
        continue
    seen.add(soname)
    path = find(soname)
    critical.append(path)
    queue.extend(needed(inside(path)))
critical = list(dict.fromkeys(critical))
absent = ['etc/ld.so.cache']
for path in critical:
    if inside(path) not in by_path or by_path[inside(path)]['type'] != 'file':
        sys.exit(f'critical {path} is not a file of the tree')
for path in absent:
    if path in by_path:
        sys.exit(f'{path} is in the tree')

archive = os.path.join(out, 'userland.tar.zst')
manifest = {
    'version': version,
    'placeholder': placeholder,
    'prefix': prefix,
    'archive': {'name': 'userland.tar.zst', 'size': os.path.getsize(archive), 'sha256': sha256(archive)},
    'entries': entries,
    'critical': critical,
    'absent': absent,
    'engine_needed': engine_needed.split(),
}
if glibc_record:
    manifest['glibc'] = json.load(open(glibc_record))
with open(os.path.join(out, 'manifest.json'), 'w') as f:
    json.dump(manifest, f, indent=1, sort_keys=True)
    f.write('\n')
files = [e for e in entries if e['type'] == 'file']
relocated = [e for e in files if 'placeholders' in e]
print(f"version {version}: {len(files)} files, "
      f"{sum(e['type'] == 'symlink' for e in entries)} symlinks, "
      f"{sum(e['type'] == 'dir' for e in entries)} dirs, "
      f"{sum(e['size'] for e in files) / 1048576:.1f} MiB unpacked, "
      f"archive {manifest['archive']['size'] / 1048576:.1f} MiB; "
      f"the placeholder in {len(relocated)} files ({sum(e.get('text', False) for e in relocated)} text), "
      f"{sum(len(e['placeholders']) for e in relocated)} times; {len(critical)} critical paths")
EOF
