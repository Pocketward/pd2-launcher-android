#!/usr/bin/env bash
# Installs our glibc (glibc/) into the staging tree in place of Debian's
# libc6. build.sh calls it only with PD2_USERLAND_WITH_GLIBC=1, after fetch-debian.sh and before
# relocate.sh.
#   - reuses glibc/out/install when glibc/out/BUILDINFO names the current Debian version, the prefix
#     and the SHA-256 of every glibc/patches/*.patch; otherwise runs glibc/build.sh (the debug
#     variant, config.sh: glibc/out-debug/, for the development build's prefix)
#   - the loader becomes the real file usr/lib/ld-linux-aarch64.so.1 (= <U>/lib, glibc's rtlddir,
#     the interpreter relocate.sh sets); Debian's usr/lib/aarch64-linux-gnu/ld-linux-aarch64.so.1
#     becomes a symlink to it
#   - the shared libraries (libc.so.6, libm, libpthread, libdl, librt, libresolv, libnss_*, ...)
#     and gconv/ replace Debian's in usr/lib/aarch64-linux-gnu (slibdir = libdir after the /usr
#     merge); etc/rpc is added (sysconfdir <U>/etc); Debian's etc/ld.so.conf.d entry is dropped
#   - debug info is stripped (strip --strip-debug), as Debian ships libc6
#   - writes the cache's work/glibc.json (BUILDINFO fields, patch SHA-256s, SHA-256 of every
#     installed file), which pack.sh puts into manifest.json and check-tree.sh verifies
# Headers, static libraries, programs (libc-bin) and locale data are not installed.
set -euo pipefail
export LC_ALL=C

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=config.sh
. "$here/config.sh"

# --- on the host: build or reuse, then install in the tools container --------------------------
if [[ "${PD2_USERLAND_IN_CONTAINER:-}" != 1 ]]; then
    g="$here/glibc"
    out="$g/out$VARIANT_SUFFIX"
    expected() {
        echo "glibc_debian_version=$(sed -n 's/^GLIBC_DEB_VERSION=//p' "$g/build.sh")"
        echo "prefix=$USERLAND"
        for p in "$g"/patches/*.patch; do echo "patch=$(basename "$p") $(sha256sum < "$p" | cut -d' ' -f1)"; done
    }
    recorded() { grep -E '^(glibc_debian_version|prefix|patch)=' "$out/BUILDINFO" 2>/dev/null || true; }
    if [[ -f "$out/install/lib/ld-linux-aarch64.so.1" && "$(recorded)" == "$(expected)" ]]; then
        echo "reusing ${out#"$here/"}/install (BUILDINFO matches the prefix and the patches)"
    else
        echo "building glibc (glibc/build.sh)"
        "$g/build.sh"
        [[ "$(recorded)" == "$(expected)" ]] || { echo "${out#"$here/"}/BUILDINFO does not match the prefix and the patches" >&2; exit 1; }
    fi
    run_in_tools build-glibc.sh "$@"
    exit
fi

# --- inside the tools container ----------------------------------------------------------------
inst=/src/glibc/out$VARIANT_SUFFIX/install
root=/cache/work/staging
ma="$root/usr/lib/aarch64-linux-gnu"
record=/cache/work/glibc.json
[[ -d "$ma" ]] || { echo "no staging tree: run fetch-debian.sh first" >&2; exit 1; }
[[ -f "$inst/lib/ld-linux-aarch64.so.1" ]] || { echo "no glibc install in ${inst#/src/}" >&2; exit 1; }
rm -f "$record"

installed=()
put() { # source, path relative to the staging root
    rm -rf "${root:?}/$2"
    cp --preserve=mode "$1" "$root/$2"
    if [[ "$(head -c4 "$root/$2" | od -An -tx1 | tr -d ' \n')" == 7f454c46 ]]; then
        aarch64-linux-gnu-strip --strip-debug "$root/$2"
    fi
    installed+=("$2")
}

# Loader at glibc's rtlddir; Debian's copy in the multiarch directory becomes a link to ours.
put "$inst/lib/ld-linux-aarch64.so.1" usr/lib/ld-linux-aarch64.so.1
rm -f "$ma/ld-linux-aarch64.so.1"
ln -s ../ld-linux-aarch64.so.1 "$ma/ld-linux-aarch64.so.1"

for f in "$inst"/lib/aarch64-linux-gnu/*; do
    put "$f" "usr/lib/aarch64-linux-gnu/$(basename "$f")"
done

# gconv modules: ours only (no gconv-modules.cache: iconvconfig is not run when cross-compiling;
# glibc then reads gconv-modules and gconv-modules.d/ directly).
rm -rf "$ma/gconv"
while IFS= read -r -d '' f; do
    rel="usr/lib/aarch64-linux-gnu/gconv/${f#"$inst/usr/lib/aarch64-linux-gnu/gconv/"}"
    mkdir -p "$(dirname "$root/$rel")"
    put "$f" "$rel"
done < <(find "$inst/usr/lib/aarch64-linux-gnu/gconv" -type f -print0 | sort -z)

put "$inst/etc/rpc" etc/rpc
# Read only by ldconfig, which does not run here; it names the device's /usr/lib paths.
rm -f "$root/etc/ld.so.conf.d/aarch64-linux-gnu.conf"
rmdir --ignore-fail-on-non-empty "$root/etc/ld.so.conf.d" 2>/dev/null || true

python3 - "$root" "/src/glibc/out$VARIANT_SUFFIX/BUILDINFO" "$record" "${installed[@]}" <<'EOF'
import hashlib, json, sys
root, buildinfo, record, *paths = sys.argv[1:]
info, patches = {}, {}
for line in open(buildinfo):
    key, _, value = line.rstrip('\n').partition('=')
    if key == 'patch':
        name, sha = value.split()
        patches[name] = sha
    elif key:
        info[key] = value
info['patches'] = patches
files = {p: hashlib.sha256(open(f'{root}/{p}', 'rb').read()).hexdigest() for p in sorted(paths)}
json.dump({'buildinfo': info, 'files': files}, open(record, 'w'), indent=1, sort_keys=True)
print(f"glibc {info.get('glibc_debian_version')} + {len(patches)} patches: {len(files)} files installed")
EOF
