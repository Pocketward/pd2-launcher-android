# How PD2 Launcher for Android works

PD2 Launcher for Android runs Project Diablo 2, a 32-bit Windows game, on ARM64 Android devices without root. It carries the stack the desktop PD2 Launcher uses on ARM64 Linux inside the app, adapted to Android:

```
Android app (Kotlin, Jetpack Compose)          native screens, input, the game's picture
 └─ session service (foreground)               owns every process below
     ├─ PD2 Launcher engine (Rust, ARM64)      installs, verifies, updates, launches
     └─ game environment, for Install and Play only
         ├─ Xvfb                               the X screen
         └─ Wine 11.0 (ARM64) ─ Diablo II + Project Diablo 2
               ├─ Box64's wowbox64.dll: the 32-bit x86 code
               ├─ ALSA ─ our plugin ─ a ring the app plays (sound)
               └─ OpenGL ─ Zink ─ Turnip (Adreno) or PanVK (Mali)
```

[`BUILDING.md`](BUILDING.md) says how to build each part.

## The app (`app/`)

A Kotlin app with Jetpack Compose and one activity, `com.pd2launcher.app` (debug builds: `com.pd2launcher.app.dev`). It only runs Project Diablo 2. The manifest lets it turn (`fullUser`); the app holds the launcher screens in portrait on a phone and in landscape on a handheld (`FormFactor`), and the Game view always in landscape.

- **minSdk 30, targetSdk 28:** our glibc answers the system calls Android 11's app seccomp policy blocks, which include all that Android 12 and 13 block. A call of a later Android API checks the version first; NDK functions above API 30 are weak references. Android lets an app run programs from its own data folder only up to target 28, so the app targets it, below its minimum.
- **Any data folder:** the Linux programs are built for a placeholder data folder, which the app rewrites to its own as it unpacks them, so any user, work profile or storage works. Before every engine start it ends the app's other processes and checks the loader, `userland.env` and the libraries the engine links against the APK's manifest; libraries loaded later with `dlopen` and the rest of the tree are not checked.
- **First-start check** (`DeviceCheck.kt`): the GPU (an Adreno read from `/dev/kgsl-3d0` by `kgsl_probe.c`, or a CSF Mali on `/dev/mali0` read by `mali_probe.c`), 4 KB pages and free space. It does not look at the kernel version, which only goes into its log line.
- **Screens** (`ui/`): Check, the Diablo II source, Download, Install, Play, Settings and About, for touch and controller; one column on phones, two panes on squarer handhelds.
- **The player's files:** characters live in `files/userdata/Save`; the game folder's `Save` is a link to it (`SaveKeeper.kt`), so reinstalling or updating never touches them. `GameFilesProvider.kt` shows the game folder in Android's Files app.
- **App updates** (`AppUpdater.kt`, `AppUpdate.kt`): each hour while the app is open (at a start, at a return to the front, and on a 5 min timer that sends a request only when a check is due), while Settings → App allows it, the app asks GitHub's API for this repository's releases and takes the highest newer version that is not a pre-release. About downloads its APK into the app's own folder, checks its size, its SHA-256 (the release's `.sha256` and GitHub's digest) and its one signing certificate (the release key's), and hands it to Android's installer, which asks the player. Only while no game, official launcher or job runs: Android ends all of the app's processes when it replaces the app.

## The session service

`SessionService.kt` owns every process: the engine always, the game environment only while Install or Play needs it. It is a foreground service, with a notification, only while something must go on in the background: the game, the official PD2 launcher, an engine job, the unpack of the game support files, the folder copy of an import or an app update's download (`BackgroundWork.kt`). The notification's Close stops what runs as the player would (Stop game, Close launcher, Stop), stops a folder copy for an Import and removes what it copied, and closes the app. A copy that the app's end cut short goes at the next session start. When nothing runs, the service stops at once after the app is closed or swiped away, and 10 s after the app leaves the screen (`SessionIdle.kt`).

- Programs start through a small native spawner (`spawner.c`, `Spawner.kt`): a new session, only the given descriptors, the userland's own dynamic loader. Each program's output goes to a bounded log in `files/logs`.
- Logs have two levels. The baseline is always on: each line of the app's logs has its UTC offset; an engine snapshot line comes only when it changes, with a count of the repeats (`SnapshotLines.kt`); each Play and each press of the official launcher logs every setting (`SettingsLog.kt`); while the game runs, the service reads its present counter and logs each wait of more than 1 s for a new picture, and at the game's end a frame summary (`FrameWatch.kt`); `device.txt` adds the CPU, the memory, the UTC offset, the runtime's pins, the screen, the thermal state and the settings (`HardwareFacts.kt`). **Detailed logging** (Settings → App → Help, off by default, it stays as the player sets it) goes with each Play and each press of the official launcher (`detailed_logging`): the engine then gives the game's and the launcher's Wine processes more debug channels and Box64's log, and keeps only the connects, closes, name lookups and errors of Wine's Winsock channel.
- When the engine exits unexpectedly, the service ends everything it started (`wineserver -k`, then every remaining process of the app's user ID) before starting it again, so no stale Wine process holds the installation.
- When the app's process dies, Android ends its processes too; on the next start the engine reads its installation records and offers Repair or Start over.

## The engine and the Control channel

The engine is the desktop PD2 Launcher ([Pocketward/pd2-launcher](https://github.com/Pocketward/pd2-launcher)) at the release `engine.lock` pins, built without its window (no GPUI, so it links only glibc and libgcc_s), shipped as `lib/arm64-v8a/libpd2launcher.so` so Android extracts it executable. The service runs it as `ld-linux-aarch64.so.1 libpd2launcher.so --engine`. It downloads and verifies every file against its pins, sets up Wine, installs Diablo II and Lord of Destruction itself or with Blizzard's installers ([Installing Diablo II](#installing-diablo-ii)), runs Project Diablo 2's installer unattended, audits the installation, applies updates and launches the game, treating the device as its own host ("Android ARM64") with host facts the app collects.

The app and the engine talk over the **Control channel** (`control/`): a socket pair whose engine end is the engine's standard input, carrying length-prefixed JSON messages with a protocol version. The app sends commands; the engine sends a snapshot of its state whenever it changes and a stable reason code for any command it refuses. When the service's end closes, the engine stops safely and exits. CD keys travel only over this channel, never on a command line or in a file.

## Installing Diablo II

The Diablo II source page (`DiabloSource.kt`, `ui/SourceScreen.kt`) offers three install sources. For a download, the app sends the engine's `install_source` setting; the engine reads it when Install Diablo II starts.

- **Fast download** (the default, `direct_download`): the engine downloads Diablo II and Lord of Destruction from Blizzard and installs them itself, without Wine. It fetches the complete installer files of both games at the same time, piece by piece, from Blizzard's download servers. The torrent inside each verified Blizzard downloader program names these servers and gives the SHA-1 of each piece. The engine checks each piece against it. The two games share one pool of connections: the engine adds connections while the total speed increases, and halves the count when requests fail. After it checks the files, it runs each game's install script into the game folder itself. Blizzard's downloader and installers do not run.
- **Blizzard's installers** (`official`): Blizzard's own downloader and installers run unattended under Wine during Install, with a small live view of their windows.
- **Use files you have:** the player's Diablo II folder, copied from a PC. The app checks its file list, copies it into its own folder (`SourceImport.kt`) and gives it to the engine. When the engine accepts the folder, the app sends `discard_downloads`, which removes what the Launcher's own downloads made.

For a download, Continue starts the Download job (Blizzard's two small downloader programs and Project Diablo 2's files) and opens the key page at once (`KeysWhileDownloading.kt`). Agree and install sends the keys while the Download job runs. The engine holds them and starts Install Diablo II by itself when the Download job ends. The game download starts only then.

**CD keys:** the engine checks both keys before it takes them, as Blizzard's installers check them. A key has 26 characters of the key alphabet; the engine removes dashes and spaces first. The product that each key decodes to must be the product of its field: Diablo II in the first field, Lord of Destruction in the second. The engine refuses 16-character keys. A refusal names the field and the problem, and the key page shows it under that field (`KeyProblems.kt`). With Blizzard's installers, each installer receives its key; with Fast download, the engine writes the key into the game's archive.

## The App userland (`userland/`)

The ARM64 Linux programs the app runs natively, packed as `userland.tar.zst` and unpacked once into the app's folder (staging folder, record written last, then renamed into place):

- a pinned subset of Debian 13 ARM64 (`packages.txt`, `packages.lock`): the X11/XCB, FreeType and GnuTLS libraries the Wine's Unix side loads, the Vulkan loader, glvnd, fonts, CA certificates;
- **our glibc** (`glibc/`): Debian 13's glibc patched so it never makes the system calls Android's app seccomp policy kills a process for, and reads its `/etc` files from the prefix;
- **Xvfb** from xorg-server 21.1.25 (`patches/xserver/`), with the app's compiled-in paths;
- **Mesa main at one commit** (`config.sh` `MESA_COMMIT`; `build-mesa.sh`, `patches/mesa/`, `mesa-update.sh` to move it): Zink, GLX and EGL, and Turnip (KGSL) for Adreno; and **Mesa PanVK** from the same commit with our kbase backend (`mesa-kbase/`, `patches/mesa-panvk/`) for Mali. The unpack leaves out the other GPU family's Vulkan driver;
- `x-input-relay` (`x-input-relay.c`), which turns the app's touch and keyboard events into X input through XTEST, keeps the pointer's moves by a distance inside the area the app gives it (the game's picture), keeps the official PD2 launcher's 800×600 window in the middle of the X screen, and tells the app the X screen's pointer (its image through XFixes, and where it is) while the pointer shows.

`etc/userland.env` (from `userland.env.in`) lists the environment and command lines the service uses.

## The runtime (`runtime/`)

The Windows side ships in the APK too, laid out under the engine's managed root as the engine names it (`pack.py`), so the engine's Download fetches only Blizzard's installers and Project Diablo 2:

- **Wine 11.0** built for ARM64 in WoW64 mode (`wine.sh`, `patches/wine/`), reduced to the modules the game, the official launcher and the install chain load (`wine-keep.txt`, `runtime-cut.txt`);
- **Box64's `wowbox64.dll`** (`wowbox64.sh`, `patches/box64/`), which Wine loads as its x86 CPU DLL for the 32-bit programs;
- the game's **sound output** (`pd2audio.sh`, `pd2-audio/`): our ALSA plugin, which writes the game's mix into a ring the app plays, and Debian's alsa-lib beside `winealsa.so`;
- a **prefix template** that the same Wine makes at build time (`prefix.sh`, under qemu-user); a new prefix on the device is a copy of it, so no `wineboot` runs on the phone. Most of its files are links into the Wine;
- the **x86 .NET 6 runtime** for Project Diablo 2's own launcher (`dotnet.sh`), which the prefix reaches through `C:\Program Files (x86)\dotnet`.

Wine's programs are built for the placeholder data folder like the userland; the app writes its own data folder into them as it unpacks the runtime. There is no Wine Gecko and no Wine Mono: no program the Launcher runs needs them.

## Wine and Box64

- The engine runs Wine's ARM64 programs directly; Wine runs the game's, the installers' and the official launcher's 32-bit x86 code in Box64's `wowbox64.dll`.
- Box64's recompiler keeps its defaults, except for Project Diablo 2's installer (exact x86 flags) and the official launcher with its updater (strong memory order). Our Box64 patch 0006 applies those settings only to the programs they name; patch 0009 allows an entry `name:SETTING`, which the app does not ask for at present. The game and a game that the official launcher starts keep Box64's defaults. The engine logs the `BOX64_*` values of each Wine program it starts (`pd2-launcher: Box64 for <program>: …`). Every `BOX64_DYNAREC_PROGRAMS` list ends with `;`: wowbox64 read it into a reused buffer without a closing NUL, so leftover bytes of an earlier, longer value joined the last name. Our Box64 patch 0010 now ends the value with a NUL; the `;` stays, as it does no harm.
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
- D2GL's graphics profile defaults by GPU: the Performance profile on older and entry-level Adrenos and on Mali. The game resolution is PD2's own 1068×600 picture by default on every screen that holds it, or a Custom size the player types (`GameResolution.kt`). DirectDraw (cnc-ddraw, GDI) is the fallback renderer.

## The X screen and the game's picture

- **Xvfb** runs at the game resolution (PD2's own 1068×600, or a Custom size), for the app's Play and the official PD2 launcher alike (a game the launcher starts runs on its X screen); every X screen has an even width and lines in a multiple of 4, rounded down in one place (`GameResolution.xScreen`), as Turnip pads a buffer's rows to 4. Xvfb runs with its framebuffer in a memfd the app passes as descriptor 3 (`-fbfd 3`): never on flash. Without DRI3, Mesa presents through its software X11 path (`MESA_VK_WSI_DEBUG=sw`); our Mesa patches read the frame from cached memory, count the game's presents and pace them by the app's clock and the Game view.
- **The Game view** (`ui/GameView.kt`, `FrameCopy.kt`, `frame_copy.c`) maps that framebuffer and copies each new picture into an Android surface once per display frame, placed as Display says (Settings and the in-game menu, at once): Fit to screen enlarges what the game shows (PD2's 4:3 menu or its 16:9 game picture, as the picture's shape tells) evenly to the screen's edges, the X screen's other parts past them; Center shows the X screen's own pixels in the middle. A new size keeps the copy's and the game's layers; only their geometry changes.
- **Direct present** (`PresentServer.kt`, `present_server.cpp`, Mesa patch 0004): while the game's full-screen window is alone on the X screen, Mesa renders into AHardwareBuffers the app allocates (one more than the swapchain's images) and the Game view shows them on a layer above the copy, with no Xvfb or CPU copy; the newest frame wins, so the game never waits for the app. `x-input-relay` tells the app when a window covers the game; then the game presents to Xvfb again. The game's buffers do not have the pointer that Xvfb draws into its framebuffer (the Windows pointer in D2GL's menu): while they show, the Game view draws that pointer over them from the relay's image and place (`GamePointer`); while the copy shows, Xvfb's own pointer is in it.
- **Frame rate:** the app's clock (`FrameClock.kt`, `frame_clock.c`, Mesa patch 0008) is the game's vertical blank: it ticks at the player's frame rate (Settings → Game → Graphics, start and sound → Frame rate, or Frame rate in the in-game menu's Quick tab: a number the player types, from 25 to the screen's highest rate), and the game waits for a tick before each frame, with vsync on or off: D2GL's V-Sync setting does not change its pace. While the game is not on the screen, or an overlay (the in-game menu, the Controls screen, their inputs, the editor) shows over it, `x-input-relay` takes the X input focus from the game's window (Slower game in the background), so the game uses its own background frame rate.
- The Install screen shows a small read-only view of the same framebuffer.

## Input and sound

- **Controller:** the app writes the pad's state into a shared-memory file (`PD2_GAMEPAD_FILE`) that the engine's XInput bridge reads, so the game sees an Xbox controller. On-screen controls are a virtual controller merged into the same file (`input/`). Each button, the d-pad's four directions one by one, is bound per mode (`Controls`, applied by `PadInput`): in pad mode to the game's pad, a key, a click or nothing; in mouse mode, where the sticks move the pointer (`PointerSpeed`), to a key, a click or nothing.
- **The in-game menu** (`ui/GameOverlay.kt`) floats over the running game, which never pauses: one panel with a tab strip (Quick: four tiles and Frame rate; Settings: three settings that apply at once, and Stop game). Its gamepad icon opens the **Controls screen** (`ui/ControlsScreen.kt`), where each binding opens the same picker as Settings (`ui/BindingPicker.kt`) and a button switches the controller between pad and mouse mode, as the chord does (`PadInput.useMouseMode`). While an overlay shows, the controller drives it instead of the game (`GameLayer.suspends`).
- **Touch and keyboard** go to `x-input-relay` over a socket as fixed-size events, replayed into the X screen through XTEST. The app also gives the relay the pointer area, the game's picture (`PointerBounds`, `GameResolution.pointerArea`): the relay stops each move by a distance (Trackpad, the controller's mouse mode) at its edges, from where the X screen's pointer really is, because the game and Wine move the pointer too.
- **Sound:** Wine's `winealsa.drv` writes the game's mix through our ALSA plugin (`runtime/pd2-audio/pcm_pd2.c`) into a ring file that the app plays on a low-latency AAudio stream at the device's own rate (`GameAudio.kt`, `game_audio.c`, which resamples from the game's 22.05 kHz). No sound server sits between them; a cushion that grows while the game loads and shrinks back in steady play covers the moments Wine's mixing stalls.

## Releases

`versionName` (`app/build.gradle.kts`) is `MAJOR.MINOR.PATCH` or `MAJOR.MINOR.PATCH-betaN` and the `versionCode` follows from it. Each release is one commit whose message is its version; its APK is signed with the project's release key, so Android updates it only with an APK signed by the same key. A release build carries no pre-filled CD keys and no debug entry points.
