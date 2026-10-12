# Building PD2 Launcher for Android

Everything is built in pinned containers; only the container engine and a few file tools run on your computer. The APK is assembled from four parts, built in this order (paths are relative to the repository's root):

| Part | Script | Output |
| --- | --- | --- |
| The PD2 Launcher engine (ARM64) | `engine/build.sh` | `pd2-launcher-<version>-aarch64` |
| The App userland | `userland/build.sh` | `app/src/main/assets/userland/` (the development build's: `app/src/debug/assets/userland/`) |
| The runtime (RootFS, Wine-GE, Gecko, Mono, .NET) | `runtime/pack.sh` | `app/src/main/assets/runtime/` |
| The app | `build.sh` | `build/outputs/app-debug.apk` or `app-release.apk` |

None of these outputs is in the repository. A release build is the app `com.pd2launcher.app`, "PD2 Launcher"; a debug build is a separate development app, `com.pd2launcher.app.dev`, "PD2 Launcher Dev", which installs beside it with its own data. The App userland is built for the app's data folder, so each of the two needs its own (step 2).

## What you need

- An x86-64 Linux computer with Podman (or Docker), Git, curl, tar, zstd, `sha256sum`/`sha512sum` and `unsquashfs` (squashfs-tools).
- A static `qemu-aarch64` binfmt_misc handler with the `F` flag (Debian/Ubuntu: `qemu-user-static`; Arch: `qemu-user-static` and `qemu-user-static-binfmt`). The userland build runs ARM64 tools under it, and the engine build runs the built engine briefly to audit it.
- About 30 GB of free disk space for the build images, caches and outputs, and a good connection for the first build.

Every download is checked against a SHA-256 (or SHA-512) in the scripts, the lock files or the container definitions; a mismatch stops the build.

## 1. The engine

The engine is the desktop PD2 Launcher's standalone ARM64 executable, from a release of [Pocketward/pd2-launcher](https://github.com/Pocketward/pd2-launcher). `engine.lock` pins the release by its URL, its tag and the commit the tag must name, and names its ARM64 executable (`asset`) with its SHA-256.

```sh
engine/build.sh                        # download into ~/.cache/pd2-android/engine-out
PD2_RELEASE_BUILD=1 engine/build.sh    # for a release APK: into ~/.cache/pd2-android/engine-release-out
engine/build.sh --build                # build the pinned release from its sources instead
```

The download is refused unless the executable's SHA-256 is the pinned one and its build evidence names the pinned commit, ARM64 and a release build. `--build` fetches the pinned tag into `~/.cache/pd2-android/engine/source`, refuses it unless the tag names the pinned commit, and builds it with that release's own container build (Ubuntu 22.04, the release's Rust toolchain, cross-compiled for ARM64; its caches go to `~/.cache/pd2launcher-container-ubuntu-22.04`, `PD2_CONTAINER_CACHE`).

- `engine/build.sh --pin v0.2.0` moves the pin to another release: it reads the commit the tag names, downloads the release's ARM64 executable, checks it against the release's `.sha256` file and writes tag, commit and SHA-256 into `engine.lock`.
- `PD2_ENGINE_SOURCE=<folder> engine/build.sh` builds from a local checkout of the engine instead (its committed `HEAD`; uncommitted changes are refused). Set the same variable for step 3, which reads its pins from the engine's sources.
- `engine/build.sh --source` prints the folder of the engine's sources the other steps use.

## 2. The App userland

```sh
PD2_USERLAND_WITH_GLIBC=1 PD2_USERLAND_WITH_FEX=1 userland/build.sh
```

The programs are built for the fixed prefix `/data/data/com.pd2launcher.app/files/userland/current`. Both switches are needed for a working app: without them the tree keeps Debian's glibc and has no FEX. For the development build add `PD2_USERLAND_VARIANT=debug`: the same build for `/data/data/com.pd2launcher.app.dev/files/userland/current`, with its own build folders and outputs (`userland/out-debug/`, `app/src/debug/assets/userland/`) and X display `:1` instead of `:0`, so both apps can run at once. `build.sh` builds it by itself when a debug build finds none.

`userland/build.sh` runs these steps, each in a container:

| Script | What it does |
| --- | --- |
| `fetch-debian.sh` | Downloads the Debian 13 ARM64 packages `packages.lock` pins from snapshot.debian.org and extracts them, without maintainer scripts, into the staging tree and the cross-build sysroot. |
| `build-xvfb.sh` | Cross-builds Xvfb from xorg-server 21.1.16 with `patches/xserver/` and the device's compiled-in paths. |
| `build-turnip.sh` | Cross-builds Mesa 26.2.3's Turnip (KGSL backend) with `patches/mesa/`. |
| `build-panvk.sh` | Cross-builds Mesa's PanVK at its own pin with `patches/mesa/`, `patches/mesa-panvk/` and our kbase backend (`mesa-kbase/`, with Arm's kbase uAPI headers at a pinned commit). |
| `build-glibc.sh` | With `PD2_USERLAND_WITH_GLIBC=1`: our glibc (`glibc/build.sh`, see `userland/glibc/README.md`) replaces Debian's `libc6`. |
| `build-fex.sh` | With `PD2_USERLAND_WITH_FEX=1`: our FEX-Emu with its thunks (`fex/build.sh`, see `userland/fex/README.md`). |
| `build-entry.sh`, `build-relay.sh` | Cross-build `pd2-fex` (`fex-entry.c`) and `x-input-relay` (`x-input-relay.c`). |
| `relocate.sh` | Prunes documentation and translations, makes symlinks relative, points every ELF's interpreter and run paths at the prefix, and writes `etc/` (with `userland.env` from `userland.env.in`). |
| `pack.sh` | Writes `userland.tar.zst` (sorted, mtime 0) and `manifest.json` (every file's SHA-256 and a version hashed from the build inputs) and copies both into the app's assets. |
| `check-tree.sh` | Unpacks the archive and checks interpreters, symlinks, each Vulkan driver and the libraries it needs, the manifest, and the glibc and FEX records against their patches. |

- Pins: `userland/config.sh` (base image, Debian snapshot, the Mesa, PanVK, kbase uAPI and X server sources), `userland/packages.lock`, `userland/glibc/build.sh`, `userland/fex/build.sh` and the container files.
- `userland/build.sh --resolve` re-resolves `packages.txt` into `packages.lock` against the snapshot (only when changing the package list).
- Caches: `~/.cache/pd2-android/userland/` (`PD2_USERLAND_CACHE`). glibc's and FEX's builds are reused from `userland/glibc/out` and `userland/fex/out` (`out-debug` for the development build) while their recorded inputs and prefix match.

## 3. The runtime

```sh
runtime/pack.sh
```

Downloads FEX's Ubuntu 24.04 RootFS, Wine-GE 8-26, Wine Gecko, Wine Mono and the .NET 6 Desktop Runtime installer at the versions, sizes and hashes the engine pins in its sources (the release from step 1, or `PD2_ENGINE_SOURCE`), trims the RootFS by `runtime/rootfs-trim.txt`, lays everything out as the engine keeps it, and writes `runtime.tar.zst` and `runtime.info` into the app's assets. Downloads and unpacked trees are cached in `~/.cache/pd2-android/guest/`.

## 4. The app

```sh
build.sh            # debug APK: build/outputs/app-debug.apk
build.sh release    # signed release APK: build/outputs/app-release.apk
build.sh check      # compile the debug APK without engine, userland or runtime, and run the unit-test task
```

`build.sh` builds the image `localhost/pd2-android-build:<hash>` from `containers/Containerfile` (Debian 13, Temurin JDK 21, the Android SDK command-line tools, platforms 28 and 36, build-tools 36.0.0, NDK r27c, CMake 3.22.1) and runs Gradle inside it with the repository mounted. Gradle is pinned by the wrapper (`gradle/wrapper/gradle-wrapper.properties`, with its SHA-256), the plugins in `build.gradle.kts` and the libraries in `app/build.gradle.kts`.

- It copies the newest `pd2-launcher-*-aarch64` from `~/.cache/pd2-android/engine-out` (`engine-release-out` for a release) into the APK as `lib/arm64-v8a/libpd2launcher.so`; `PD2_ENGINE_BINARY` names another one. Without an engine the APK cannot start a session.
- The userland and runtime archives come from the assets folders that steps 2 and 3 filled.
- Caches live in `~/.cache/pd2-android/` (`PD2_ANDROID_CACHE`): Gradle's and Maven's, and `debug.keystore`, the debug signing key, so debug builds install over each other.
- The unit-test task has nothing to run here: the test suites are not part of this repository.

Install a build with `adb install -r build/outputs/app-debug.apk`; the development build starts with `adb shell am start -n com.pd2launcher.app.dev/com.pd2launcher.app.MainActivity`.

## Signing a release

`build.sh release` signs with your own key and refuses to build without one. Keep the key outside the repository, by default in `~/.config/pd2launcher/` (`PD2_RELEASE_SIGNING_DIR` names another folder), which `build.sh` mounts read-only into the container:

```sh
mkdir -p ~/.config/pd2launcher && cd ~/.config/pd2launcher
keytool -genkeypair -keystore release.jks -storetype PKCS12 -alias release \
  -keyalg RSA -keysize 4096 -validity 10950 -dname "CN=Your name"
cat > release-signing.properties <<'EOF'
storeFile=release.jks
storePassword=<the keystore password>
keyAlias=release
keyPassword=<the key password>
EOF
chmod 600 release.jks release-signing.properties
```

`storeFile` is relative to the properties file. Back the folder up offline: Android updates an installed app only with an APK signed by the same key. The release build signs with APK Signature Scheme v2 and checks the APK with `apksigner verify` and `zipalign -c`.

## Versions

`versionName` in `app/build.gradle.kts` is `MAJOR.MINOR.PATCH` or `MAJOR.MINOR.PATCH-betaN`; the `versionCode` is computed from it, so every later version has a higher code (0.1.0-beta1 is 10001, 0.1.0 is 10099).

## Continuous integration

- `.github/workflows/ci.yml`, on every push to `main` and every pull request: `build.sh check`.
- `.github/workflows/release.yml`, on a push to `main` whose commit carries a tag `vX.Y.Z` or `vX.Y.Z-betaN` (or `workflow_dispatch` for a given tag): builds the userland, the runtime and the release APK with the engine download, signs it with the repository's release key and publishes the APK and its SHA-256 on the tag's release. The userland, the runtime, FEX's and glibc's builds and the Debian downloads are cached under keys made from their inputs. The key comes from the secrets `ANDROID_KEYSTORE_BASE64` (the PKCS12 keystore, base64), `ANDROID_KEYSTORE_PASSWORD` (its password, also the key's) and `ANDROID_KEY_ALIAS`.
