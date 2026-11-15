# Building PD2 Launcher for Android

Everything is built in pinned containers; only the container engine and a few file tools run on your computer. The APK is assembled from four parts, built in this order (paths are relative to the repository's root):

| Part | Script | Output |
| --- | --- | --- |
| The PD2 Launcher engine (ARM64) | `engine/build.sh` | `pd2-launcher-<version>-<commit>-aarch64` |
| The App userland | `userland/build.sh` | `app/src/main/assets/userland/` |
| The runtime (Wine, Box64's WowBox64, the two prefix templates, .NET) | `runtime/build.sh` | `app/src/main/assets/runtime/` |
| The app | `build.sh` | `build/outputs/app-debug.apk` or `app-release.apk` |

None of these outputs is in the repository. A release build is the app `com.pd2launcher.app`, "PD2 Launcher"; a debug build is a separate development app, `com.pd2launcher.app.dev3`, "PD2 Launcher Dev 0.3", which installs beside it with its own data, and also beside the 0.2 line's development app (`com.pd2launcher.app.dev`, "PD2 Launcher Dev"). To test an update in place from that development app, build with `PD2_DEV_AS_FEX_DEV=1 ./build.sh` (the Gradle property `pd2.asFexDev=true`): the debug build then is `com.pd2launcher.app.dev`, "PD2 Launcher Dev", and installs over it when both builds have the same debug key (`debug.keystore` in the cache below). The App userland and the runtime are built for a placeholder data folder, which each app rewrites to its own as it unpacks them, so both apps carry the same ones.

## What you need

- An x86-64 Linux computer with Podman (or Docker), Git, curl, tar, zstd, `sha256sum`/`sha512sum`, `xxd`, `xz`, `readelf` (binutils) and `flock` (util-linux).
- A static `qemu-aarch64` binfmt_misc handler with the `F` flag (Debian/Ubuntu: `qemu-user-static`; Arch: `qemu-user-static` and `qemu-user-static-binfmt`). The userland build runs ARM64 tools under it, the runtime build makes the prefix templates with the ARM64 Wine under it, and the engine build runs the built engine once to check its identity. This is a prerequisite of a local build on an x86 PC only: the release workflow runs these ARM64 steps natively on an arm64 runner (`ubuntu-24.04-arm`), with no qemu.
- About 30 GB of free disk space for the build images, caches and outputs, and a good connection for the first build.

Every download is checked against a SHA-256 (or SHA-512) in the scripts, the lock files or the container definitions; a mismatch stops the build. The Android SDK packages `sdkmanager` installs in the app's build image (platforms, build-tools, CMake) are checked by `sdkmanager` against the SHA-1 in Google's repository index.

## The base image

The engine, the userland, the runtime and the system-driver server build in one image, the base image (`containers/Containerfile.base`): Debian 13 (`userland/config.sh`'s `DEBIAN_IMAGE`) with the compilers and build tools at the exact versions of `containers/base-packages.lock`, rustup-init, the `.deb` files of `userland/packages.lock` in `/opt/pd2/debs`, and Debian's glibc source package that `userland/glibc/build.sh` builds (`2.41-12+deb13u4`, each file checked by SHA-256) in `/opt/pd2/src/glibc`, so a build does not depend on Debian keeping that version. Its apt sources are deb.debian.org and then snapshot.debian.org at `DEBIAN_SNAPSHOT` (the lock files' snapshot), and its downloads take snapshot.debian.org when deb.debian.org no longer has a pinned version; `containers/check-sources.sh` lists those versions (run it before an export). Its name is `ghcr.io/pocketward/pd2-build-base:<tag>`, the tag a hash of those four inputs and `DEBIAN_SNAPSHOT` (`containers/base-image.sh`). A script uses the local image of that name, else pulls it, else builds it. Each part's identity names the image, so a new base image builds the parts again.

## 1. The engine

The engine is the PD2 Launcher, whose sources are in `core/` (the same crate as the [desktop PD2 Launcher](https://github.com/Pocketward/pd2-launcher)), built for ARM64 without its window: `cargo build --release --no-default-features` leaves out its `gui` Cargo feature, so it links no GPUI and no X11 or Wayland library, and runs only as the app's engine. The APK always holds the engine of its own commit.

```sh
engine/build.sh                        # build into ~/.cache/pd2-android/engine-out
PD2_RELEASE_BUILD=1 engine/build.sh    # for a release APK: into ~/.cache/pd2-android/engine-release-out
```

It takes the committed files of `core/` (`ENGINE_FILES` in `engine/build.sh`; uncommitted changes are refused). It builds a copy of the committed files in the base image: Debian 13 with its cross compiler for ARM64 and glibc 2.41, the userland's C library, and rustup, which installs the Rust toolchain that `core/rust-toolchain.toml` names into `~/.cache/pd2-android/engine` (with Cargo's downloads and build folder). The engine may link only the libraries in `ENGINE_NEEDED` (`userland/config.sh`); it gets the standalone ARM64 identity and its build evidence beside it (`<engine>.build-evidence.txt`: the commit, the sources' SHA-256, the features, the libraries it links, the glibc it needs, the toolchain and the image). The build itself runs no ARM64 code, so the evidence first says `package_identity=unattested`; then the engine runs once in an arm64 Debian container to attest its identity (`engine/build.sh --attest`, natively on an ARM64 host or under qemu-user), which a build does by itself where it can, and the evidence says `package_identity=native-aarch64-binary`. `PD2_RELEASE_BUILD=1` adds the `release-build` feature (no dev CD keys, Wine's quiet logs). `build.sh release` takes only an engine whose build evidence names the pinned commit and `source_sha256` and says `release_build=1` and `package_identity=native-aarch64-binary`, and never reads `PD2_ENGINE_BINARY`.

- `engine/build.sh --tree` prints the engine's identity: the SHA-256 of the `git ls-tree` listing of its files. A commit that changes only the app keeps it. `build.sh release` takes only an engine whose build evidence names it.
- `engine/build.sh --source` prints the engine's folder, which step 3 reads the XInput bridge from; `engine/build.sh --image` prints the name of the base image.

## 2. The App userland

```sh
PD2_USERLAND_WITH_GLIBC=1 userland/build.sh
```

The programs are built for a 200-byte placeholder data folder (`DATA_PLACEHOLDER` in `userland/config.sh`); the app rewrites it to its own data folder as it unpacks the tree (in place in binaries, padded with `/`), so the release app and the development build carry the same userland. The switch is needed for a working app: without it the tree keeps Debian's glibc. `build.sh` builds it by itself when a debug build finds none.

`userland/build.sh` runs these steps, each in the base image:

| Script | What it does |
| --- | --- |
| `fetch-debian.sh` | Takes the Debian 13 ARM64 packages `packages.lock` pins from the base image (`/opt/pd2/debs`), checks them again and extracts them, without maintainer scripts, into the staging tree and the cross-build sysroot. |
| `build-xvfb.sh` | Cross-builds Xvfb from xorg-server 21.1.25 with `patches/xserver/` and the device's compiled-in paths. |
| `build-mesa.sh` | Cross-builds Mesa main at `config.sh`'s `MESA_COMMIT` (Zink, softpipe, GLX and EGL for glvnd, Turnip with the KGSL backend) with `patches/mesa/`. `mesa-update.sh` tries a newer commit of main and refreshes the patches. |
| `build-panvk.sh` | Cross-builds Mesa's PanVK from the same Mesa commit (the archive `build-mesa.sh` fetches) with the WSI patches of `patches/mesa/`, `patches/mesa-panvk/` and our kbase backend (`mesa-kbase/`, with Arm's kbase uAPI headers at a pinned commit). |
| `build-sysdrv.sh` | The system-driver mode's parts (the game on the phone's own Vulkan driver, for GPUs that Turnip and PanVK do not drive): Mesa's Venus ICD from the same Mesa commit with `patches/mesa/` and `patches/mesa-sysdrv/` (the Venus protocol headers from `config.sh`'s `VENUS_PROTOCOL_*` release, put into the folder that Mesa's `subprojects/venus-protocol.wrap` names). The mode's OpenGL is `build-mesa.sh`'s Zink. |
| `build-hybris.sh` | The libhybris route for Imagination PowerVR (the phone's Vulkan driver in the game's process): libhybris at `config.sh`'s `LIBHYBRIS_COMMIT` with Halium's android-headers at `HYBRIS_HEADERS_COMMIT` (both GitHub archives checked by SHA-256), `patches/libhybris/` applied with `--fuzz=0`, `hybris/common` only (`libhybris-common.so.1` and the q linker `libhybris/linker/q.so`, its folder compiled in under the userland's prefix), and our ICD `libvulkan_pd2hybris.so` from `hybris/` with its JSON `pd2hybris_icd.aarch64.json`; the licences go to `usr/share/licenses/pd2-libhybris/`. All its files go into the `system-driver` part. |
| `build-radv.sh` | RADV for Samsung's Xclipse GPUs (the RADV route): the community tree radv-xclipse (MIT) at `config.sh`'s `RADV_XC_COMMIT`, with the files of its own glibc build and our WSI patches ported to it (`patches/radv-xclipse/`), and libdrm 2.4.133 linked in statically from the tree's wrap; RADV only, X11, no LLVM, with no network. Its library, ICD and licence files join the system-driver part (after `build-sysdrv.sh`). The source tree and the configured build folder stay in the cache's `work/` for the next run. |
| `build-mali-panvk.sh` | PanVK for the Mali GPUs of Arm's job-manager kernel driver (Bifrost and Valhall v9: the mali-panvk route) from the community tree mali-panvk (MIT) at `config.sh`'s `MALI_PANVK_COMMIT`, beside our PanVK, which it never replaces: the upstream Mesa files it leaves out (`MALI_PANVK_UPSTREAM_FILES`), the stand-ins in `radv-compat/` and the WSI patches of `patches/radv-xclipse/` (its WSI files are the same as radv-xclipse's), then our fixes for the tree in `patches/mali-panvk/` (eleven patches, see its `README.md`), each exactly; host tools from the tree, then PanVK only, X11, no LLVM, with no network. Its library (`usr/lib/aarch64-linux-gnu/mali-panvk/`), its ICD (`usr/share/mali-panvk/`, outside `icd.d`) and its licence files join the system-driver part (after `build-sysdrv.sh`). Its work folders are in `MALI_PANVK_CACHE`. |
| `build-glibc.sh` | With `PD2_USERLAND_WITH_GLIBC=1`: our glibc (`glibc/build.sh`, see `userland/glibc/README.md`) replaces Debian's `libc6`. |
| `build-relay.sh` | Cross-builds `x-input-relay` (`x-input-relay.c`). |
| `relocate.sh` | Prunes documentation and translations, makes symlinks relative, points every ELF's interpreter and run paths at the prefix, and writes `etc/` (with `userland.env` from `userland.env.in`). |
| `pack.sh` | Writes `userland.tar.zst` (sorted, mtime 0) and `manifest.json` (every file's SHA-256, where each file holds the placeholder, the critical files the app checks before every engine start, and a version hashed from the build inputs) and copies both into the app's assets. The files of a part that an APK can leave out (the system-driver mode's, which `build-sysdrv.sh` lists) go into an archive and manifest of their own, copied to `app/system-driver/assets/userland-system-driver/`; the app unpacks the main archive and the parts it carries into one tree. |
| `check-tree.sh` | Unpacks the archive and checks interpreters, symlinks, each Vulkan driver and the libraries it needs, the manifest (with the placeholder and the critical files), and the glibc record against its patches; then rewrites the tree as the app does (`rewrite-placeholder.py`) and runs a few of its programs there (qemu-aarch64). On an ARM64 host, where the base image (an amd64 image) does not run, `check-relocated.sh` does these runs on the host itself, with no container. |

- Pins: `userland/config.sh` (Debian image, Debian snapshot, the Mesa, PanVK, kbase uAPI and X server sources), `userland/packages.lock`, `userland/glibc/build.sh` and the base image.
- `userland/build.sh --resolve` re-resolves `packages.txt` into `packages.lock` against the snapshot (only when changing the package list).
- Caches: `~/.cache/pd2-android/userland/` (`PD2_USERLAND_CACHE`). Each build is reused while its recorded inputs match: glibc's from `userland/glibc/out`, Xvfb's and Mesa's from the cache's `xvfb-out/` and `mesa-out/`, and PanVK's from `~/.cache/pd2-android/kbase/out/` (`PD2_PANVK_CACHE`).

### Moving the Mesa pin

Every build uses one fixed commit of Mesa's main branch (`MESA_COMMIT`, with the SHA-256 of GitLab's archive of it and the tree's `VERSION`); a release keeps the commit its tests ran on.

1. `userland/mesa-update.sh` tries the newest commit of main (or `--commit <full commit id>`): it applies every patch (`patches/mesa/`, `patches/mesa-panvk/` and the Venus ICD's `patches/mesa-sysdrv/`) to that commit with `--fuzz=0` and prints `exact`, `offsets` or `FAILS` per patch; it writes nothing. A patch that fails is rebased by hand (the scratch tree keeps the `.rej` files); then run the script again with the same `--commit`.
2. `userland/mesa-update.sh --commit <full commit id> --write` writes the pin into `config.sh` and refreshes every patch, only when all of them apply.
3. `mesa-update.sh` also refuses a commit whose `subprojects/venus-protocol.wrap` names another release than `config.sh`'s `VENUS_PROTOCOL_VERSION` (move it and `devdriver/build.sh`'s `VPROTO_*` together, so the Venus ICD and the server speak one protocol). Read the upstream changes to the files the patches touch, compare Mesa's GPU tables with the app's (`freedreno_devices.py` with `KgslProbe.kt`, `pan_model.c` with `MaliProbe.kt`) and with the README's "Supported graphics chips" table, and check that the values Mesa gets still exist: those of `userland.env.in` (`TU_DEBUG` options and the other variables) and the app's own `TU_DEBUG` value (`noconform`, `SessionPaths.kt`). Also compare Zink's required Vulkan features (`zink_device_info.py`, `required=True`, and `check_base_requirements` in `zink_screen.c`) with what Turnip offers on the Adreno 6xx: a new requirement can take the game's OpenGL away from a GPU. The system-driver mode's probe (`system_driver_probe.c`, `SystemDriverReport`) checks the same needs of the product Zink on the phone's own driver: change it with them. It also follows Zink's checks of Imagination's own driver (`zink_screen.c`: Mesa 25.1.9 fails without geometry shaders, main warns below DDK 24.1, `SystemDriverGpu.powerVrWithoutGeometryShaders` and `venusLowConfidence`): change them with those checks.
4. Gates before a release: the userland and the app build, with their JVM and host tests; on one device of each GPU family the app supports (Adreno 7xx, Adreno 6xx without hardware multiview, Adreno 650, Mali on PanVK), Play reaches PD2's menu with the game's sound, `files/logs/engine.log` has `MESA: direct present: <size>`, the presents per second in logcat (`pd2-present`) hold, and nothing crashes for a few minutes; in the game, a town and a fight show a correct picture; the GPU time per frame at the menu stays within about 5 % of the pin before (a debug build gives the `NAME=value` lines of its `files/kbase/hostenv`, for example `MESA_GPU_TRACES=print_csv` and `MESA_GPU_TRACEFILE=<file>`, to the game's processes); and the candidate installs over the release before it with the game, the settings and the characters kept. The system-driver mode (its Venus ICD moves with the pin): with a debug build's forced route (`files/debug/system-driver`), Play reaches PD2's menu on one Adreno and one Mali with their own drivers.
5. One commit moves the pin (`config.sh` and the patches), so one `git revert` takes it back.

## 3. The runtime

```sh
runtime/build.sh
```

Needs the App userland of step 2 (its relocated tree in the userland cache). `runtime/build.sh` runs these steps, each in the base image (the ARM64 Wine in an ARM64 Debian container), and keeps each finished part in `~/.cache/pd2-android/runtime/parts/` (`PD2_RUNTIME_CACHE`) with the identity of its inputs, so an unchanged part is not built again:

| Script | What it does |
| --- | --- |
| `wine.sh` | Cross-builds Wine 11.0 for ARM64 in WoW64 mode (aarch64 and i386 PE sides, llvm-mingw) with `patches/wine/`. |
| `wowbox64.sh` | Builds Box64's `wowbox64.dll`, Wine's x86 CPU DLL, at the pinned commit with `patches/box64/`. |
| `pd2audio.sh` | Builds our ALSA plugin (`pd2-audio/pcm_pd2.c`, the game's sound into the app's ring) and takes Debian's `libasound.so.2`; both go beside `winealsa.so`. |
| `prefix.sh` | Makes the PLAIN prefix template with that Wine under qemu-user, links its copies of Wine's files into the runtime's Wine (`prefix-dedupe.py`), and puts in the engine's XInput bridge (from its sources), the link to .NET, the template's generation (`.pd2-prefix-generation`, `runtime/config.sh`'s `PREFIX_GENERATION`) and its kind (`.pd2-prefix-template`). Then the GOLDEN template (`golden.py`): the PLAIN one with `golden.reg` written into its hives and the files of `golden/files.txt`, which `cabextract` takes from the two pinned Visual C++ redist installers without running them, each checked by its SHA-256; Wine then reads three of its keys in a copy, under qemu-user. Its steps between the two Wine runs need no particular CPU (Python's standard library, `patchelf`, `strip`, `cabextract`): they run in the base image, or on an ARM64 host on the host itself, where Wine runs natively. |
| `dotnet.sh` | Unpacks the .NET 6 runtime and Windows Desktop runtime, x86. |
| `trim.py`, `stage-wineb.py`, `pack.py` | Reduce the Wine to `wine-keep.txt`, point its programs at the userland's loader, leave out what `runtime-cut.txt` names, and write `runtime.tar.zst` and `runtime.info` (the layout under the Launcher's managed root, and the files the app relocates) into the app's assets. |

- Pins: `runtime/config.sh` (Wine, llvm-mingw, Box64, .NET, the XInput bridge) and the base image; the Debian pins are the userland's.

## 4. The app

```sh
build.sh            # debug APK: build/outputs/app-debug.apk
build.sh release    # signed release APK: build/outputs/app-release.apk (requires lintVitalRelease)
build.sh check      # compile the debug APK without engine, userland or runtime, and run the
                    # release build's lint check (lintVitalRelease)
```

The release build's lint check also stops at a call of an API that Android has only above the app's minSdk 30 (lint's `NewApi`, made fatal in `app/build.gradle.kts`): such a call (`java.nio.file.Path.of` is API 34, for one) builds, and then fails on the older phones with `NoSuchMethodError`.

`build.sh` builds the image `localhost/pd2-android-build:<hash>` from `containers/Containerfile` (Debian 13, Temurin JDK 21, the Android SDK command-line tools, platforms 28 and 36, build-tools 36.0.0, NDK r27c, CMake 3.22.1) and runs Gradle inside it with the repository mounted. Gradle is pinned by the wrapper (`gradle/wrapper/gradle-wrapper.properties`, with its SHA-256), the plugins in `build.gradle.kts` and the libraries in `app/build.gradle.kts`, and checks every plugin, library and POM it downloads against its SHA-256 in `gradle/verification-metadata.xml`. After changing a plugin or library, `build.sh verification-metadata` writes that file again; review its diff before committing it.

- It builds the system-driver mode's server, virglrenderer's vtest and render servers for Android (`devdriver/build.sh`, in the base image with the NDK r27c mounted: `ANDROID_NDK_ROOT` or `ANDROID_HOME/ndk/27.2.12479018` when that is r27c, else the NDK of the app's build image; into `~/.cache/pd2-android/devdriver-out` or `PD2_DEVDRIVER_OUT`) when that folder's build does not match today's sources, and puts them into the APK as native libraries, staged in `app/system-driver/jniLibs`: virglrenderer's vtest and render servers with Venus only for the Venus route (`libvirgl_test_server.so`, `libvirgl_render_server.so`), and a second vtest server with the OpenGL renderer (vrend, on libepoxy, EGL only) for the VirGL route (`libvirgl_test_server_gl.so`), with the userland's system-driver part (`app/system-driver/assets`). `PD2_SYSTEM_DRIVER=0 build.sh` (the Gradle property `pd2.systemDriver=false`) builds the APK without them: the game then has no route to the phone's own driver, and the device check refuses GPUs that the built-in drivers do not support. `build.sh check` does not build the server.
- It copies the newest `pd2-launcher-*-aarch64` from `~/.cache/pd2-android/engine-out` into the APK as `lib/arm64-v8a/libpd2launcher.so`; `PD2_ENGINE_BINARY` names another one. A release copies the newest release build of this commit's engine (its `engine_tree`, step 1) from `engine-release-out` and nothing else. The engine may link only the libraries in `ENGINE_NEEDED` (`userland/config.sh`). Without an engine the APK cannot start a session.
- The userland and runtime archives come from the assets folders that steps 2 and 3 filled.
- The release build goes through R8, which removes the code that the app does not use: Android keeps the app's code in memory while the game runs. `app/proguard-rules.pro` keeps the code that native code finds by name, and the names of all classes and methods, so stack traces show real names. R8 can still move a method to another class, writes line numbers in its own form and names the source file `r8-map-id-<id>`; `app/build/outputs/mapping/release/mapping.txt` of the build whose `# pg_map_id` is that id gives the exact classes and lines back (`retrace`). Keep that file with each release: another build of the same commit can give another mapping. The release workflow puts it on the release beside the APK, as `pd2-launcher-android-<version>-mapping.txt`. `PD2_DEV_R8=1 build.sh` (the Gradle property `pd2.devR8=true`) also sends the debug build through R8, for device tests of the shrunk code. For a debug build, R8 only removes code and does not optimize it, so a release build must also be tested.
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

`storeFile` is relative to the properties file. Back the folder up offline: Android updates an installed app only with an APK signed by the same key. The release build requires `lintVitalRelease` to pass before it verifies the signature with `apksigner verify`, checks alignment with `zipalign -c`, and exports `build/outputs/app-release.apk`. It signs with APK Signature Scheme v2. Tagged and manual release workflows both run this gate before publication.

## Versions

`versionName` in `app/build.gradle.kts` is `MAJOR.MINOR.PATCH` or `MAJOR.MINOR.PATCH-betaN`. A Stable's `versionCode` is computed from its name, so every later Stable has a higher code (0.3.8 is 30899). A beta sets `betaBase`, beside `versionName`, to the Stable it was made after, and has that Stable's code (0.4.0-beta1 made after 0.3.8 is 30899), so the two replace each other in place. The build fails when a beta has no `betaBase`, or a Stable has one. A Stable's APK on a release is `pd2-launcher-android-<version>.apk`; a beta's is `pd2-launcher-android-<version>-<code>.apk` (`apk-name.sh`), because the app reads a beta's code from that name.

## Continuous integration

- `.github/workflows/ci.yml`, on every push to `main` and every pull request: `build.sh check`, including the release lint check, with Gradle on the runner (`PD2_GRADLE_ON_HOST=1`: Temurin 21 and the runner's Android SDK with the pinned packages, which `sdkmanager` installs; `.github/actions/android-sdk`).
- `.github/workflows/build.yml`, a reusable workflow that `release.yml` and `test-build.yml` call: every build job of the APK's parts. Layer 1 is the base image `ghcr.io/pocketward/pd2-build-base:<tag>` (`containers/base-image.sh`); its job builds and pushes it only when no image has that tag, and every other job pulls it. Layer 2: the userland (`userland/build.sh` with our glibc), Wine, `wowbox64.dll` and the sound plugin (`runtime/wine.sh`, `wowbox64.sh`, `pd2audio.sh`), the engine (`PD2_RELEASE_BUILD=1 engine/build.sh`) and the system-driver servers (`devdriver/build.sh`) side by side; then one job on an arm64 runner (`ubuntu-24.04-arm`), where all ARM64 code runs natively in every build: the userland's relocated runs (`userland/check-relocated.sh`), the engine's attest (`engine/build.sh --attest`) and, when the runtime is not in the cache, the prefix templates (`runtime/prefix.sh`); then the runtime (`runtime/build.sh`). The slow parts are packages on ghcr.io, as the base image is: each part (the userland's nine parts and its packed assets, Wine, `wowbox64.dll`, the sound plugin, the prefix templates, the runtime, the engine and the system-driver servers) is a data-only image `ghcr.io/pocketward/pd2-part-<part>:<tag>` (`containers/part-image.sh`), whose tag is made from the part's identity (the text it writes into its BUILDINFO, which `<script> --identity` prints before any build; the userland's assets by `userland/pack.sh --version`). A job pulls a part whose package exists and checks that its BUILDINFO is that identity; it builds a missing part once and pushes it. One changed input builds that part (and the parts made from it) again; the packages do not expire, and there is no keep-alive. Only Gradle's downloads and the engine's toolchain are in the Actions cache. The engine that the arm64 job attested goes to the APK's job as an artifact of the run. A local build needs none of this: the scripts reuse their own output folders.
- `.github/workflows/release.yml`, on a push to `main` whose commit carries a tag `vX.Y.Z` or `vX.Y.Z-betaN` (or `workflow_dispatch` for a given tag): `build.yml` for the one commit the tag names, then the release APK (`PD2_GRADLE_ON_HOST=1 build.sh release`), signed with the repository's release key, and publishes the APK (named by `apk-name.sh`), its SHA-256 and R8's `mapping.txt` of the same build on the tag's release. Every job builds the one commit the tag names, which must be on `main`; the APK's `versionName` must be the tag's, and a release that already has files is not overwritten. With `verify` set (`gh workflow run release.yml --ref main -f tag=vX.Y.Z -f verify=true`), it builds `main`'s head in the same way and keeps the signed APK and its SHA-256 as an artifact of the run for 7 days, with no draft and no release: a check of the workflows without a candidate. The release key comes from the secrets of the repository's GitHub Environment `release`, which only the job that builds the APK names: `ANDROID_KEYSTORE_BASE64` (the PKCS12 keystore, base64), `ANDROID_KEYSTORE_PASSWORD` (its password, also the key's) and `ANDROID_KEY_ALIAS`.
