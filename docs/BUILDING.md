# Building PD2 Launcher for Android

Everything is built in pinned containers; only the container engine and a few file tools run on your computer. The APK is assembled from four parts, built in this order (paths are relative to the repository's root):

| Part | Script | Output |
| --- | --- | --- |
| The PD2 Launcher engine (ARM64) | `engine/build.sh` | `pd2-launcher-<version>-aarch64` |
| The App userland | `userland/build.sh` | `app/src/main/assets/userland/` |
| The runtime (Wine, Box64's WowBox64, the prefix template, .NET) | `runtime/build.sh` | `app/src/main/assets/runtime/` |
| The app | `build.sh` | `build/outputs/app-debug.apk` or `app-release.apk` |

None of these outputs is in the repository. A release build is the app `com.pd2launcher.app`, "PD2 Launcher"; a debug build is a separate development app, `com.pd2launcher.app.dev3`, "PD2 Launcher Dev 0.3", which installs beside it with its own data, and also beside the 0.2 line's development app (`com.pd2launcher.app.dev`, "PD2 Launcher Dev"). To test an update in place from that development app, build with `PD2_DEV_AS_FEX_DEV=1 ./build.sh` (the Gradle property `pd2.asFexDev=true`): the debug build then is `com.pd2launcher.app.dev`, "PD2 Launcher Dev", and installs over it when both builds have the same debug key (`debug.keystore` in the cache below). The App userland and the runtime are built for a placeholder data folder, which each app rewrites to its own as it unpacks them, so both apps carry the same ones.

## What you need

- An x86-64 Linux computer with Podman (or Docker), Git, curl, tar, zstd, `sha256sum`/`sha512sum`, `xxd`, `xz` and `readelf` (binutils).
- A static `qemu-aarch64` binfmt_misc handler with the `F` flag (Debian/Ubuntu: `qemu-user-static`; Arch: `qemu-user-static` and `qemu-user-static-binfmt`). The userland build runs ARM64 tools under it, the runtime build makes the prefix template with the ARM64 Wine under it, and the engine build runs the built engine briefly to audit it.
- About 30 GB of free disk space for the build images, caches and outputs, and a good connection for the first build.

Every download is checked against a SHA-256 (or SHA-512) in the scripts, the lock files or the container definitions; a mismatch stops the build. The Android SDK packages `sdkmanager` installs in the app's build image (platforms, build-tools, CMake) are checked by `sdkmanager` against the SHA-1 in Google's repository index.

## 1. The engine

The engine is the desktop PD2 Launcher's standalone ARM64 executable, from a release of [Pocketward/pd2-launcher](https://github.com/Pocketward/pd2-launcher). `engine.lock` pins the release by its URL, its tag and the commit the tag must name, and names its ARM64 executable (`asset`) with its SHA-256.

```sh
engine/build.sh                        # download into ~/.cache/pd2-android/engine-out
PD2_RELEASE_BUILD=1 engine/build.sh    # for a release APK: into ~/.cache/pd2-android/engine-release-out
engine/build.sh --build                # build the pinned release from its sources instead
```

The download is refused unless the executable's SHA-256 is the pinned one and its build evidence names the pinned commit, ARM64 and a release build. `--build` fetches the pinned tag into `~/.cache/pd2-android/engine/source`, refuses it unless the tag names the pinned commit, and builds it with that release's own container build (Ubuntu 22.04, the release's Rust toolchain, cross-compiled for ARM64; its caches go to `~/.cache/pd2launcher-container-ubuntu-22.04`, `PD2_CONTAINER_CACHE`). `build.sh release` takes only the executable `engine.lock` names, checked again, and never reads `PD2_ENGINE_BINARY`, so an engine built with `--build` can go into a debug APK but not into a release.

- `engine/build.sh --pin v0.2.0` moves the pin to another release: it reads the commit the tag names, downloads the release's ARM64 executable, checks it against the release's `.sha256` file and writes tag, commit and SHA-256 into `engine.lock`.
- `PD2_ENGINE_SOURCE=<folder> engine/build.sh` builds from a local checkout of the engine instead (its committed `HEAD`; uncommitted changes are refused). Set the same variable for step 3, which takes the engine's XInput bridge from its sources.
- `engine/build.sh --source` prints the folder of the engine's sources the other steps use.

## 2. The App userland

```sh
PD2_USERLAND_WITH_GLIBC=1 userland/build.sh
```

The programs are built for a 200-byte placeholder data folder (`DATA_PLACEHOLDER` in `userland/config.sh`); the app rewrites it to its own data folder as it unpacks the tree (in place in binaries, padded with `/`), so the release app and the development build carry the same userland. The switch is needed for a working app: without it the tree keeps Debian's glibc. `build.sh` builds it by itself when a debug build finds none.

`userland/build.sh` runs these steps, each in a container:

| Script | What it does |
| --- | --- |
| `fetch-debian.sh` | Downloads the Debian 13 ARM64 packages `packages.lock` pins from snapshot.debian.org and extracts them, without maintainer scripts, into the staging tree and the cross-build sysroot. |
| `build-xvfb.sh` | Cross-builds Xvfb from xorg-server 21.1.25 with `patches/xserver/` and the device's compiled-in paths. |
| `build-mesa.sh` | Cross-builds Mesa 26.2.3 (Zink, softpipe, GLX and EGL for glvnd, Turnip with the KGSL backend) with `patches/mesa/`, in its own image (`containers/Containerfile.mesa`). |
| `build-panvk.sh` | Cross-builds Mesa's PanVK at its own pin with the WSI patches of `patches/mesa/`, `patches/mesa-panvk/` and our kbase backend (`mesa-kbase/`, with Arm's kbase uAPI headers at a pinned commit). |
| `build-glibc.sh` | With `PD2_USERLAND_WITH_GLIBC=1`: our glibc (`glibc/build.sh`, see `userland/glibc/README.md`) replaces Debian's `libc6`. |
| `build-relay.sh` | Cross-builds `x-input-relay` (`x-input-relay.c`). |
| `relocate.sh` | Prunes documentation and translations, makes symlinks relative, points every ELF's interpreter and run paths at the prefix, and writes `etc/` (with `userland.env` from `userland.env.in`). |
| `pack.sh` | Writes `userland.tar.zst` (sorted, mtime 0) and `manifest.json` (every file's SHA-256, where each file holds the placeholder, the critical files the app checks before every engine start, and a version hashed from the build inputs) and copies both into the app's assets. |
| `check-tree.sh` | Unpacks the archive and checks interpreters, symlinks, each Vulkan driver and the libraries it needs, the manifest (with the placeholder and the critical files), and the glibc record against its patches; then rewrites the tree as the app does and runs a few of its programs there (qemu-aarch64). |

- Pins: `userland/config.sh` (base image, Debian snapshot, the Mesa, PanVK, kbase uAPI and X server sources), `userland/packages.lock`, `userland/glibc/build.sh` and the container files.
- `userland/build.sh --resolve` re-resolves `packages.txt` into `packages.lock` against the snapshot (only when changing the package list).
- Caches: `~/.cache/pd2-android/userland/` (`PD2_USERLAND_CACHE`). Each build is reused while its recorded inputs match: glibc's from `userland/glibc/out`, Xvfb's and Mesa's from the cache's `xvfb-out/` and `mesa-out/`, and PanVK's from `~/.cache/pd2-android/kbase/out/` (`PD2_PANVK_CACHE`). With `PD2_IMAGE_ARCHIVE` set to a folder, a missing build image is loaded from its archive there and a newly built one is archived there.

## 3. The runtime

```sh
runtime/build.sh
```

Needs the App userland of step 2 (its relocated tree in the userland cache). `runtime/build.sh` runs these steps, each in a container, and keeps each finished part in `~/.cache/pd2-android/runtime/parts/` (`PD2_RUNTIME_CACHE`) with the identity of its inputs, so an unchanged part is not built again:

| Script | What it does |
| --- | --- |
| `wine.sh` | Cross-builds Wine 11.0 for ARM64 in WoW64 mode (aarch64 and i386 PE sides, llvm-mingw) with `patches/wine/`. |
| `wowbox64.sh` | Builds Box64's `wowbox64.dll`, Wine's x86 CPU DLL, at the pinned commit with `patches/box64/`. |
| `prefix.sh` | Makes the prefix template with that Wine under qemu-user, links its copies of Wine's files into the runtime's Wine (`prefix-dedupe.py`), and puts in the engine's XInput bridge (from its sources), the link to .NET and the template's generation (`.pd2-prefix-generation`, `runtime/config.sh`'s `PREFIX_GENERATION`). |
| `dotnet.sh` | Unpacks the .NET 6 runtime and Windows Desktop runtime, x86. |
| `trim.py`, `stage-wineb.py`, `pack.py` | Reduce the Wine to `wine-keep.txt`, point its programs at the userland's loader, leave out what `runtime-cut.txt` names, and write `runtime.tar.zst` and `runtime.info` (the layout under the Launcher's managed root, and the files the app relocates) into the app's assets. |

- Pins: `runtime/config.sh` (Wine, llvm-mingw, Box64, .NET, the XInput bridge) and the container files; the Debian pins are the userland's.

## 4. The app

```sh
build.sh            # debug APK: build/outputs/app-debug.apk
build.sh release    # signed release APK: build/outputs/app-release.apk
build.sh check      # compile the debug APK without engine, userland or runtime
```

`build.sh` builds the image `localhost/pd2-android-build:<hash>` from `containers/Containerfile` (Debian 13, Temurin JDK 21, the Android SDK command-line tools, platforms 28 and 36, build-tools 36.0.0, NDK r27c, CMake 3.22.1) and runs Gradle inside it with the repository mounted. Gradle is pinned by the wrapper (`gradle/wrapper/gradle-wrapper.properties`, with its SHA-256), the plugins in `build.gradle.kts` and the libraries in `app/build.gradle.kts`, and checks every plugin, library and POM it downloads against its SHA-256 in `gradle/verification-metadata.xml`. After changing a plugin or library, `build.sh verification-metadata` writes that file again; review its diff before committing it.

- It copies the newest `pd2-launcher-*-aarch64` from `~/.cache/pd2-android/engine-out` into the APK as `lib/arm64-v8a/libpd2launcher.so`; `PD2_ENGINE_BINARY` names another one. A release copies the pinned executable from `engine-release-out` (step 1) and nothing else. Without an engine the APK cannot start a session.
- The userland and runtime archives come from the assets folders that steps 2 and 3 filled.
- Caches live in `~/.cache/pd2-android/` (`PD2_ANDROID_CACHE`): Gradle's and Maven's, and `debug.keystore`, the debug signing key, so debug builds install over each other.

Install a build with `adb install -r build/outputs/app-debug.apk`; the development build starts with `adb shell am start -n com.pd2launcher.app.dev3/com.pd2launcher.app.MainActivity` (`com.pd2launcher.app.dev` for a build with `PD2_DEV_AS_FEX_DEV=1`).

### CD keys for the development build

A debug build pre-fills the key screen from `files/debug-keys.properties` in the development build's own storage, when that file is there; a release build never reads it, and no APK carries a key. Put your keys in `.env.local` beside `build.sh` (gitignored):

```sh
D2_KEY=<your Diablo II key>
D2_LOD_KEY=<your Lord of Destruction key>
```

Then, with the development build installed and the device connected:

```sh
./debug-keys.sh            # writes the two keys into the development build's storage (adb run-as)
PD2_DEBUG_PACKAGE=com.pd2launcher.app.dev ./debug-keys.sh   # the same for a PD2_DEV_AS_FEX_DEV=1 build
./debug-keys.sh --remove   # deletes them again
```

The keys go to the device on standard input, never on a command line, and are never printed. `PD2_DEBUG_KEYS_FILE` names another file; without either, a linked worktree's `.env.local` is looked for at its checkout's root, then at the main checkout's. Uninstalling the development build removes the file with its data; installing a new build over it keeps it.

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
- `.github/workflows/release.yml`, on a push to `main` whose commit carries a tag `vX.Y.Z` or `vX.Y.Z-betaN` (or `workflow_dispatch` for a given tag): builds the userland, the runtime and the release APK with the engine download, signs it with the repository's release key and publishes the APK and its SHA-256 on the tag's release. Every job builds the one commit the tag names, which must be on `main`; the APK's `versionName` must be the tag's, and a release that already has files is not overwritten. The userland is cached under a key made from its inputs; its parts (the glibc, Xvfb, Mesa and PanVK builds, PanVK's host tools and the downloads) and the build images are cached apart, the runtime's parts in its cache (`PD2_RUNTIME_CACHE`). The release key comes from the secrets of the repository's GitHub Environment `release`, which only the job that builds the APK names: `ANDROID_KEYSTORE_BASE64` (the PKCS12 keystore, base64), `ANDROID_KEYSTORE_PASSWORD` (its password, also the key's) and `ANDROID_KEY_ALIAS`.
