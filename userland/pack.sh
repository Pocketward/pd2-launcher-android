#!/usr/bin/env bash
# Packs the relocated staging tree into out/userland.tar.zst plus out/manifest.json and copies
# both into the APK's assets (../app/src/main/assets/userland/, gitignored); for the debug variant
# (PD2_USERLAND_VARIANT=debug, config.sh) out-debug/ and ../app/src/debug/assets/userland/.
#   archive   GNU tar, entries sorted by name, mtime 0, owner/group 0, hard links dereferenced
#             (--hard-dereference), compressed with zstd (pinned in the tools image), level 19
#   manifest  {"version", "prefix", "archive": {name, size, sha256}, "entries": [{path, type,
#             mode, size, sha256 | target}]}, entries sorted by path
#   version   the first 16 hex digits of a SHA-256 over this directory's inputs (scripts, pins,
#             package lock, patches, our PanVK backend's files in mesa-kbase/, opt-in switches, and
#             with glibc or FEX their patches and build files) and the app's data folder, so equal
#             inputs give an equal version and the two variants never share one
#   glibc     with PD2_USERLAND_WITH_GLIBC=1, the manifest's "glibc" key is build-glibc.sh's record
#             (BUILDINFO fields, patch SHA-256s, SHA-256 of every installed file)
#   fex       with PD2_USERLAND_WITH_FEX=1, the manifest's "fex" key is build-fex.sh's record (same
#             shape)
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=config.sh
. "$here/config.sh"

if [[ "${PD2_USERLAND_IN_CONTAINER:-}" != 1 ]]; then
    mkdir -p "$USERLAND_OUT"
    RUN_EXTRA=(-v "$USERLAND_OUT:/out" -e "PD2_USERLAND_WITH_GLIBC=${PD2_USERLAND_WITH_GLIBC:-0}"
        -e "PD2_USERLAND_WITH_FEX=${PD2_USERLAND_WITH_FEX:-0}")
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
        find . -maxdepth 1 -type f \( -name '*.sh' -o -name '*.in' -o -name '*.c' -o -name 'packages.*' \)
        find ./containers ./patches ./mesa-kbase -type f
        if [[ "${PD2_USERLAND_WITH_GLIBC:-0}" == 1 ]]; then
            find ./glibc/patches ./glibc/build.sh ./glibc/Containerfile -type f
        fi
        if [[ "${PD2_USERLAND_WITH_FEX:-0}" == 1 ]]; then
            find ./fex/patches ./fex/build.sh ./fex/Containerfile ./fex/toolchain-i686-guest.cmake -type f
        fi
    } | LC_ALL=C sort | xargs sha256sum
    echo "glibc=${PD2_USERLAND_WITH_GLIBC:-0} fex=${PD2_USERLAND_WITH_FEX:-0}"
    echo "data=$DATA_DIR"
)"
version="$(sha256sum <<<"$version" | cut -c1-16)"

glibc_record=""
if [[ "${PD2_USERLAND_WITH_GLIBC:-0}" == 1 ]]; then
    glibc_record=/cache/work/glibc.json
    [[ -f "$glibc_record" ]] || { echo "no glibc record: run build-glibc.sh first" >&2; exit 1; }
fi
fex_record=""
if [[ "${PD2_USERLAND_WITH_FEX:-0}" == 1 ]]; then
    fex_record=/cache/work/fex.json
    [[ -f "$fex_record" ]] || { echo "no FEX record: run build-fex.sh first" >&2; exit 1; }
fi

rm -f "$out/userland.tar.zst" "$out/manifest.json"
mapfile -t top < <(cd "$root" && ls -A | LC_ALL=C sort)
tar --create --file=- --directory="$root" --format=gnu --sort=name \
    --mtime=@0 --owner=0 --group=0 --numeric-owner --hard-dereference "${top[@]}" \
    | zstd -19 -T0 -q -o "$out/userland.tar.zst"

python3 - "$root" "$out" "$version" "$USERLAND" "$glibc_record" "$fex_record" <<'EOF'
import hashlib, json, os, stat, sys
root, out, version, prefix, glibc_record, fex_record = sys.argv[1:]

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
            e.update(type='symlink', target=os.readlink(full))
        elif stat.S_ISDIR(st.st_mode):
            e.update(type='dir')
        elif stat.S_ISREG(st.st_mode):
            e.update(type='file', size=st.st_size, sha256=sha256(full))
        else:
            sys.exit('unsupported file type: ' + rel)
        entries.append(e)
entries.sort(key=lambda e: e['path'].encode())
archive = os.path.join(out, 'userland.tar.zst')
manifest = {
    'version': version,
    'prefix': prefix,
    'archive': {'name': 'userland.tar.zst', 'size': os.path.getsize(archive), 'sha256': sha256(archive)},
    'entries': entries,
}
if glibc_record:
    manifest['glibc'] = json.load(open(glibc_record))
if fex_record:
    manifest['fex'] = json.load(open(fex_record))
with open(os.path.join(out, 'manifest.json'), 'w') as f:
    json.dump(manifest, f, indent=1, sort_keys=True)
    f.write('\n')
files = [e for e in entries if e['type'] == 'file']
print(f"version {version}: {len(files)} files, "
      f"{sum(e['type'] == 'symlink' for e in entries)} symlinks, "
      f"{sum(e['type'] == 'dir' for e in entries)} dirs, "
      f"{sum(e['size'] for e in files) / 1048576:.1f} MiB unpacked, "
      f"archive {manifest['archive']['size'] / 1048576:.1f} MiB")
EOF
