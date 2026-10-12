#!/usr/bin/env bash
# Builds the debug APK inside the pinned build container; never runs Gradle on the host.
# Output: build/outputs/app-debug.apk (paths are relative to this script's folder): the
# development build, com.pd2launcher.app.dev, which installs beside the release app.
# `build.sh release` builds the signed release APK instead: build/outputs/app-release.apk
# (com.pd2launcher.app; the release key and engine below; the JVM tests run on the debug variant).
# `build.sh check` compiles the debug APK without the App userland and runs the JVM tests, the
# public repository's CI check; it copies no APK to build/outputs, as one without the userland
# cannot run.
# `build.sh mockup` and `build.sh spike` build development apps that are not part of the
# public source.
set -euo pipefail

target="${1:-app}"
release_apk=app/build/outputs/apk/release/app-release.apk
after=""
case "$target" in
    app) tasks=":app:assembleDebug :app:testDebugUnitTest" ;;
    release)
        tasks=":app:assembleRelease :app:testDebugUnitTest"
        after=" && apksigner verify --verbose --print-certs $release_apk && zipalign -c 4 $release_apk"
        ;;
    check) tasks=":app:assembleDebug :app:testDebugUnitTest" ;;
    mockup) tasks=":mockup:assembleDebug" ;;
    spike) tasks=":spike:assembleDebug" ;;
    *) echo "usage: build.sh [app|release|check|mockup|spike]" >&2; exit 2 ;;
esac

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cache="${PD2_ANDROID_CACHE:-$HOME/.cache/pd2-android}"

if [[ "${PD2_ANDROID_IN_CONTAINER:-}" == 1 ]]; then
    echo "refusing: build.sh runs on the host and starts the container itself" >&2
    exit 1
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
if ! "$engine" image exists "$image" 2>/dev/null; then
    if ! try_build; then
        echo "build with default networking failed; retrying with --network host" >&2
        net_args=(--network host)
        try_build
    fi
fi

mkdir -p "$cache/gradle" "$cache/m2" "$cache/home"

# The Launcher engine (the standalone ARM64 Launcher that engine/build.sh builds from the
# release engine.lock pins) goes into the APK as a native library. PD2_ENGINE_BINARY names
# it; the default is the newest build in $cache/engine-out; for a release, the newest in
# $cache/engine-release-out, which must be a release build (PD2_RELEASE_BUILD=1: no dev CD keys,
# Wine's quiet logs) by its build evidence.
engine_dir=engine-out
[[ "$target" == release ]] && engine_dir=engine-release-out
engine_binary="${PD2_ENGINE_BINARY:-$(ls -t "$cache/$engine_dir"/pd2-launcher-*-aarch64 2>/dev/null | head -n 1 || true)}"
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

# The App userland is built for one app's data folder (userland/config.sh). A debug APK, the
# development build com.pd2launcher.app.dev, carries the debug variant's from
# app/src/debug/assets/userland, which this builds first when it is missing (userland/build.sh
# with our glibc and FEX); a release APK carries the release app's from app/src/main/assets/userland,
# which userland/build.sh must have built. Either must name its own app's prefix.
if [[ "$target" == app || "$target" == release ]]; then
    PD2_USERLAND_VARIANT=debug
    [[ "$target" == release ]] && PD2_USERLAND_VARIANT=release
    export PD2_USERLAND_VARIANT
    # shellcheck source=userland/config.sh
    . "$here/userland/config.sh"
    manifest="$USERLAND_ASSETS/manifest.json"
    if [[ "$target" == app && ! -f "$manifest" ]]; then
        echo "no userland for $APP_ID in ${USERLAND_ASSETS#"$here/"}: building it (userland/build.sh)"
        PD2_USERLAND_WITH_GLIBC=1 PD2_USERLAND_WITH_FEX=1 "$here/userland/build.sh"
    fi
    prefix="$(sed -n 's/^ "prefix": "\(.*\)",$/\1/p' "$manifest" 2>/dev/null || true)"
    if [[ "$prefix" != "$USERLAND" ]]; then
        echo "refusing: the APK needs the userland for $APP_ID in ${USERLAND_ASSETS#"$here/"}" \
            "(PD2_USERLAND_VARIANT=$PD2_USERLAND_VARIANT userland/build.sh); found one for: ${prefix:-none}" >&2
        exit 1
    fi
    echo "userland: $APP_ID, version $(sed -n 's/^ "version": "\(.*\)"$/\1/p' "$manifest")"
fi

# Debug builds pre-fill the key screen from the developer's .env.local (D2_KEY, D2_LOD_KEY) for
# device tests. Only those two lines are read; the values travel by environment (-e NAME), never
# on a command line, and are never printed.
# The file is PD2_DEBUG_KEYS_FILE, else the first .env.local of: this folder; where this folder is
# inside a larger Git checkout, that checkout's root, then its main checkout's root (for a linked
# worktree).
keys_file="${PD2_DEBUG_KEYS_FILE:-$here/.env.local}"
if [[ -z "${PD2_DEBUG_KEYS_FILE:-}" && ! -f "$keys_file" ]] &&
    top="$(git -C "$here" rev-parse --show-toplevel 2>/dev/null)" && [[ "$top" != "$(cd "$here" && pwd -P)" ]]; then
    common="$(git -C "$here" rev-parse --path-format=absolute --git-common-dir)"
    keys_file="$top/.env.local"
    [[ -f "$keys_file" ]] || keys_file="$(dirname "$common")/.env.local"
fi
key_args=()
if [[ "$target" != release && -f "$keys_file" ]]; then
    PD2_DEBUG_D2_KEY="$(sed -n 's/^D2_KEY=//p' "$keys_file" | head -n 1 | tr -d "\"' \r")"
    PD2_DEBUG_LOD_KEY="$(sed -n 's/^D2_LOD_KEY=//p' "$keys_file" | head -n 1 | tr -d "\"' \r")"
    export PD2_DEBUG_D2_KEY PD2_DEBUG_LOD_KEY
    key_args=(-e PD2_DEBUG_D2_KEY -e PD2_DEBUG_LOD_KEY)
    echo "debug key prefill: from $keys_file"
fi

run_args=(--rm -v "$here:/work" -v "$cache:/cache" "${key_args[@]}")

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

# AGP reads ANDROID_USER_HOME as its ".android" directory, so the debug keystore is $cache/debug.keystore.
if ! run_gradle "${net_args[@]}"; then
    if [[ ${#net_args[@]} -eq 0 ]]; then
        echo "gradle run failed; retrying with --network host" >&2
        run_gradle --network host
    else
        exit 1
    fi
fi

if [[ "$target" == mockup || "$target" == spike ]]; then
    echo "APK: $here/$target/build/outputs/apk/debug/$target-debug.apk"
    exit 0
fi
if [[ "$target" == check ]]; then
    echo "check passed: the debug APK compiles and the JVM tests pass"
    exit 0
fi

variant=debug
[[ "$target" == release ]] && variant=release
mkdir -p "$here/build/outputs"
cp "$here/app/build/outputs/apk/$variant/app-$variant.apk" "$here/build/outputs/app-$variant.apk"
echo "APK: $here/build/outputs/app-$variant.apk"
