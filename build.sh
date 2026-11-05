#!/usr/bin/env bash
# Builds the debug APK inside the pinned build container; never runs Gradle on the host.
# Output: build/outputs/app-debug.apk (paths are relative to this script's folder): the
# development build, com.pd2launcher.app.dev3, which installs beside the release app and the 0.2
# line's development build (PD2_DEV_AS_FEX_DEV=1 builds it as com.pd2launcher.app.dev).
# `build.sh release` builds the signed release APK instead: build/outputs/app-release.apk
# (com.pd2launcher.app; the release key and engine below).
# `build.sh check` compiles the debug APK without the App userland, the CI check; it copies no APK
# to build/outputs, as one without the userland cannot run. The JVM tests, where the repository
# has them (app/src/test), run on the debug variant. It also runs the release build's lint check
# (lintVitalRelease), which must pass before either check or release succeeds.
# `build.sh mockup` and `build.sh spike` build development apps that are not part of the
# public source.
# PD2_SYSTEM_DRIVER=0 builds the APK without the system-driver mode's parts (the game on the
# phone's own Vulkan driver): no virglrenderer server and no userland part
# app/system-driver/assets/userland-system-driver (Gradle property pd2.systemDriver=false). The
# default (1) builds the server (devdriver/build.sh) when needed and includes both.
# `build.sh verification-metadata` writes gradle/verification-metadata.xml again (after a
# dependency or plugin changes): the SHA-256 of everything Gradle downloads for the debug and
# release builds and the tests (and the mock-up and spike, where present), which Gradle checks on
# every build. The release's signing check is skipped there: that run only resolves.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
target="${1:-app}"
release_apk=app/build/outputs/apk/release/app-release.apk
after=""
# The mock-up and the spike are targets only where their folders are.
targets="app|release|check"
for module in mockup spike; do
    [[ -d "$here/$module" ]] && targets+="|$module"
done
targets+="|verification-metadata"
if [[ ("$target" == mockup || "$target" == spike) && ! -d "$here/$target" ]]; then
    echo "usage: build.sh [$targets] (there is no $target folder here)" >&2
    exit 2
fi
case "$target" in
    app) tasks=":app:assembleDebug :app:testDebugUnitTest" ;;
    release)
        tasks=":app:assembleRelease :app:testDebugUnitTest :app:lintVitalRelease"
        after=" && apksigner verify --verbose --print-certs $release_apk && zipalign -c 4 $release_apk"
        ;;
    # The release variant's lint check needs no release key: its signing guard (preReleaseBuild)
    # is skipped.
    check) tasks=":app:assembleDebug :app:testDebugUnitTest :app:lintVitalRelease -x :app:preReleaseBuild" ;;
    mockup) tasks=":mockup:assembleDebug" ;;
    spike) tasks=":spike:assembleDebug" ;;
    verification-metadata)
        tasks="--write-verification-metadata sha256 help :app:assembleDebug :app:testDebugUnitTest :app:assembleRelease -x :app:preReleaseBuild"
        ;;
    *) echo "usage: build.sh [$targets]" >&2; exit 2 ;;
esac

# PD2_DEV_AS_FEX_DEV=1: the development build gets the 0.2 line's development id
# (com.pd2launcher.app.dev) instead of its own (com.pd2launcher.app.dev3), so it installs over
# that build as an update in place (app/build.gradle.kts, pd2.asFexDev).
if [[ "${PD2_DEV_AS_FEX_DEV:-}" == 1 ]]; then
    tasks="-Ppd2.asFexDev=true $tasks"
fi
# PD2_DEV_R8=1: the development build also goes through R8 (code shrinking), as the release
# build always does, for device tests of the shrunk code (app/build.gradle.kts, pd2.devR8).
if [[ "${PD2_DEV_R8:-}" == 1 ]]; then
    tasks="-Ppd2.devR8=true $tasks"
fi

# PD2_SYSTEM_DRIVER=0: no system-driver parts in the APK (app/build.gradle.kts, pd2.systemDriver).
system_driver="${PD2_SYSTEM_DRIVER:-1}"
[[ "$system_driver" == 0 || "$system_driver" == 1 ]] || { echo "PD2_SYSTEM_DRIVER must be 0 or 1" >&2; exit 2; }
if [[ "$system_driver" == 0 ]]; then
    tasks="-Ppd2.systemDriver=false $tasks"
fi

# PD2_TEST_BUILD=<label>: a test build, shown as "<version> (test <label>)" with the version's own
# versionCode, and this tree's commit in device.txt (app/build.gradle.kts, pd2.testBuild).
if [[ -n "${PD2_TEST_BUILD:-}" ]]; then
    [[ "$PD2_TEST_BUILD" =~ ^[0-9A-Za-z.-]{1,32}$ ]] ||
        { echo "refusing: PD2_TEST_BUILD $PD2_TEST_BUILD is not 1 to 32 of 0-9, A-Z, a-z, '.' and '-'" >&2; exit 1; }
    commit="$(git -C "$here" rev-parse HEAD 2>/dev/null)" ||
        { echo "refusing: PD2_TEST_BUILD names the commit, but $here is not in a git checkout" >&2; exit 1; }
    [[ -z "$(git -C "$here" status --porcelain -- .)" ]] || commit+="-dirty"
    tasks="-Ppd2.testBuild=$PD2_TEST_BUILD -Ppd2.testCommit=$commit $tasks"
fi
# Without unit tests the test task would only compile the debug variant again.
[[ -d "$here/app/src/test" ]] || tasks="${tasks/ :app:testDebugUnitTest/}"
if [[ "$target" == verification-metadata ]]; then
    for module in mockup spike; do
        if [[ -d "$here/$module" ]]; then tasks+=" :$module:assembleDebug"; fi
    done
fi

cache="${PD2_ANDROID_CACHE:-$HOME/.cache/pd2-android}"

if [[ "${PD2_ANDROID_IN_CONTAINER:-}" == 1 ]]; then
    echo "refusing: build.sh runs on the host and starts the container itself" >&2
    exit 1
fi

# test-build.yml is written from release.yml (test-build-workflow.py): a change of one without the
# other fails here.
if [[ "$target" == check && -f "$here/test-build-workflow.py" ]]; then
    python3 "$here/test-build-workflow.py" --check
fi
if [[ "$target" == check && -f "$here/app/src/test/scripts/release-gate-test.sh" ]]; then
    bash "$here/app/src/test/scripts/release-gate-test.sh"
fi
if [[ "$target" == check && -f "$here/app/src/test/scripts/apk-name-test.sh" ]]; then
    bash "$here/app/src/test/scripts/apk-name-test.sh"
fi
if [[ "$target" == check && -f "$here/app/src/test/scripts/beta-base-gate-test.sh" ]]; then
    bash "$here/app/src/test/scripts/beta-base-gate-test.sh"
fi

if command -v podman >/dev/null 2>&1; then
    engine=podman
elif command -v docker >/dev/null 2>&1; then
    engine=docker
else
    echo "refusing: no container engine (podman or docker) found; Gradle is never run on the host" >&2
    exit 1
fi

# Image tag is a hash of the Containerfile, so edits rebuild and unchanged files reuse the cache.
hash="$(sha256sum "$here/containers/Containerfile" | cut -c1-16)"
image="localhost/pd2-android-build:$hash"

# Network: the default first, host networking as the fallback (netavark/pasta issues on some hosts).
net_args=()
try_build() {
    "$engine" build --platform linux/amd64 "${net_args[@]}" -t "$image" -f "$here/containers/Containerfile" "$here/containers"
}
if ! "$engine" image inspect "$image" >/dev/null 2>&1; then
    if ! try_build; then
        echo "build with default networking failed; retrying with --network host" >&2
        net_args=(--network host)
        try_build
    fi
fi

mkdir -p "$cache/gradle" "$cache/m2" "$cache/home"

# The Launcher engine (the standalone ARM64 Launcher without its window, which engine/build.sh
# builds from the sources engine.lock pins) goes into the APK as a native library.
# PD2_ENGINE_BINARY names it; the default is the newest build in $cache/engine-out. A release
# takes only the newest engine of $cache/engine-release-out whose build evidence names the pinned
# commit and sources (source_sha256) and its own SHA-256, whatever PD2_ENGINE_BINARY says, and it
# must be a release build (PD2_RELEASE_BUILD=1: no dev CD keys, Wine's quiet logs) by its build
# evidence.
engine_dir=engine-out
[[ "$target" == release ]] && engine_dir=engine-release-out
if [[ "$target" == release ]]; then
    pin() { sed -n "s/^$1=//p" "$here/engine.lock" | head -n 1; }
    evidence() { sed -n "s/^$1=//p" "$2.build-evidence.txt" 2>/dev/null | head -n 1; }
    engine_binary=""
    if [[ -n "$(pin source_sha256)" ]]; then
        while IFS= read -r candidate; do
            if [[ "$(evidence git_commit "$candidate")" == "$(pin commit)" &&
                "$(evidence source_sha256 "$candidate")" == "$(pin source_sha256)" &&
                "$(evidence sha256 "$candidate")" == "$(sha256sum < "$candidate" | cut -d' ' -f1)" ]]; then
                engine_binary="$candidate"
                break
            fi
        done < <(ls -t "$cache/$engine_dir"/pd2-launcher-*-aarch64 2>/dev/null || true)
    fi
    if [[ -z "$engine_binary" ]]; then
        echo "refusing: a release APK needs the engine built from the sources engine.lock pins (commit $(pin commit)," \
            "source_sha256 $(pin source_sha256)) in $cache/$engine_dir (PD2_RELEASE_BUILD=1 engine/build.sh);" \
            "PD2_ENGINE_BINARY is not used for a release" >&2
        exit 1
    fi
else
    engine_binary="${PD2_ENGINE_BINARY:-$(ls -t "$cache/$engine_dir"/pd2-launcher-*-aarch64 2>/dev/null | head -n 1 || true)}"
fi
engine_staged="$here/app/engine/arm64-v8a/libpd2launcher.so"
if [[ "$target" == release ]] && ! grep -qx 'release_build=1' "$engine_binary.build-evidence.txt" 2>/dev/null; then
    echo "refusing: a release APK needs a release engine, whose build evidence says release_build=1 (PD2_RELEASE_BUILD=1 engine/build.sh); found: ${engine_binary:-none}" >&2
    exit 1
fi
if [[ -n "$engine_binary" && -f "$engine_binary" ]]; then
    mkdir -p "$(dirname "$engine_staged")"
    cp "$engine_binary" "$engine_staged"
    echo "engine: $engine_binary"
else
    rm -f "$engine_staged"
    echo "warning: no engine binary (build one with engine/build.sh, or set PD2_ENGINE_BINARY); the APK has no engine" >&2
fi

# The system-driver mode's servers (devdriver/build.sh <dir>), bionic: virgl_test_server and
# virgl_render_server (the Venus route) and virgl_test_server_gl (the VirGL route) go into the APK
# as native libraries beside the engine (staged in
# app/system-driver/jniLibs, which the APK takes unless PD2_SYSTEM_DRIVER=0), so they are extracted
# executable into nativeLibraryDir. PD2_DEVDRIVER_OUT names their folder; the default is
# $cache/devdriver-out. They are built there again when the folder's SHA256SUMS does not match
# its binaries or its BUILDINFO is not the identity of today's sources (`devdriver/build.sh
# --identity`: the pins, the patches, the scripts), also in a folder that PD2_DEVDRIVER_OUT names
# (its binaries are then replaced). Without them the APK has no system-driver mode: the device
# check refuses GPUs that Turnip and PanVK do not drive. Only the app and release targets build
# them (not check, which runs no APK, nor PD2_SYSTEM_DRIVER=0); a release refuses the servers
# without the userland part, or the opposite.
rm -f "$here/app/engine/arm64-v8a/libvirgl_"*.so # where builds before the parts' folder put them
if [[ "$system_driver" == 1 && ("$target" == app || "$target" == release) ]]; then
    devdriver_out="${PD2_DEVDRIVER_OUT:-$cache/devdriver-out}"
    if ! (cd "$devdriver_out" 2>/dev/null && sha256sum -c --quiet SHA256SUMS 2>/dev/null) ||
        [[ "$(cat "$devdriver_out/BUILDINFO" 2>/dev/null)" != "$("$here/devdriver/build.sh" --identity)" ]]; then
        echo "building the system-driver server into $devdriver_out (devdriver/build.sh)"
        PD2_DEVDRIVER_SRC="${PD2_DEVDRIVER_SRC:-$cache/devdriver/src}" PD2_DEVDRIVER_WORK="${PD2_DEVDRIVER_WORK:-$cache/devdriver/build}" \
            "$here/devdriver/build.sh" "$devdriver_out"
    fi
    for server in virgl_test_server virgl_render_server virgl_test_server_gl; do
        staged="$here/app/system-driver/jniLibs/arm64-v8a/lib$server.so"
        if [[ -f "$devdriver_out/$server" ]]; then
            mkdir -p "$(dirname "$staged")"
            cp "$devdriver_out/$server" "$staged"
            echo "system driver: $devdriver_out/$server"
        else
            rm -f "$staged"
            echo "warning: no $devdriver_out/$server (devdriver/build.sh); the APK has no system-driver mode" >&2
        fi
    done
    servers=0
    for server in virgl_test_server virgl_render_server virgl_test_server_gl; do
        [[ -f "$here/app/system-driver/jniLibs/arm64-v8a/lib$server.so" ]] && servers=$((servers + 1))
    done
    part=0
    [[ -f "$here/app/system-driver/assets/userland-system-driver/manifest.json" ]] && part=1
    if [[ "$servers" != 3 || "$part" != 1 ]]; then
        # One without the other is no system-driver mode (SystemDriver.inApk): a release takes both or neither.
        if [[ "$target" == release ]]; then
            echo "refusing: the system-driver mode needs its three servers and the userland part (servers: $servers of 3," \
                "part: $part; userland/build.sh, devdriver/build.sh), or PD2_SYSTEM_DRIVER=0" >&2
            exit 1
        fi
        echo "warning: the system-driver mode is incomplete (servers: $servers of 3, userland part: $part); the APK has no such mode" >&2
    fi
elif [[ "$system_driver" == 0 ]]; then
    echo "system driver: left out (PD2_SYSTEM_DRIVER=0)"
fi

# The App userland is built for userland/config.sh's placeholder data folder, which the app
# rewrites to its own as it unpacks it, so the release app and the development build carry the
# same one, from app/src/main/assets/userland. A debug build makes it first when it is missing
# (userland/build.sh with our glibc); a release build needs it built. The engine may need
# only libraries the userland's critical set covers (its manifest's engine_needed).
if [[ "$target" == app || "$target" == release ]]; then
    # shellcheck source=userland/config.sh
    . "$here/userland/config.sh"
    manifest="$USERLAND_ASSETS/manifest.json"
    # A debug build type's own userland would replace the shared one in a debug APK.
    if [[ -e "$here/app/src/debug/assets/userland" ]]; then
        echo "refusing: app/src/debug/assets/userland, from an older build, would replace the userland; remove it" >&2
        exit 1
    fi
    if [[ "$target" == app && ! -f "$manifest" ]]; then
        echo "no userland in ${USERLAND_ASSETS#"$here/"}: building it (userland/build.sh)"
        PD2_USERLAND_WITH_GLIBC=1 "$here/userland/build.sh"
    fi
    prefix="$(sed -n 's/^ "prefix": "\(.*\)",$/\1/p' "$manifest" 2>/dev/null || true)"
    if [[ "$prefix" != "$USERLAND" ]]; then
        echo "refusing: the APK needs the userland built for $USERLAND in ${USERLAND_ASSETS#"$here/"}" \
            "(userland/build.sh); found one for: ${prefix:-none}" >&2
        exit 1
    fi
    if [[ -f "$engine_staged" ]]; then
        command -v readelf >/dev/null || { echo "refusing: readelf (binutils) is needed to read the engine's libraries" >&2; exit 1; }
        mapfile -t covered < <(sed -n '/^ "engine_needed": \[$/,/^ \],\{0,1\}$/s/^  "\(.*\)",\{0,1\}$/\1/p' "$manifest")
        for lib in $(readelf -d "$engine_staged" | sed -n 's/.*(NEEDED).*\[\(.*\)\]$/\1/p'); do
            if [[ " ${covered[*]} " != *" $lib "* ]]; then
                echo "refusing: the engine needs $lib, which the userland's critical set leaves out (ENGINE_NEEDED in userland/config.sh)" >&2
                exit 1
            fi
        done
    fi
    echo "userland: version $(sed -n 's/^ "version": "\(.*\)"$/\1/p' "$manifest")"
fi

run_args=(--rm -v "$here:/work" -v "$cache:/cache")

# Release signing (docs/BUILDING.md): the release key's folder, outside the
# repository, mounted read-only; app/build.gradle.kts reads release-signing.properties there.
if [[ "$target" == release ]]; then
    signing_dir="${PD2_RELEASE_SIGNING_DIR:-$HOME/.config/pd2launcher}"
    if [[ ! -f "$signing_dir/release-signing.properties" ]]; then
        echo "refusing: no release key ($signing_dir/release-signing.properties; docs/BUILDING.md)" >&2
        exit 1
    fi
    run_args+=(-v "$signing_dir:/signing:ro" -e PD2_RELEASE_SIGNING=/signing/release-signing.properties)
fi
if [[ "$engine" == podman ]]; then
    run_args+=(--userns=keep-id)
else
    run_args+=(--user "$(id -u):$(id -g)")
fi

run_gradle() {
    "$engine" run "${run_args[@]}" "$@" \
        -e PD2_ANDROID_IN_CONTAINER=1 \
        -e HOME=/cache/home \
        -e ANDROID_USER_HOME=/cache \
        -e GRADLE_USER_HOME=/cache/gradle \
        -e MAVEN_USER_HOME=/cache/m2 \
        "$image" \
        sh -c "mkdir -p \"\$HOME\" && ./gradlew --no-daemon $tasks$after"
}

# The run's output, also kept in a file: only a network failure (of the container's default
# network, or of Gradle's downloads) is tried again, with host networking; a failed build, test or
# lint check is not.
run_log="$(mktemp)"
trap 'rm -f "$run_log"' EXIT
network_failure() {
    grep -Eq 'netavark|pasta|slirp4netns|Could not (resolve|GET|HEAD)|UnknownHostException|Connection (refused|reset|timed out)|Network is unreachable|No route to host|Temporary failure in name resolution|Read timed out' "$run_log"
}
lint_failure() {
    grep -Eq "Execution failed for task ':app:lintVital(Analyze|Report)?Release'" "$run_log"
}
# AGP reads ANDROID_USER_HOME as its ".android" directory, so the debug keystore is $cache/debug.keystore.
if ! run_gradle "${net_args[@]}" 2>&1 | tee "$run_log"; then
    if [[ ${#net_args[@]} -eq 0 ]] && network_failure; then
        echo "gradle run failed on the network; retrying with --network host" >&2
        run_gradle --network host
    else
        if lint_failure; then
            echo "build failed: the release lint check (lintVitalRelease) found errors; its report is above" >&2
        else
            echo "build failed (not a network failure: no retry); the reason is above" >&2
        fi
        exit 1
    fi
fi

if [[ "$target" == mockup || "$target" == spike ]]; then
    echo "APK: $here/$target/build/outputs/apk/debug/$target-debug.apk"
    exit 0
fi
if [[ "$target" == check ]]; then
    if [[ -d "$here/app/src/test" ]]; then
        echo "check passed: the debug APK compiles, the JVM tests pass and the release lint check passes"
    else
        echo "check passed: the debug APK compiles and the release lint check passes (this repository has no JVM tests)"
    fi
    exit 0
fi
if [[ "$target" == verification-metadata ]]; then
    echo "written: $here/gradle/verification-metadata.xml"
    exit 0
fi

variant=debug
[[ "$target" == release ]] && variant=release
mkdir -p "$here/build/outputs"
cp "$here/app/build/outputs/apk/$variant/app-$variant.apk" "$here/build/outputs/app-$variant.apk"
echo "APK: $here/build/outputs/app-$variant.apk"
