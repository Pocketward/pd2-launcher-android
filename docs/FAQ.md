# FAQ

Short answers to questions that players often ask. For a different problem, open an [issue](https://github.com/Pocketward/pd2-launcher-android/issues) and attach the app's logs (see [How do I send logs?](#how-do-i-send-logs)).

**The game**

- [The game or the official PD2 launcher closes by itself in the background](#the-game-or-the-official-pd2-launcher-closes-by-itself-in-the-background)
- [Why does the game not fill my screen?](#why-does-the-game-not-fill-my-screen)
- [Which devices and graphics chips work?](#which-devices-and-graphics-chips-work)
- [How do I use a controller?](#how-do-i-use-a-controller)
- [Can I use a mouse and a keyboard?](#can-i-use-a-mouse-and-a-keyboard)
- [How do I open the game on the online screen?](#how-do-i-open-the-game-on-the-online-screen)

**The official PD2 launcher**

- [The official PD2 launcher is slow to open](#the-official-pd2-launcher-is-slow-to-open)
- [The official PD2 launcher asks a second time after its update](#the-official-pd2-launcher-asks-a-second-time-after-its-update)
- [What is the Live folder in ProjectD2?](#what-is-the-live-folder-in-projectd2)

**Downloads and updates**

- [The download or install stops part way](#the-download-or-install-stops-part-way)
- [The app does not accept my CD key](#the-app-does-not-accept-my-cd-key)
- [A download stops while the app is in the background](#a-download-stops-while-the-app-is-in-the-background)
- [How do updates work?](#how-do-updates-work)

**Your files**

- [Where are the game folder and my characters?](#where-are-the-game-folder-and-my-characters)
- [How do I sync my characters with Syncthing?](#how-do-i-sync-my-characters-with-syncthing)

**Android and problem reports**

- [Android 17 shows a message about 16 KB memory pages](#android-17-shows-a-message-about-16-kb-memory-pages)
- [How do I send logs?](#how-do-i-send-logs)

## The game

### The game or the official PD2 launcher closes by itself in the background

Android 12 and later have a limit for the extra processes that apps start, the "child processes". The app runs Wine, the game and the official PD2 launcher as child processes. When Android stops one of them, the game or the launcher can close with no error message.

Android has two rules for child processes:

- When all apps together have more than 32 child processes, Android stops some of them. It stops the processes of apps in the background first.
- Android stops a child process that uses much processor time while its app is in the background.

To prevent this, turn off the limit. The steps are different for each Android version.

**Android 11:** you do not have to do anything. Android 11 has no limit for child processes.

**Android 12:** use adb (see the steps below) and type these two commands:

```
adb shell device_config set_sync_disabled_for_tests persistent
adb shell device_config put activity_manager max_phantom_processes 2147483647
```

The first command makes sure that Android does not overwrite the value of the second command. The second command sets the limit to the largest possible number. These commands do not turn off the rule about processor time.

**Android 12L and 13:** use adb (see the steps below) and type this command. It turns off the two rules.

```
adb shell settings put global settings_enable_monitor_phantom_procs false
```

**Android 14 and later:** open **Developer options** (step 2 below). In the **Apps** part, turn on **Disable child process restrictions**. This turns off the two rules. Keep Developer options on: when you turn Developer options off, Android turns the restrictions on again. If your phone does not show this switch, use the adb command for Android 12L and 13.

To use adb:

1. On a computer (Windows, macOS or Linux), download [Android SDK Platform-Tools](https://developer.android.com/tools/releases/platform-tools) and unpack it. It contains adb.
2. On the phone, show **Developer options**: open **Settings → About phone** and tap **Build number** seven times. On some phones, **Build number** is in a different place. See [Android's guide](https://developer.android.com/studio/debug/dev-options#enable).
3. In **Developer options**, turn on **USB debugging**.
4. Connect the phone to the computer with a USB cable. When the phone asks, allow USB debugging from this computer.
5. On the computer, open a terminal in the platform-tools folder. Type `adb devices` and make sure that your phone shows in the list. In Windows PowerShell, type `.\adb` instead of `adb`.
6. Type the commands for your Android version.
7. When you are done, you can turn off **USB debugging**.

With no USB cable, Android 11 and later can also use **Wireless debugging**. See [Android's guide](https://developer.android.com/tools/adb#wireless-adb-android-11).

To undo the change:

- Android 12: type `adb shell device_config delete activity_manager max_phantom_processes`, then `adb shell device_config set_sync_disabled_for_tests none`.
- Android 12L and later: type `adb shell settings delete global settings_enable_monitor_phantom_procs`. On Android 14 and later, also turn off **Disable child process restrictions**.

Sources, in Android's source code:

- The limit of 32 and `max_phantom_processes`: [ActivityManagerConstants.java](https://android.googlesource.com/platform/frameworks/base/+/refs/tags/android-12.0.0_r1/services/core/java/com/android/server/am/ActivityManagerConstants.java) (Android 12).
- `set_sync_disabled_for_tests`: [DeviceConfigService.java](https://android.googlesource.com/platform/frameworks/base/+/refs/tags/android-12.0.0_r1/packages/SettingsProvider/src/com/android/providers/settings/DeviceConfigService.java) (Android 12).
- `settings_enable_monitor_phantom_procs`: [FeatureFlagUtils.java](https://android.googlesource.com/platform/frameworks/base/+/refs/tags/android-12.1.0_r1/core/java/android/util/FeatureFlagUtils.java) (Android 12L). It turns off the rule about the number in [PhantomProcessList.java](https://android.googlesource.com/platform/frameworks/base/+/refs/tags/android-13.0.0_r1/services/core/java/com/android/server/am/PhantomProcessList.java) and the rule about processor time in [ActivityManagerService.java](https://android.googlesource.com/platform/frameworks/base/+/refs/tags/android-13.0.0_r1/services/core/java/com/android/server/am/ActivityManagerService.java) (Android 13).
- **Disable child process restrictions**: [PhantomProcessPreferenceController.java](https://android.googlesource.com/platform/packages/apps/Settings/+/refs/tags/android-14.0.0_r1/src/com/android/settings/development/PhantomProcessPreferenceController.java) (Android 14).

### Why does the game not fill my screen?

Project Diablo 2 supports only one resolution for online play: 1068 × 600 (widescreen). This is the official rule of the Project Diablo 2 team. The `!res` command in their Discord shows it. This app does not change Project Diablo 2's files to get other resolutions.

By default, the game runs at its own size, 1068 × 600. On a screen that is smaller than this, the game uses the full size of the screen. The game keeps its shape. On a screen with a different shape, for example 4:3 or 20:9, a part of the screen stays black.

**Settings → Game** has two settings for this:

- **Display:** how the game shows on the screen. **Fit to screen** (the default) makes what the game shows as large as the screen allows, with the same shape. **Center** shows each game pixel as one screen pixel, in the middle of the screen. A change applies at once. **Display** is also in the in-game menu.
- **Game resolution:** the size that the game draws at. Select **Game's own size** (recommended) or **Custom…**. For **Custom…**, type a width from 640 and a height from 480, up to the size of your screen. The width must be an even number, and the height must be a multiple of 4. A change applies the next time you press **Play**.

For single player, some mods let the game use other resolutions, for example 4:3. You can find them on Reddit or on the Project Diablo 2 Discord. To use such a mod:

1. Put the mod in your game folder. Import a game folder that has the mod (**Use files you have**), or copy the mod's files into the game folder with Android's Files app.
2. In **Settings → Game**, turn off **Check for game updates**. Play then starts the game as it is, with no update. The official PD2 launcher can still update the game when you open it.
3. In **Settings → Game → Game resolution**, select **Custom…** and type the size for the mod.

For online play, use **Game's own size** and keep **Check for game updates** on.

### Which devices and graphics chips work?

The app needs an ARM64 device with Android 11 or later, 4 KB memory pages and a supported graphics chip (GPU). Android does not install the app on Android 10 or earlier. See [What your device needs](../README.md#what-your-device-needs) and [Supported graphics chips](../README.md#supported-graphics-chips) in the README. When the app starts for the first time, it checks your device and tells you what is missing.

### How do I use a controller?

Connect a Bluetooth or USB controller. In pad mode, the game sees an Xbox controller. The app selects a button layout for each controller when it connects.

- Press **Select + Start** to switch between pad mode and mouse mode. Hold **Select + Start** to open the in-game menu. To use **L3 + R3** instead, go to **Settings → Controller**.
- Use mouse mode for the game's menus and for typing. In mouse mode, the sticks move the pointer. **A** or **L2** is a left click, and **B** or **R2** is a right click.
- To see or change what each button does, open the in-game menu and tap the gamepad icon (**Controls**). Press the row of a button to change it, also while you play. Each direction of the d-pad can change too. **Settings → Controller** has the same settings.

If a controller has wrong buttons, send us the logs (see [How do I send logs?](#how-do-i-send-logs)). The logs name each controller and the layout that the app selected.

### Can I use a mouse and a keyboard?

Yes. A USB or Bluetooth mouse moves the game's cursor. Its left, right and middle buttons and its wheel work as on a PC. A USB or Bluetooth keyboard works as on a PC. With no keyboard, tap **Keyboard** in the in-game menu to open Android's keyboard. A mouse or a keyboard does not hide the on-screen controls. Only a controller hides them.

### How do I open the game on the online screen?

Turn on **Skip to Battle.net** in **Settings → Game**. The next time you press **Play**, the game opens on the online screen.

## The official PD2 launcher

### The official PD2 launcher is slow to open

While the official PD2 launcher starts, the screen is black and shows **Starting the official PD2 launcher…** in the middle. On a slower phone, the launcher can take up to a minute to open. Wait for its window. If the window does not show after 20 seconds, the screen also tells you that it can take a minute.

### The official PD2 launcher asks a second time after its update

After the official PD2 launcher updates itself, it can ask a second time. Tap **OK** again.

### What is the Live folder in ProjectD2?

After its first update, the official PD2 launcher keeps a second copy of the game's client files in the folder **Live** in the **ProjectD2** folder. This copy is about 0.5 GB. The app does not use this copy and does not remove it.

## Downloads and updates

### The download or install stops part way

For example, it stops at about 80 %. Tap **Try again**. The app keeps what it downloaded and continues from there. A second try often finishes. If **Fast download** fails again, go back to the **Diablo II source** page and choose **Blizzard's installers**.

### The app does not accept my CD key

The line under the key says what is wrong. A key has 26 letters and numbers; the dashes are optional. Keys of 16 characters do not work: your Blizzard account shows the 26-character key for such a key. Type the Diablo II key in the first field and the Lord of Destruction key in the second.

### A download stops while the app is in the background

Some phones stop the network of an app, or stop the app, while it is in the background. This saves battery or data. Do these steps:

1. In **Settings → App → Android**, tap **Battery settings**. Select **Not optimised** for PD2 Launcher in the list that opens.
2. In the same part, make sure that **Notifications** are on. The notification keeps the app and the game running.
3. In Android's settings for PD2 Launcher, let the app use the network in the background. The names of these settings are different on each phone.

The **Connection** row on the **Check** screen tells you when the phone blocks the network of the app. It also opens the app's settings.

### How do updates work?

The app and Project Diablo 2 have different updates.

- **The app:** while the app is open, it looks for a new version each hour. The setting is **Settings → App → Look for new versions** (on by default). When a new version is available, Android shows a notification, and a gold dot shows on **About**. In **About**, tap **Download**, then **Install**, and confirm in Android's window. The download starts only when you tap **Download**; on mobile data, a note gives the size first. The first time, Android asks you to allow **Install unknown apps** for PD2 Launcher, and can restart the app when you do. Your game, characters and settings stay.
- **Project Diablo 2:** when you press **Play**, the app first checks for a new version of Project Diablo 2. If there is one, **New version found** asks you. **Update** installs it, and then the game starts. **Cancel** keeps your version, and Play then offers **Update** and **Play without updating**. Online games can need the current version.
- **Check for game updates** (**Settings → Game**, on by default): turn it off to keep your Project Diablo 2 as it is, for example with a mod. This setting does not change the updates of the app.
- After the official PD2 launcher updates the game files, Play can ask you to check them one time. Tap **Check files**.

## Your files

### Where are the game folder and my characters?

In Android's Files app, the game folder is under **PD2 Launcher**, in the folder **Diablo II**. **Settings → App → Game files → Open in Files** opens it. Your characters are in its **Save** folder. Your loot filter is in the game folder too.

When the app finds two different copies of a character, it keeps the copy that was saved last. The other copy goes to **Save/Backups**. The app keeps all backups: delete them yourself when you do not need them. When **Save sync** is on, a backup that you delete in the game folder stays in your sync folder. Delete it there too.

When you uninstall the app, Android removes the installation. Android offers to keep the app's data. Copy your characters out first, or use **Save sync**. Before you put a mod's files in the game folder, read [Your files](../README.md#your-files) in the README.

### How do I sync my characters with Syncthing?

Use **Save sync** in **Settings → App → Game files**. It keeps the game's Save folder and a folder that you pick the same.

1. Turn on **Save sync**.
2. Pick or make a folder, for example **Documents/PD2 Saves**, and allow access one time. The folder can be on the device or on an SD card.
3. In Syncthing, or in a different sync app, sync this folder with your other devices.

The app copies from the folder into the game when you press **Play** or open the official PD2 launcher. It copies from the game to the folder after the game or the launcher closes. **Sync now** copies in the two directions. The app never copies while the game runs.

When a character changed in the two places, the copy that was saved last stays. The other copy goes to **Save/Backups**. When you delete a character in one place, the app deletes it in the other place too, after a backup. The app copies each backup to the folder one time. It never copies backups from the folder into the game. Syncthing's own files, for example its conflict copies, never go into the game. When the folder is not available, for example when the SD card is out, **Play** still works and Settings tells you. When you turn **Save sync** off, both copies stay.

## Android and problem reports

### Android 17 shows a message about 16 KB memory pages

Android 17 can show a message about 16 KB memory pages. The app still works.

### How do I send logs?

1. In **Settings → App → Help**, turn on **Detailed logging**. It applies at the next **Play** or official PD2 launcher.
2. Make the problem occur again.
3. In **Settings → App → Help**, tap **Share logs** to send the file, or **Save logs** to save it on the device, for example in Downloads.
4. Attach the file to an [issue](https://github.com/Pocketward/pd2-launcher-android/issues). Write your device's model, its Android version and its GPU (the **Check** screen shows them), and what occurred.
5. Turn off **Detailed logging**. While it is on, the logs fill up faster.

**Share logs** and **Save logs** are also below an error on the **Check**, **Download** and **Install** screens. The logs contain no CD keys, but they can show your character names and network addresses.
