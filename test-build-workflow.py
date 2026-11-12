#!/usr/bin/env python3
# Writes .github/workflows/test-build.yml from .github/workflows/release.yml: the test build for a
# player takes release.yml's build jobs as they are, so the two cannot drift apart. With --check it
# only fails when the file differs from what it would write (the Checks run it).
import pathlib
import re
import sys

HERE = pathlib.Path(__file__).resolve().parent
RELEASE = HERE / ".github/workflows/release.yml"
TEST_BUILD = HERE / ".github/workflows/test-build.yml"
rel = RELEASE.read_text()
def jobs(s):
    idx = [(m.group(1), m.start()) for m in re.finditer(r'^  ([a-z_-]+):$', s, re.M)]
    return {n: s[a:(idx[i+1][1] if i+1 < len(idx) else len(s))] for i, (n, a) in enumerate(idx)}
J = jobs(rel)
def must(s, a, b, count=1):
    assert s.count(a) == count, (a, s.count(a))
    return s.replace(a, b)

# The cache key step of the tag job, as release.yml has it.
tag = J['tag']
keys = tag[tag.index('      - name: Name the caches\n'):]
keys = must(keys, "        if: steps.tag.outputs.tag != ''\n", "")
# The comment of release.yml's next job (draft) ends the slice: it is not the tag job's.
keys = re.split(r"\n  #", keys, maxsplit=1)[0] + "\n"
assert keys.count("      - ") == 1, "release.yml's tag job has a step after Name the caches: take it here or not"

def build_job(name):
    j = J[name]
    j = must(j, "    needs: [tag, draft]\n    if: needs.tag.outputs.tag != '' && needs.draft.outputs.reuse != 'true'\n", "    needs: tag\n") if name != 'runtime' else j
    j = re.sub(r"\n          ref: \$\{\{ needs\.tag\.outputs\.commit \}\}", "", j)
    # A test starts from a published Stable, whose engine is released: no unreleased engine.
    j, n = re.subn(r"    # A candidate's engine may be a draft release[^\n]*\n(    #[^\n]*\n)*    env:\n      PD2_ENGINE_UNRELEASED: \$\{\{ needs\.tag\.outputs\.candidate == 'true' && '1' \|\| '' \}\}\n", "", j)
    assert n == (0 if name == "userland" else 1), name
    assert 'needs.tag.outputs.commit' not in j and 'candidate' not in j and 'draft' not in j, name
    return j

rt = J['runtime']
assert rt.startswith('  runtime:\n')
apk = J['apk']
cut = apk.index('      # The tag may have moved since the tag job looked')
apk = apk[:cut]
apk = must(apk, """    # The release key's secrets are the environment's, so no other job can read them.
    environment: release
    permissions:
      contents: write
    env:
      TAG: ${{ needs.tag.outputs.tag }}
""", """    # The release key's secrets are the environment test-build's, which waits for the owner's
    # approval before this job starts, so no other job can read them.
    environment: test-build
    permissions:
      contents: read
    env:
      LABEL: ${{ needs.tag.outputs.label }}
      STABLE: ${{ needs.tag.outputs.stable }}
""")
apk = must(apk, "      # Not persisted: the release is published with gh, not git.\n", "      # Not persisted: no step here uses Git's credentials.\n")
apk = re.sub(r"\n          ref: \$\{\{ needs\.tag\.outputs\.commit \}\}", "", apk)
apk = must(apk, "      - name: Build the signed release APK\n        run: ./build.sh release\n",
 "      - name: Build the signed release APK\n        run: PD2_TEST_BUILD=$LABEL ./build.sh release\n")
desc_start = apk.index('      - name: Name the APK and describe it\n')
apk = apk[:desc_start] + r'''      # The APK must replace the Stable in place, and the Stable must replace it: the same package,
      # versionName, versionCode and certificate as the newest published Stable.
      - name: Name the APK and check it
        id: apk
        env:
          IMAGE: ${{ steps.image.outputs.name }}
          GH_TOKEN: ${{ github.token }}
        run: |
          tools() { podman run --rm -v "$PWD/build/outputs:/apk:ro" "$IMAGE" "$@"; }
          badging=$(tools aapt2 dump badging /apk/app-release.apk | head -n 1)
          version_name=$(sed -n "s/.* versionName='\([^']*\)'.*/\1/p" <<<"$badging")
          version_code=$(sed -n "s/.* versionCode='\([^']*\)'.*/\1/p" <<<"$badging")
          package=$(sed -n "s/^package: name='\([^']*\)'.*/\1/p" <<<"$badging")
          certificate=$(tools apksigner verify --print-certs /apk/app-release.apk |
            sed -n 's/^Signer #1 certificate SHA-256 digest: //p')
          [[ -n $version_name && -n $version_code && -n $certificate ]]
          [[ $package == com.pd2launcher.app ]] ||
            { echo "::error::the APK's package is ${package:-unknown}, not com.pd2launcher.app"; exit 1; }
          [[ v$version_name == "$STABLE" ]] ||
            { echo "::error::the APK's versionName is $version_name, not the base release's ${STABLE#v}"; exit 1; }
          stable_apk=$(./apk-name.sh "$version_name")
          gh release download "$STABLE" --repo "$GITHUB_REPOSITORY" --pattern "$stable_apk" --dir stable
          stable_badging=$(podman run --rm -v "$PWD/stable:/apk:ro" "$IMAGE" aapt2 dump badging "/apk/$stable_apk" | head -n 1)
          stable_code=$(sed -n "s/.* versionCode='\([^']*\)'.*/\1/p" <<<"$stable_badging")
          stable_certificate=$(podman run --rm -v "$PWD/stable:/apk:ro" "$IMAGE" apksigner verify --print-certs "/apk/$stable_apk" |
            sed -n 's/^Signer #1 certificate SHA-256 digest: //p')
          [[ $version_code == "$stable_code" ]] ||
            { echo "::error::the APK's versionCode is $version_code, not the Stable's $stable_code"; exit 1; }
          [[ $certificate == "$stable_certificate" ]] ||
            { echo "::error::the APK's certificate is $certificate, not the Stable's $stable_certificate"; exit 1; }
          unzip -p build/outputs/app-release.apk 'classes*.dex' | grep -aqF "$version_name (test $LABEL)" ||
            { echo "::error::the APK does not show \"$version_name (test $LABEL)\""; exit 1; }
          apk=pd2-launcher-android-$version_name-test-$LABEL.apk
          mkdir -p test-build
          cp build/outputs/app-release.apk "test-build/$apk"
          (cd test-build && sha256sum "$apk" > "$apk.sha256")
          # R8's mapping of this build, for the stack traces in the player's logs (retrace).
          grep -q '^# pg_map_id: ' app/build/outputs/mapping/release/mapping.txt ||
            { echo "::error::the build wrote no R8 mapping (app/build/outputs/mapping/release/mapping.txt)"; exit 1; }
          cp app/build/outputs/mapping/release/mapping.txt "test-build/${apk%.apk}-mapping.txt"
          cat "test-build/$apk.sha256"
          echo "Package $package, versionName $version_name, versionCode $version_code, certificate $certificate, commit $GITHUB_SHA."
          echo "name=${apk%.apk}" >> "$GITHUB_OUTPUT"

      # One artifact, named as the APK without .apk, which holds the APK, its .sha256 file and
      # its mapping.
      - name: Upload the APK
        uses: actions/upload-artifact@cf430e030ddbb5b0abf93d22962f4752f3646cd9 # v7.0.2
        with:
          name: ${{ steps.apk.outputs.name }}
          path: test-build/
          if-no-files-found: error
          retention-days: 7
'''

head = r'''# GENERATED from release.yml by test-build-workflow.py: do not edit. Change release.yml or the
# generator, then run ./test-build-workflow.py (the Checks run it with --check).
#
# The test build for a player: a fix on top of the newest published Stable, built as that Stable's
# own release APK (package com.pd2launcher.app, its versionName and versionCode, the release key),
# so that it installs over the Stable and the Stable installs over it, in place, both ways, and
# the next Stable updates it. About shows "<version> (test <label>)" (build.sh: PD2_TEST_BUILD), and
# the logs name its commit. It publishes nothing: the APK, its .sha256 and its R8 mapping are one
# artifact of the run, for 7 days. Reading the artifact needs a GitHub login.
#
# To start it, push a branch test/<label> or test/<label>-<words> that starts from the Stable's
# commit (the label is digits, the issue's number: test/13-taskset gives "0.3.8 (test 13)"; 0.3.8 is the first Stable that can show it).
# The branch carries this file. The apk job then waits for the owner's approval (environment
# test-build), and only that job reads the release key.
#
# The engine, userland, wine and runtime jobs are release.yml's: the cache keys and paths are the
# same, so a branch restores the caches of main's newest release build while its inputs are the
# same (a branch can read the caches of the default branch). A part whose inputs differ is built
# again and saved in a cache of the branch, which counts against the repository's cache space.
#
# While the engine's repository is private, the secret ENGINE_REPO_TOKEN (read access to its
# contents) lets Git and the GitHub CLI fetch from it; a public one needs none.
name: Test build

on:
  push:
    branches: ['test/**']

permissions:
  contents: read

env:
  # The runtime's parts that the wine job builds (in parts/: Wine, wowbox64.dll and the sound
  # plugin) and the runtime's downloads, as the wine and runtime jobs cache and restore them.
  WINE_PARTS: |
    ~/.cache/pd2-android/runtime/parts
    ~/.cache/pd2-android/runtime/dl

concurrency:
  group: test-build-${{ github.ref }}
  cancel-in-progress: false

jobs:
  # The test's label and Stable, and the names of the caches, as release.yml's tag job gives them.
  tag:
    runs-on: ubuntu-24.04
    timeout-minutes: 5
    outputs:
      label: ${{ steps.test.outputs.label }}
      stable: ${{ steps.test.outputs.stable }}
'''
outs = re.findall(r"^      (userland|runtime|engine|wine|image_[a-z_]+): \$\{\{ steps\.keys\.outputs\.[a-z_]+ \}\}\n", tag, re.M)
outblock = ''.join(re.findall(r"^      (?:userland|runtime|engine|wine|image_[a-z_]+): \$\{\{ steps\.keys\.outputs\.[a-z_]+ \}\}\n", tag, re.M))
assert len(outs) >= 10, outs
head += outblock + r'''    steps:
      # The whole history: the branch must start at the Stable's commit.
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
        with:
          fetch-depth: 0
          persist-credentials: false

      # The label from the branch's name, and the newest published Stable: the branch starts at its
      # commit, names its versionName, and can show the label.
      - name: Find the label and the Stable
        id: test
        env:
          BRANCH: ${{ github.ref_name }}
          GH_TOKEN: ${{ github.token }}
        run: |
          [[ $BRANCH =~ ^test/([0-9]{1,8})(-[0-9A-Za-z-]+)?$ ]] ||
            { echo "::error::the branch $BRANCH is not test/<label> or test/<label>-<words> (the label is the issue's number)"; exit 1; }
          label=${BASH_REMATCH[1]}
          # The base: the newest published Stable, or the newest published Beta for a player on the
          # Beta channel (a Beta has its Stable's versionCode, so the test installs over both).
          latest=$(gh api "repos/$GITHUB_REPOSITORY/releases/latest" --jq .tag_name)
          beta=$(gh api "repos/$GITHUB_REPOSITORY/releases?per_page=30" --jq '[.[] | select(.prerelease and (.draft | not))][0].tag_name // ""')
          version=$(sed -n 's/^ *versionName = "\([^"]*\)"$/\1/p' app/build.gradle.kts)
          if [[ -n $beta && v$version == "$beta" ]]; then stable=$beta; else stable=$latest; fi
          [[ v$version == "$stable" ]] ||
            { echo "::error::the commit's versionName is ${version:-unknown}, not the newest Stable's ${latest#v} or Beta's ${beta#v}: start the branch from its commit"; exit 1; }
          git merge-base --is-ancestor "$(git rev-parse "refs/tags/$stable^{commit}")" HEAD ||
            { echo "::error::the branch does not start at the commit of $stable"; exit 1; }
          if ! grep -q PD2_TEST_BUILD build.sh || ! grep -q pd2.testBuild app/build.gradle.kts; then
            echo "::error::this tree cannot show a test label (PD2_TEST_BUILD): the Stable is older than 0.3.8"; exit 1
          fi
          echo "The commits on top of $stable:"
          git log --oneline "$stable..HEAD"
          echo "label=$label" >> "$GITHUB_OUTPUT"
          echo "stable=$stable" >> "$GITHUB_OUTPUT"
          echo "A test build of $stable, label $label, commit $GITHUB_SHA."

'''
out = head + keys.rstrip('\n') + '\n\n' + build_job('engine') + build_job('userland') + build_job('wine') + build_job('runtime') + apk

# Nothing that only release.yml's tag and draft jobs give may stay, and no job keeps a job-level key
# whose env: was removed.
left = re.findall(r"needs\.tag\.outputs\.(?:tag|commit|candidate)\b|needs\.draft\b|steps\.(?:tag|release)\.|\$TAG\b|\$COMMIT\b|NOTES_(?:BEGIN|END)", out)
assert not left, f"release.yml's own outputs are in the test build: {sorted(set(left))}"
assert not re.search(r"^    timeout-minutes: \d+\n      \w+:", out, re.M), "a job-level env key lost its env:"

if sys.argv[1:] == ["--check"]:
    if not TEST_BUILD.exists() or TEST_BUILD.read_text() != out:
        sys.exit(f"{TEST_BUILD.relative_to(HERE)} is not what ./test-build-workflow.py writes from release.yml: run it")
elif sys.argv[1:]:
    sys.exit("usage: ./test-build-workflow.py [--check]")
else:
    TEST_BUILD.write_text(out)
