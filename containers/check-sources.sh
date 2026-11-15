#!/usr/bin/env bash
# Lists every version that the base image pins (Containerfile.base) and that deb.debian.org no longer
# has: a package of base-packages.lock (apt, in a $DEBIAN_IMAGE container with deb.debian.org's
# trixie, trixie-updates and trixie-security only), a .deb of userland/packages.lock and the glibc
# source files (their pool URLs). The base image then takes them from snapshot.debian.org at
# DEBIAN_SNAPSHOT, which is slower; a new lock (a maintainer step) moves them to Debian's current
# versions. Run it before an export. Exit 0 when deb.debian.org has everything, else 1.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../userland/config.sh
. "$here/../userland/config.sh"
missing=0

echo "== base-packages.lock: apt against deb.debian.org"
grep -v '^#' "$here/base-packages.lock" | grep . > "${TMPDIR:-/tmp}/base-packages.$$"
trap 'rm -f "${TMPDIR:-/tmp}/base-packages.$$"' EXIT
out="$("$(container_engine)" run --rm --platform linux/amd64 -v "${TMPDIR:-/tmp}/base-packages.$$:/lock:ro" "$DEBIAN_IMAGE" bash -c '
    set -e
    rm -f /etc/apt/sources.list.d/*
    printf "%s\n" "Types: deb" "URIs: http://deb.debian.org/debian" "Suites: trixie trixie-updates" "Components: main" \
        "Signed-By: /usr/share/keyrings/debian-archive-keyring.gpg" "" "Types: deb" "URIs: http://deb.debian.org/debian-security" \
        "Suites: trixie-security" "Components: main" "Signed-By: /usr/share/keyrings/debian-archive-keyring.gpg" \
        > /etc/apt/sources.list.d/debian.sources
    dpkg --add-architecture arm64
    apt-get update -qq >/dev/null
    n=0
    while read -r pin; do
        n=$((n + 1))
        # apt-cache show prints nothing (and exits 0) for a version that is not there.
        apt-cache show "$pin" 2>/dev/null | grep -q "^Package:" ||
            echo "MISSING $pin (now: $(apt-cache policy "${pin%%=*}" | sed -n "s/^ *Candidate: //p"))"
    done < /lock
    echo "checked $n"')"
grep '^MISSING' <<<"$out" && missing=1
grep '^checked' <<<"$out"

echo "== userland/packages.lock: deb.debian.org's pool"
n=0
while read -r set name version arch sha size url; do
    n=$((n + 1))
    pool="${url#*/archive/}"; repo="${pool%%/*}"; pool="${pool#*/}"; pool="${pool#*/}"
    code="$(curl -s -o /dev/null -I -w '%{http_code}' "http://deb.debian.org/$repo/$pool")"
    [[ "$code" == 200 ]] || { echo "MISSING $name=$version ($arch): HTTP $code"; missing=1; }
done < <(grep -v '^#' "$USERLAND_SRC/packages.lock")
echo "checked $n"

echo "== glibc source package: deb.debian.org's pool"
for f in $(sed -n 's/^GLIBC_\(DSC\|ORIG\|DEBIAN\)=//p' "$USERLAND_SRC/glibc/build.sh"); do
    code="$(curl -s -o /dev/null -I -w '%{http_code}' "http://deb.debian.org/debian/pool/main/g/glibc/${f//+/%2B}")"
    [[ "$code" == 200 ]] || { echo "MISSING $f: HTTP $code"; missing=1; }
done
echo "checked 3"
[[ "$missing" == 0 ]] && echo "deb.debian.org has every pinned version"
exit "$missing"
