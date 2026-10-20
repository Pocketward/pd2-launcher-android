# How PD2 Launcher for Android works

PD2 Launcher for Android runs Project Diablo 2, a 32-bit Windows game, on ARM64 Android devices without root. It carries the stack the desktop PD2 Launcher uses on ARM64 Linux inside the app, adapted to Android:

```
Android app (Kotlin, Jetpack Compose)          native screens, input, the game's picture
 └─ session service (foreground)               owns every process below
     ├─ PD2 Launcher engine (Rust, ARM64)      installs, verifies, updates, launches
     └─ game environment, for Install and Play only
         ├─ Xvfb                               the X screen
         ├─ PulseAudio                         sound
         └─ Wine 11.0 (ARM64) ─ Diablo II + Project Diablo 2
               ├─ Box64's wowbox64.dll: the 32-bit x86 code
               └─ OpenGL ─ Zink ─ Turnip (Adreno) or PanVK (Mali)
```

[`BUILDING.md`](BUILDING.md) says how to build each part.

## The app (`app/`)

A Kotlin app with Jetpack Compose and one activity, `com.pd2launcher.app` (debug builds: `com.pd2launcher.app.dev`). It only runs Project Diablo 2. The manifest lets it turn (`fullUser`); the app holds the launcher screens in portrait on a phone and in landscape on a handheld (`FormFactor`), and the Game view always in landscape.

- **minSdk 33, targetSdk 28:** our glibc answers the system calls Android 13's app seccomp policy blocks, and Android 11 and 12 block more. Android lets an app run programs from its own data folder only up to target 28, so the app targets it, below its minimum.
- **Any data folder:** the Linux programs are built for a placeholder data folder, which the app rewrites to its own as it unpacks them, so any user, work profile or storage works. Before every engine start it ends the app's other processes and checks the loader, `userland.env` and the libraries the engine links against the APK's manifest; libraries loaded later with `dlopen` and the rest of the tree are not checked.
- **First-start check** (`DeviceCheck.kt`): the GPU (an Adreno read from `/dev/kgsl-3d0` by `kgsl_probe.c`, or a CSF Mali on `/dev/mali0` read by `mali_probe.c`), 4 KB pages and free space. It does not look at the kernel version, which only goes into its log line.
- **Screens** (`ui/`): Check, the Diablo II source, Download, Install, Play, Settings and About, for touch and controller; one column on phones, two panes on squarer handhelds.
- **The player's files:** characters live in `files/userdata/Save`; the game folder's `Save` is a link to it (`SaveKeeper.kt`), so reinstalling or updating never touches them. `GameFilesProvider.kt` shows the game folder in Android's Files app.
- **App updates** (`AppUpdater.kt`, `AppUpdate.kt`): once a day, while Settings → App allows it, the app asks GitHub's API for this repository's releases and takes the highest newer version that is not a pre-release. About downloads its APK into the app's own folder, checks its size, its SHA-256 (the release's `.sha256` and GitHub's digest) and its one signing certificate (the release key's), and hands it to Android's installer, which asks the player. Only while no game, official launcher or job runs: Android ends all of the app's processes when it replaces the app.

## The session service

`SessionService.kt`, a foreground service, owns every process: the engine always, the game environment only while Install or Play needs it.

- Programs start through a small native spawner (`spawner.c`, `Spawner.kt`): a new session, only the given descriptors, the userland's own dynamic loader. Each program's output goes to a bounded log in `files/logs`.
- When the engine exits unexpectedly, the service ends everything it started (`wineserver -k`, then every remaining process of the app's user ID) before starting it again, so no stale Wine process holds the installation.
- When the app's process dies, Android ends its processes too; on the next start the engine reads its installation records and offers Repair or Start over.

## The engine and the Control channel

The engine is the desktop PD2 Launcher ([Pocketward/pd2-launcher](https://github.com/Pocketward/pd2-launcher)) at the release `engine.lock` pins, shipped as `lib/arm64-v8a/libpd2launcher.so` so Android extracts it executable. The service runs it as `ld-linux-aarch64.so.1 libpd2launcher.so --engine`. It downloads and verifies every file against its pins, sets up Wine, runs Blizzard's and Project Diablo 2's installers unattended, audits the installation, applies updates and launches the game, treating the device as its own host ("Android ARM64") with host facts the app collects.

The app and the engine talk over the **Control channel** (`control/`): a socket pair whose engine end is the engine's standard input, carrying length-prefixed JSON messages with a protocol version. The app sends commands; the engine sends a snapshot of its state whenever it changes and a stable reason code for any command it refuses. When the service's end closes, the engine stops safely and exits. CD keys travel only over this channel, never on a command line or in a file.

## The App userland (`userland/`)

The ARM64 Linux programs the app runs natively, packed as `userland.tar.zst` and unpacked once into the app's folder (staging folder, record written last, then renamed into place):

- a pinned subset of Debian 13 ARM64 (`packages.txt`, `packages.lock`): PulseAudio, the X11/XCB, FreeType and GnuTLS libraries the Wine's Unix side loads, the Vulkan loader, glvnd, fonts, CA certificates;
- **our glibc** (`glibc/`): Debian 13's glibc patched so it never makes the system calls Android's app seccomp policy kills a process for, and reads its `/etc` files from the prefix;
- **Xvfb** from xorg-server 21.1.25 (`patches/xserver/`), with the app's compiled-in paths;
- **Mesa 26.2.3** (`build-mesa.sh`, `patches/mesa/`): Zink, GLX and EGL, and Turnip (KGSL) for Adreno; and **Mesa PanVK** with our kbase backend (`mesa-kbase/`, `patches/mesa-panvk/`) for Mali. The unpack leaves out the other GPU family's Vulkan driver;
- `x-input-relay` (`x-input-relay.c`), which turns the app's touch and keyboard events into X input through XTEST.

`etc/userland.env` (from `userland.env.in`) lists the environment and command lines the service uses.

## The runtime (`runtime/`)

The Windows side ships in the APK too, laid out under the engine's managed root as the engine names it (`pack.py`), so the engine's Download fetches only Blizzard's installers and Project Diablo 2:

- **Wine 11.0** built for ARM64 in WoW64 mode (`wine.sh`, `patches/wine/`), reduced to the modules the game, the official launcher and the install chain load (`wine-keep.txt`, `runtime-cut.txt`);
- **Box64's `wowbox64.dll`** (`wowbox64.sh`, `patches/box64/`), which Wine loads as its x86 CPU DLL for the 32-bit programs;
- a **prefix template** that the same Wine makes at build time (`prefix.sh`, under qemu-user); a new prefix on the device is a copy of it, so no `wineboot` runs on the phone. Most of its files are links into the Wine;
- the **x86 .NET 6 runtime** for Project Diablo 2's own launcher (`dotnet.sh`), which the prefix reaches through `C:\Program Files (x86)\dotnet`.

Wine's programs are built for the placeholder data folder like the userland; the app writes its own data folder into them as it unpacks the runtime. There is no Wine Gecko and no Wine Mono: no program the Launcher runs needs them.

## Wine and Box64

- The engine runs Wine's ARM64 programs directly; Wine runs the game's, the installers' and the official launcher's 32-bit x86 code in Box64's `wowbox64.dll`.
- Box64's recompiler keeps its defaults, except for Project Diablo 2's installer (exact x86 flags) and the official launcher with its updater (strong memory order); our Box64 patch 0006 applies those settings only to the programs they name.
- On Android the engine also installs its XInput bridge as `xinput1_4.dll`, disables `nsiproxy.sys` (Android refuses the netlink socket it needs) and Wine's HID and Bluetooth drivers (the controller goes through the bridge), seeds D2GL's settings, and reaches an imported game folder through drive `G:` on the data partition, because Android's root (Wine's `Z:`) is read-only.
- An update from an app version with the older runtime (Wine-GE under FEX) replaces that runtime and moves the game folder, the Launcher's records and the game's registry keys into a new prefix (`LegacyPrefix.kt`); so does an update whose prefix template has another generation (`.pd2-prefix-generation`, `runtime/config.sh`'s `PREFIX_GENERATION`).

## Graphics

```
PD2 (32-bit OpenGL, D2GL) → Wine's OpenGL (ARM64) → Mesa: Zink (OpenGL on Vulkan)
   → Turnip over KGSL                 on Adreno
   → PanVK over Arm's kbase driver    on Mali
```

- Turnip runs with `TU_DEBUG=sysmem` (passes straight to memory suit the game's many small quads); the Compatibility setting adds `noubwc,nolrz`.
- PanVK is written for Linux's panthor driver; Android devices run Arm's kbase driver, so `mesa-kbase/` adds a kbase backend behind Mesa's `pan_kmod` interface and the PanVK patches use it.
- The game resolution and D2GL's graphics profile default by GPU: fewer lines and the Performance profile on older and entry-level Adrenos and on Mali. DirectDraw (cnc-ddraw, GDI) is the fallback renderer.

## The X screen and the game's picture

- **Xvfb** runs at the device's landscape size or the chosen game resolution, with its framebuffer in a memfd the app passes as descriptor 3 (`-fbfd 3`): never on flash. Without DRI3, Mesa presents through its software X11 path (`MESA_VK_WSI_DEBUG=sw`); our Mesa patches read the frame from cached memory, count the game's presents and pace them by the Game view.
- **The Game view** (`ui/GameView.kt`, `FrameCopy.kt`, `frame_copy.c`) maps that framebuffer and copies each new picture into an Android surface once per display frame, scaled to fill the screen.
- **Direct present** (`PresentServer.kt`, `present_server.cpp`, Mesa patch 0004): while the game's full-screen window is alone on the X screen, Mesa renders into AHardwareBuffers the app allocates and the Game view shows them on a layer above the copy, with no Xvfb or CPU copy. `x-input-relay` tells the app when a window covers the game; then the game presents to Xvfb again.
- The Install screen shows a small read-only view of the same framebuffer.

## Input and sound

- **Controller:** the app writes the pad's state into a shared-memory file (`PD2_GAMEPAD_FILE`) that the engine's XInput bridge reads, so the game sees an Xbox controller. On-screen controls are a virtual controller merged into the same file (`input/`).
- **Touch and keyboard** go to `x-input-relay` over a socket as fixed-size events, replayed into the X screen through XTEST.
- **Sound:** PulseAudio writes into a FIFO that the app plays with an Android audio track (`GameAudio.kt`) at the game's sample rate.

## Releases

`versionName` (`app/build.gradle.kts`) is `MAJOR.MINOR.PATCH` or `MAJOR.MINOR.PATCH-betaN` and the `versionCode` follows from it. Each release is one commit whose message is its version; its APK is signed with the project's release key, so Android updates it only with an APK signed by the same key. A release build carries no pre-filled CD keys and no debug entry points.
