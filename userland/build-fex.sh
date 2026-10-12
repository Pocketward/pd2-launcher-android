#!/usr/bin/env bash
# Installs our FEX (fex/) into the staging tree. build.sh calls it only with
# PD2_USERLAND_WITH_FEX=1, after build-glibc.sh and before relocate.sh.
#   - reuses fex/out/userland when fex/out/BUILDINFO names the tag and commit in fex/build.sh,
#     thunks=ON, the prefix, and the SHA-256 of fex/build.sh, fex/Containerfile,
#     fex/toolchain-i686-guest.cmake and every fex/patches/*.patch; otherwise runs
#     fex/build.sh --thunks (the debug variant, config.sh: fex/out-debug/, for the development
#     build's prefix)
#   - installs usr/bin/FEX* , the host thunks (usr/lib/fex-emu/HostThunks{,_32}/), the guest thunks
#     (usr/share/fex-emu/GuestThunks{,_32}/), ThunksDB.json and AppConfig/ as built (already
#     stripped or without debug info; interpreter already $USERLAND/lib/ld-linux-aarch64.so.1)
#   - left out: the CUDA thunks (no CUDA on Android; the host thunk would need libcuda.so.1),
#     libFEXCore.so and usr/include (no installed program links libFEXCore.so: FEXCore is linked
#     statically), usr/lib/binfmt.d (no binfmt_misc for apps), usr/share/man
#   - writes the cache's work/fex.json (BUILDINFO fields, patch SHA-256s, SHA-256 of every
#     installed file), which pack.sh puts into manifest.json and check-tree.sh verifies
set -euo pipefail
export LC_ALL=C

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=config.sh
. "$here/config.sh"

# --- on the host: build or reuse, then install in the tools container --------------------------
if [[ "${PD2_USERLAND_IN_CONTAINER:-}" != 1 ]]; then
    f="$here/fex"
    out="$f/out$VARIANT_SUFFIX"
    expected() {
        echo "fex_tag=$(sed -n 's/^FEX_TAG=//p' "$f/build.sh")"
        echo "fex_commit=$(sed -n 's/^FEX_COMMIT=//p' "$f/build.sh")"
        echo "thunks=ON"
        echo "prefix=$USERLAND"
        for i in build.sh Containerfile toolchain-i686-guest.cmake; do
            echo "input=$i $(sha256sum < "$f/$i" | cut -d' ' -f1)"
        done
        for p in "$f"/patches/*.patch; do echo "patch=$(basename "$p") $(sha256sum < "$p" | cut -d' ' -f1)"; done
    }
    recorded() { grep -E '^(fex_tag|fex_commit|thunks|prefix|input|patch)=' "$out/BUILDINFO" 2>/dev/null || true; }
    if [[ -f "$out/userland/usr/bin/FEX" && "$(recorded)" == "$(expected)" ]]; then
        echo "reusing ${out#"$here/"}/userland (BUILDINFO matches the prefix, the patches and build files)"
    else
        echo "building FEX (fex/build.sh --thunks)"
        "$f/build.sh" --thunks
        [[ "$(recorded)" == "$(expected)" ]] || { echo "${out#"$here/"}/BUILDINFO does not match the inputs" >&2; exit 1; }
    fi
    run_in_tools build-fex.sh "$@"
    exit
fi

# --- inside the tools container ----------------------------------------------------------------
inst=/src/fex/out$VARIANT_SUFFIX/userland
root=/cache/work/staging
record=/cache/work/fex.json
[[ -d "$root/usr" ]] || { echo "no staging tree: run fetch-debian.sh first" >&2; exit 1; }
[[ -f "$inst/usr/bin/FEX" ]] || { echo "no FEX install in ${inst#/src/}" >&2; exit 1; }
rm -f "$record"
rm -rf "$root/usr/lib/fex-emu" "$root/usr/share/fex-emu"

mapfile -t installed < <(
    cd "$inst"
    find usr/bin -type f -name 'FEX*'
    find usr/lib/fex-emu usr/share/fex-emu -type f ! -name 'libcuda-*'
)
[[ "$(cd "$inst" && find . -type l | wc -l)" == 0 ]] || { echo "unexpected symlinks in ${inst#/src/}" >&2; exit 1; }
for rel in "${installed[@]}"; do
    mkdir -p "$(dirname "$root/$rel")"
    rm -f "$root/$rel"
    cp --preserve=mode "$inst/$rel" "$root/$rel"
done

python3 - "$root" "/src/fex/out$VARIANT_SUFFIX/BUILDINFO" "$record" "${installed[@]}" <<'EOF'
import hashlib, json, sys
root, buildinfo, record, *paths = sys.argv[1:]
info, patches, inputs = {}, {}, {}
for line in open(buildinfo):
    key, _, value = line.rstrip('\n').partition('=')
    if key in ('patch', 'input'):
        name, sha = value.split()
        (patches if key == 'patch' else inputs)[name] = sha
    elif key:
        info[key] = value
info['patches'] = patches
info['inputs'] = inputs
files = {p: hashlib.sha256(open(f'{root}/{p}', 'rb').read()).hexdigest() for p in sorted(paths)}
json.dump({'buildinfo': info, 'files': files}, open(record, 'w'), indent=1, sort_keys=True)
print(f"FEX {info.get('fex_tag')} + {len(patches)} patches, thunks={info.get('thunks')}: {len(files)} files installed")
EOF
