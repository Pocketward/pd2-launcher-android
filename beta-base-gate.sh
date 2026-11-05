#!/usr/bin/env bash
# The release workflow's gate for a beta: its versionCode must equal the code of the newest
# published Stable, so that the beta and that Stable replace each other in place (betaBase,
# versionCodeOf in app/build.gradle.kts). A Stable passes with no check.
# Usage: beta-base-gate.sh <versionName> <versionCode> < <the tags of the published Stables, one per line>
#        beta-base-gate.sh --draft <release tag> <owner/repo>
# --draft is for scripts/export-android.sh --publish-candidate: it reads the code from the name of the
# draft's APK (pd2-launcher-android-<version>-<code>.apk) and the Stables from the repository, with gh.
# The workflow gives the tags of the releases that are not drafts and not pre-releases. A tag that is
# not vMAJOR.MINOR.PATCH is ignored. The newest Stable is the highest version, not the latest date.
set -euo pipefail
stable='[0-9]+\.[0-9]{1,2}\.[0-9]{1,2}'
if [[ ${1:-} == --draft ]]; then
    release_tag=${2:?usage: beta-base-gate.sh --draft <release tag> <owner/repo>}
    repo=${3:?usage: beta-base-gate.sh --draft <release tag> <owner/repo>}
    name=${release_tag#v}
    [[ $name == *-* ]] || exit 0
    assets=$(gh release view "$release_tag" --repo "$repo" --json assets --jq '.assets[].name')
    code=$(sed -n "s/^pd2-launcher-android-${name//./\\.}-\\([0-9]\\+\\)\\.apk\$/\\1/p" <<<"$assets")
    if [[ -z $code ]]; then
        echo "::error::the draft $release_tag has no APK named pd2-launcher-android-$name-<code>.apk, so its betaBase cannot be checked"
        exit 1
    fi
    gh api "repos/$repo/releases?per_page=100" --paginate \
        --jq '.[] | select(.draft == false and .prerelease == false) | .tag_name' |
        "$0" "$name" "$code"
    exit
fi
name=${1:?usage: beta-base-gate.sh <versionName> <versionCode> < tags}
code=${2:?usage: beta-base-gate.sh <versionName> <versionCode> < tags}
stable_code() { local major minor patch; IFS=. read -r major minor patch <<<"$1"; echo "$((((10#$major * 100 + 10#$minor) * 100 + 10#$patch) * 100 + 99))"; }
if [[ $name =~ ^$stable$ ]]; then exit 0; fi
[[ $name =~ ^$stable-beta([1-9]|[1-8][0-9]|9[0-8])$ ]] ||
    { echo "::error::versionName $name is not MAJOR.MINOR.PATCH or MAJOR.MINOR.PATCH-betaN"; exit 1; }
newest=
newest_code=-1
while IFS= read -r tag; do
    [[ $tag =~ ^v($stable)$ ]] || continue
    c=$(stable_code "${BASH_REMATCH[1]}")
    if ((c > newest_code)); then newest=${BASH_REMATCH[1]}; newest_code=$c; fi
done
if [[ -z $newest ]]; then
    echo "::error::the beta $name cannot be checked: this repository has no published Stable release, so there is no betaBase to compare. Publish a Stable first"
    exit 1
fi
if [[ $code != "$newest_code" ]]; then
    echo "::error::the beta $name has versionCode $code, which is not the code $newest_code of the newest published Stable $newest. Set betaBase to \"$newest\" in app/build.gradle.kts and export the beta again"
    exit 1
fi
echo "the beta $name has betaBase $newest, the newest published Stable (versionCode $code)"
