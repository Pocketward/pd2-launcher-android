#!/usr/bin/env bash
# The APK's name on a release, for a versionName. A Stable: pd2-launcher-android-<version>.apk, the
# name that the updater of 0.3.x reads. A beta: pd2-launcher-android-<version>-<code>.apk, as a
# beta's versionCode does not follow from its name: it is the code of the Stable it was made after
# (betaBase, versionCodeOf in app/build.gradle.kts), and the app reads it from this name.
# Usage: apk-name.sh <versionName> [betaBase (default: the one in app/build.gradle.kts)]
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
stable='[0-9]+\.[0-9]{1,2}\.[0-9]{1,2}'
version=${1:?usage: apk-name.sh <versionName> [betaBase]}
if [[ $version =~ ^$stable$ ]]; then
    echo "pd2-launcher-android-$version.apk"
    exit
fi
[[ $version =~ ^$stable-beta([1-9]|[1-8][0-9]|9[0-8])$ ]] ||
    { echo "versionName $version is not MAJOR.MINOR.PATCH or MAJOR.MINOR.PATCH-betaN" >&2; exit 1; }
base=${2:-$(sed -n 's/^ *val betaBase: String? = "\(.*\)"$/\1/p' "$here/app/build.gradle.kts")}
[[ $base =~ ^$stable$ ]] || { echo "the beta $version needs a betaBase MAJOR.MINOR.PATCH, not '$base'" >&2; exit 1; }
IFS=. read -r major minor patch <<<"$base"
echo "pd2-launcher-android-$version-$((((10#$major * 100 + 10#$minor) * 100 + 10#$patch) * 100 + 99)).apk"
