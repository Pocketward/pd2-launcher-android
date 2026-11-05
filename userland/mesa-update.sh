#!/usr/bin/env bash
# Moves the Mesa pin (config.sh MESA_*: a commit of Mesa's development branch, main) to a newer
# commit and says whether our patches still apply. It writes nothing unless every patch applies.
#
#   mesa-update.sh [--commit <sha>] [--write]
#
#   --commit  the commit to try (default: the newest commit of main, from git ls-remote)
#   --write   when every patch applies with --fuzz=0, write the pin into config.sh (MESA_COMMIT,
#             MESA_SHA256, MESA_VERSION from the tree's VERSION file) and refresh every patch for
#             the new tree (the same changes at the new line numbers; each patch's description,
#             the lines before its first "--- a/", is kept). All refreshed patches and the new
#             config.sh are made and checked first, and written only when every one was made.
#
# Steps: download GitLab's archive of the commit into the scratch folder (with --write it then
# goes into the cache's sources/, where build-mesa.sh's fetch_source takes it, and its SHA-256
# becomes MESA_SHA256; for the current pin the checked archive in sources/ is used, and one whose
# SHA-256 is not MESA_SHA256 is refused), unpack it into a scratch tree, then apply in build
# order patches/mesa/*.patch (build-mesa.sh's series; build-panvk.sh applies its *-wsi-* part),
# patches/mesa-panvk/*.patch (build-panvk.sh's, after the WSI series) and
# patches/mesa-sysdrv/*.patch (build-sysdrv.sh's Venus build, after the whole patches/mesa/
# series), each with --fuzz=0 as the builds do. Per patch it prints "exact", "offsets" or "FAILS" (with the failed hunks, and whether
# fuzz would let it apply: a sign that only context moved). A failed patch is applied as far as
# it goes, so the scratch tree keeps its .rej files for a rebase by hand.
# For build-sysdrv.sh it also checks that the new tree's subprojects/venus-protocol.wrap names
# config.sh's VENUS_PROTOCOL_VERSION, and that the patches of patches/mesa/ that the older Mesa of
# the system-driver mode's GL stack also takes (MESA_SYSDRV_GL_SHARED_PATCHES) still apply to it
# with --fuzz=0, before and after their refresh (its archive from the cache's sources/, downloaded
# and checked against MESA_SYSDRV_SHA256 when missing); nothing is written otherwise.
# One tree serves the three builds' orders only while the patches of patches/mesa/ that are not
# *-wsi-* change no file that a WSI or PanVK patch changes, and the Venus patches no file that a
# PanVK patch changes; the script refuses otherwise, and it refuses a patch that adds or deletes a
# file (the refresh handles changed files only). config.sh's VENUS_PROTOCOL_* pin must match the
# new tree's subprojects/venus-protocol.wrap; build-sysdrv.sh checks that.
# Exit 0: every patch applies (with --write: pin and patches written); 1: a patch fails or is
# refused, or a step fails (download, unpack, refresh), nothing written; 2: wrong arguments.
#
# A failed patch is rebased by hand (on the scratch tree or a git clone at that commit), written
# into patches/, and this script run again with the same --commit until every patch applies (a
# run for another commit removes the scratch tree). After --write: build the
# userland (build.sh) and the app, and run the gate tests (docs/BUILDING.md, "Moving the Mesa
# pin") before the new pin goes into a release.
#
# Runs on the host (git ls-remote, curl, tar, gzip, patch, diff, sha256sum), not in a container:
# it builds nothing. The scratch folder is $USERLAND_CACHE/mesa-update: the tree <commit> stays
# for inspection; a later run for the same commit replaces it, and a run for another commit
# removes it.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=config.sh
. "$here/config.sh"

repo=https://gitlab.freedesktop.org/mesa/mesa
commit="" write=0
while [[ $# -gt 0 ]]; do
    case "$1" in
        --commit)
            [[ $# -ge 2 ]] || { echo "--commit needs a commit id" >&2; exit 2; }
            commit="$2"; shift 2 ;;
        --write) write=1; shift ;;
        *) echo "usage: mesa-update.sh [--commit <sha>] [--write]" >&2; exit 2 ;;
    esac
done
if [[ -z "$commit" ]]; then
    commit="$(git ls-remote "$repo.git" refs/heads/main | cut -f1)" || true
    [[ -n "$commit" ]] || { echo "could not read main's newest commit from $repo" >&2; exit 1; }
fi
[[ "$commit" =~ ^[0-9a-f]{40}$ ]] || { echo "not a full commit id: '$commit'" >&2; exit 2; }
# From here on a failed step exits 1, not with the failed command's own status (which can be 2).
trap 'echo "a step failed (line $LINENO): nothing written" >&2; exit 1' ERR
echo "current pin: $MESA_COMMIT ($MESA_VERSION)"
echo "trying:      $commit"

# The files a patch changes, as its "+++ b/" lines name them, each once.
patch_files() { sed -n 's|^+++ b/\([^[:space:]]*\).*|\1|p' "$1" | awk '!seen[$0]++'; }

# Build order: the Turnip series, then PanVK's, then Venus's (only the folders' own files, as the
# builds take).
mapfile -t series < <(find "$here/patches/mesa" -maxdepth 1 -name '*.patch' | LC_ALL=C sort
    find "$here/patches/mesa-panvk" -maxdepth 1 -name '*.patch' | LC_ALL=C sort
    find "$here/patches/mesa-sysdrv" -maxdepth 1 -name '*.patch' | LC_ALL=C sort)

# Refusals, before anything is downloaded: a patch that adds or deletes a file, and a non-WSI
# Turnip patch that shares a file with the WSI series or PanVK's (build-panvk.sh applies the
# WSI series and its own patches without it, so one tree would no longer stand for both).
refused=0
declare -A shared=()
for p in "${series[@]}"; do
    if grep -qE '^(--- /dev/null|\+\+\+ /dev/null)' "$p"; then
        echo "refusing ${p#"$here/patches/"}: it adds or deletes a file; refresh it by hand" >&2
        refused=1
    fi
    case "$p" in
        */patches/mesa/*-wsi-*|*/patches/mesa-panvk/*)
            while IFS= read -r f; do shared["$f"]=1; done < <(patch_files "$p") ;;
    esac
done
for p in "${series[@]}"; do
    [[ "$p" == */patches/mesa/* && "$p" != */patches/mesa/*-wsi-* ]] || continue
    while IFS= read -r f; do
        if [[ -n "${shared[$f]:-}" ]]; then
            echo "refusing ${p#"$here/patches/"}: it changes $f, which a WSI or PanVK patch changes" \
                "too; the two builds would need two trees" >&2
            refused=1
        fi
    done < <(patch_files "$p")
done
# Venus's build applies no PanVK patch: a file of both would need a tree of its own.
declare -A panvk=()
for p in "${series[@]}"; do
    [[ "$p" == */patches/mesa-panvk/* ]] || continue
    while IFS= read -r f; do panvk["$f"]=1; done < <(patch_files "$p")
done
for p in "${series[@]}"; do
    [[ "$p" == */patches/mesa-sysdrv/* ]] || continue
    while IFS= read -r f; do
        if [[ -n "${panvk[$f]:-}" ]]; then
            echo "refusing ${p#"$here/patches/"}: it changes $f, which a PanVK patch changes too;" \
                "the Venus build would need a tree of its own" >&2
            refused=1
        fi
    done < <(patch_files "$p")
done
[[ $refused == 0 ]] || exit 1

base="$USERLAND_CACHE/mesa-update"
tree="$base/$commit"
work="$base/work-$commit"
mkdir -p "$base"

# The archive, from the URL build-mesa.sh uses. A new commit's archive stays in the scratch folder
# until --write. For the current pin the file in sources/ is used when it has MESA_SHA256 (as
# fetch_source, a file that does not is downloaded again into the scratch folder).
url="$repo/-/archive/$commit/mesa-$commit.tar.gz"
cached="$USERLAND_CACHE/sources/$(basename "$url")"
tarball="$base/mesa-$commit.tar.gz"
if [[ "$commit" == "$MESA_COMMIT" && -f "$cached" ]] &&
    echo "$MESA_SHA256  $cached" | sha256sum -c --quiet - >/dev/null 2>&1; then
    tarball="$cached"
elif [[ ! -f "$tarball" ]]; then
    curl -fsSL --retry 3 -o "$tarball.part" "$url"
    mv "$tarball.part" "$tarball"
fi
sha="$(sha256sum < "$tarball" | cut -d' ' -f1)"
echo "archive:     $url"
echo "sha256:      $sha"
if [[ "$commit" == "$MESA_COMMIT" && "$sha" != "$MESA_SHA256" ]]; then
    echo "the archive of the pinned commit does not have MESA_SHA256 ($MESA_SHA256): GitLab may have" \
        "compressed it anew. Compare its tar (gzip -dc | sha256sum) with git archive of the commit" \
        "before MESA_SHA256 changes by hand; nothing written" >&2
    exit 1
fi

# Now that this commit's archive is here: the scratch folder keeps only this commit's tree, work
# folder and archive (the tree of a commit tried before goes, with its .rej files).
find "$base" -mindepth 1 -maxdepth 1 ! -name "$commit" ! -name "work-$commit" ! -name "mesa-$commit.tar.gz" \
    -exec rm -rf {} +

rm -rf "$tree" "$work"
mkdir -p "$tree" "$work"
tar -xzf "$tarball" -C "$tree" --strip-components=1
version="$(cat "$tree/VERSION")"
echo "version:     $version"

# Before each patch, the files it changes are kept (work/<n>/), so that --write can refresh it
# against the tree it was applied to.
failed=0 n=0
for p in "${series[@]}"; do
    n=$((n + 1))
    name="${p#"$here/patches/"}"
    while IFS= read -r f; do
        [[ -f "$tree/$f" ]] && install -D -m 0644 "$tree/$f" "$work/$n/$f"
    done < <(patch_files "$p")
    if out="$(patch -d "$tree" -p1 --forward --fuzz=0 --dry-run < "$p" 2>&1)"; then
        patch -d "$tree" -p1 --forward --fuzz=0 --no-backup-if-mismatch --quiet < "$p"
        if grep -q 'offset' <<<"$out"; then echo "offsets  $name"; else echo "exact    $name"; fi
    else
        failed=1
        echo "FAILS    $name"
        grep -E 'FAILED|reversed|can.t find' <<<"$out" | sed 's/^/           /' || true
        if patch -d "$tree" -p1 --forward --fuzz=3 --dry-run < "$p" >/dev/null 2>&1; then
            echo "           (applies with fuzz: only its context moved)"
        fi
        patch -d "$tree" -p1 --forward --fuzz=0 --no-backup-if-mismatch --quiet < "$p" >/dev/null 2>&1 || true
    fi
done
if [[ $failed == 1 ]]; then
    echo "a patch does not apply to $commit: rebase it by hand (scratch tree with .rej files:" \
        "$tree); nothing written" >&2
    exit 1
fi
echo "every patch applies to $commit"

# The system-driver mode (build-sysdrv.sh): the Venus protocol release the new tree's wrap names,
# and the patches the older GL stack shares, on that older Mesa.
wrap_dir="$(sed -n 's/^directory *= *//p' "$tree/subprojects/venus-protocol.wrap" 2>/dev/null || true)"
if [[ "$wrap_dir" != "venus-protocol-${VENUS_PROTOCOL_VERSION#v}" ]]; then
    echo "the new tree's subprojects/venus-protocol.wrap names '${wrap_dir:-nothing}', not" \
        "venus-protocol-${VENUS_PROTOCOL_VERSION#v}: move config.sh's VENUS_PROTOCOL_* and devdriver/build.sh's" \
        "VPROTO_* to that release first (both sides speak one protocol); nothing written" >&2
    exit 1
fi
sysdrv_tarball="$USERLAND_CACHE/sources/$(basename "$MESA_SYSDRV_URL")"
if ! echo "$MESA_SYSDRV_SHA256  $sysdrv_tarball" | sha256sum -c --quiet - >/dev/null 2>&1; then
    mkdir -p "$(dirname "$sysdrv_tarball")"
    curl -fsSL --retry 3 -o "$sysdrv_tarball.part" "$MESA_SYSDRV_URL"
    echo "$MESA_SYSDRV_SHA256  $sysdrv_tarball.part" | sha256sum -c --quiet - ||
        { rm -f "$sysdrv_tarball.part"; echo "$(basename "$MESA_SYSDRV_URL"): SHA-256 mismatch; nothing written" >&2; exit 1; }
    mv "$sysdrv_tarball.part" "$sysdrv_tarball"
fi
# [patch file] applies to the older Mesa with --fuzz=0: only its files are unpacked.
applies_to_sysdrv_gl() {
    local scratch rc=0 files
    scratch="$(mktemp -d "$base/sysdrv-gl.XXXXXX")"
    mapfile -t files < <(patch_files "$1")
    tar -xJf "$sysdrv_tarball" -C "$scratch" --strip-components=1 "${files[@]/#/mesa-$MESA_SYSDRV_VERSION/}" || rc=1
    [[ $rc == 0 ]] && { patch -d "$scratch" -p1 --forward --fuzz=0 --dry-run --quiet < "$1" >/dev/null 2>&1 || rc=1; }
    rm -rf "$scratch"
    return "$rc"
}
for shared in "${MESA_SYSDRV_GL_SHARED_PATCHES[@]}"; do
    applies_to_sysdrv_gl "$here/patches/mesa/$shared" ||
        { echo "patches/mesa/$shared does not apply to Mesa $MESA_SYSDRV_VERSION (build-sysdrv.sh); nothing written" >&2; exit 1; }
done
echo "the system-driver mode: venus-protocol ${VENUS_PROTOCOL_VERSION#v} as the wrap names; ${MESA_SYSDRV_GL_SHARED_PATCHES[*]} apply to Mesa $MESA_SYSDRV_VERSION"
if [[ $write != 1 ]]; then
    rm -rf "$work"
    exit 0
fi
[[ "$version" =~ ^[0-9A-Za-z._-]+$ ]] || { echo "unexpected VERSION '$version': nothing written" >&2; exit 1; }

# Refresh: each patch as diff -u -p of its files before it and after it alone (a later patch may
# change the same file), its description kept as it is. Made in work/new/ first; nothing in
# patches/ or config.sh changes until every refreshed patch is there.
mkdir -p "$work/new"
n=0
for p in "${series[@]}"; do
    n=$((n + 1))
    new="$work/new/$n.patch"
    awk '/^--- a\//{exit} {print}' "$p" > "$new"
    while IFS= read -r f; do
        # This file's part of the patch, applied to the file as it was before the patch.
        awk -v f="a/$f" '/^--- / { keep = ($2 == f) } keep' "$p" > "$work/part"
        patch -s --batch --forward --fuzz=0 -o "$work/after" "$work/$n/$f" < "$work/part"
        rc=0
        diff -u -p --label "a/$f" --label "b/$f" "$work/$n/$f" "$work/after" >> "$new" || rc=$?
        [[ $rc == 1 ]] || { echo "refreshing ${p#"$here/patches/"}: no change in $f (diff exit $rc)" >&2; exit 1; }
        rm -f "$work/part" "$work/after"
    done < <(patch_files "$p")
    # A refreshed patch that the older GL stack shares must still apply there.
    for shared in "${MESA_SYSDRV_GL_SHARED_PATCHES[@]}"; do
        if [[ "$p" == "$here/patches/mesa/$shared" ]] && ! applies_to_sysdrv_gl "$new"; then
            echo "the refreshed patches/mesa/$shared does not apply to Mesa $MESA_SYSDRV_VERSION (build-sysdrv.sh):" \
                "keep both forms or rebase it by hand; nothing written" >&2
            exit 1
        fi
    done
done
# The new config.sh is made and checked in work/ too. Then the archive, config.sh (written in
# place, so that it keeps its mode) and the patches go to their places: if a write stops part way,
# the new pin with the old patches still builds (every old patch applies to the new tree).
sed -e "s/^MESA_COMMIT=.*/MESA_COMMIT=$commit/" -e "s/^MESA_SHA256=.*/MESA_SHA256=$sha/" \
    -e "s/^MESA_VERSION=.*/MESA_VERSION=$version/" "$here/config.sh" > "$work/config.sh"
if ! { grep -qx "MESA_COMMIT=$commit" "$work/config.sh" && grep -qx "MESA_SHA256=$sha" "$work/config.sh" &&
    grep -qx "MESA_VERSION=$version" "$work/config.sh"; }; then
    echo "config.sh could not be made as expected (MESA_COMMIT, MESA_SHA256, MESA_VERSION): nothing written" >&2
    exit 1
fi
trap 'echo "a step failed (line $LINENO) while writing: config.sh and patches/ can be partly written;" \
    "git checkout -- userland/config.sh userland/patches takes them back" >&2; exit 1' ERR
if [[ "$tarball" != "$cached" ]]; then
    mkdir -p "$(dirname "$cached")"
    mv -f "$tarball" "$cached"
fi
cat "$work/config.sh" > "$here/config.sh"
n=0
for p in "${series[@]}"; do
    n=$((n + 1))
    mv "$work/new/$n.patch" "$p"
done
rm -rf "$work"
echo "written: config.sh (MESA_COMMIT, MESA_SHA256, MESA_VERSION) and the refreshed patches;" \
    "next: build.sh, the app build and the gate tests"
