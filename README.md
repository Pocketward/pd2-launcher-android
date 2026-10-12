# PD2 Launcher for Android

> **Unofficial.** PD2 Launcher is a community-made tool. It is not affiliated with or endorsed by the Project Diablo 2 team or Blizzard Entertainment.

An Android app that installs [Project Diablo 2](https://www.projectdiablo2.com/) on your phone or handheld, keeps it up to date, and starts it with touch and controller controls. It sets up Diablo II, Lord of Destruction and Project Diablo 2 the same way the [desktop PD2 Launcher](https://github.com/Pocketward/pd2-launcher) does on Linux, and runs the game on your device's GPU. No root, no Linux container app, no PC streaming.

## What your device needs

- An ARM64 device with Android 13 or later, using 4 KB memory pages (almost every phone and handheld does).
- A supported GPU:
  - Qualcomm Adreno 6xx, 7xx or 8xx (Snapdragon devices), or
  - Arm Mali of the v10 to v13 architectures on Arm's CSF kernel driver, such as the Mali-G310, G610, G615, G715, G720 and G725.
- About 10 GB of free storage for the app and the installation. Once the game works, the app offers to free about 2.5 GB of installer downloads.

The app checks all of this when it first starts and says what is missing. A GPU of a supported family that the bundled graphics driver does not know gets a warning instead: the game may still run. The check does not look at the kernel version.

## Install

Download the APK from the [releases page](https://github.com/Pocketward/pd2-launcher-android/releases) and open it on your device. Android asks once to allow installs from the app you opened it with (your browser or file manager). The app is not on Google Play.

You need your Diablo II and Lord of Destruction CD keys (two different keys) and an internet connection; the app downloads Blizzard's official installers and Project Diablo 2's files from their official sources. If you already have Diablo II and Lord of Destruction 1.14b or 1.14d installed on a PC, you can copy that folder to the device instead and need no CD keys.

## Using it

The app walks you through four steps:

1. **Check:** your device, notifications and battery settings (both optional, but without them Android may stop the game while the app is in the background), and the game's support files, which the app unpacks once.
2. **Download:** pick **Download** (Diablo II and Lord of Destruction from Blizzard, about 2 GB) or **Import an existing installation** (your Diablo II folder copied from a PC; the app makes its own copy). The installers download and are checked.
3. **Install:** enter your CD keys and accept Blizzard's terms; the app then runs Blizzard's installers and Project Diablo 2's unattended, with a small live view of their windows. Expect half an hour or more. **Stop** pauses safely and **Resume** continues where it stopped. The app never saves your CD keys: it passes them to Blizzard's installer only. Blizzard's installer stores them in the installed game, as Diablo II requires; **Repair → Start over** removes the installed game and with it the keys.
4. **Play:** starts Project Diablo 2. When Project Diablo 2 has an update, the button offers **Update** first; you can always play without updating. **Official PD2 launcher** opens Project Diablo 2's own launcher, for example to manage loot filters.

In the game:

- **Touch:** tap to click, long press and drag as with a mouse. On-screen controls (a virtual controller) show while no controller is connected; switch them with **Touch pad** in the in-game menu and arrange them with **Edit controls**.
- **Controller:** works as an Xbox controller. Press **Select + Start** (or **L3 + R3**, set in Controls) to switch to mouse mode for menus and typing; hold it to open the in-game menu. Every button can be rebound.
- **In-game menu:** the floating button, Android's Back gesture, or the controller chord opens a menu on the right edge, without pausing the game: Keyboard, Touch pad, Show fps, Controls, Edit controls, Settings and Stop game.
- **Settings:** the renderer (Glide, recommended, or DirectDraw), the game resolution (fewer lines run smoother; Automatic picks one for your GPU), the graphics profile (Default or Performance), Skip to Battle.net, sound in the background, the controller, and storage.

The game keeps running when you switch apps; a notification shows while it does.

## Your files

Characters are kept in the app's own storage, apart from the installation, so reinstalling or updating Project Diablo 2 never touches them. The game folder (with your characters and loot filter) appears in Android's Files app under **PD2 Launcher**, and **Settings → App → Game files** imports and exports files.

Uninstalling the app removes the installation. Android offers to keep the app's data; copy your characters out first if you want to be sure to keep them.

## Problems

Open an [issue](https://github.com/Pocketward/pd2-launcher-android/issues) with your device's model, Android version and GPU (the Check screen shows them) and what happened.

## Building from source

This repository holds the Android app (`app/`), the Linux programs and patches it carries (`userland/`) and the packing of the game's runtime (`runtime/`). The build runs in pinned containers (Podman or Docker); only the runtime's packing (`runtime/pack.sh`), which downloads and packs files without building anything, runs on the host. The build downloads the engine, the [desktop PD2 Launcher](https://github.com/Pocketward/pd2-launcher) release that [`engine.lock`](engine.lock) pins, and fetches FEX-Emu, glibc, Mesa, the X server, the Debian packages and the game's runtime at pinned versions, with the patches in `userland/` applied on top. See [`docs/BUILDING.md`](docs/BUILDING.md) for the steps and [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) for how the pieces fit together.

## Disclaimer

Project Diablo 2, Diablo, and Blizzard are names of their respective owners. PD2 Launcher contains none of their software or data: it downloads Blizzard's installers and Project Diablo 2's files from their official sources on your device.
