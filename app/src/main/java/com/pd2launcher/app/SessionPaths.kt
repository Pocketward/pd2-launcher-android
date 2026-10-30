package com.pd2launcher.app

import java.io.File
import java.nio.file.Files
import java.nio.file.attribute.PosixFilePermissions

/**
 * The session's folders under the app's canonical data folder [data] ([DeviceCheck.dataDir]) and
 * the environments of the programs it starts.
 */
class SessionPaths(val data: String) {
    val files = File(data, "files")
    val home = File(files, "home")
    val launcherHome = File(files, "launcher")
    val tmp = File(files, "tmp")
    val logs = File(files, "logs")
    /** The engine's own diagnostics (its activity log and job history), which Share logs and Save logs pack ([LogShare]). */
    val engineDiagnostics = File(launcherHome, "logs/diagnostics")
    val userlandRoot = File(files, "userland")
    /** The path every userland program runs from: the unpack rewrites the userland for it ([Relocation]). */
    val userland = File(userlandRoot, UserlandInstaller.CURRENT)
    val loader = File(userland, "lib/ld-linux-aarch64.so.1")
    val userlandEnvFile = File(userland, "etc/userland.env")

    /**
     * Xvfb's framebuffer folder (`-fbdir`, when no memfd can be made, [Framebuffer]),
     * compiled keymaps and the session's runtime folder (`XDG_RUNTIME_DIR` of the engine and the
     * game's Wine processes).
     */
    val framebufferDir = File(tmp, "xvfb")
    /** Screen 0's framebuffer file in [framebufferDir] (Xvfb names it), when no memfd can be made. */
    val framebuffer = File(framebufferDir, "Xvfb_screen0")
    val keymapDir = File(tmp, "xkb")
    val runtimeDir = File(tmp, "run")
    /**
     * The game's present counter: the App userland's Mesa (patch 0002) adds one to its first
     * 32-bit word for every frame the game sends to the X screen; the Game view's fps counter
     * reads it ([FrameCopy.presents]). [GameEnvironment] makes it; the engine passes its path to
     * the game's Wine processes as `PD2_PRESENT_COUNTER` ([engineEnvironment]).
     */
    val presentCounter = File(runtimeDir, "presents")
    /**
     * The game's direct present ([PresentServer], Mesa patch 0004): the app listens on this Unix
     * socket for the game environment's lifetime; the engine passes its path to the game's Wine
     * processes as `PD2_PRESENT_SOCKET` ([engineEnvironment]).
     */
    val presentSocket = File(runtimeDir, "present.sock")
    /**
     * The bytes this app has received (Android's count for its user, which Blizzard's downloader
     * under Wine shares), written by the session service while a job downloads or installs
     * ([ReceivedBytes]); the engine reads it as `PD2_RECEIVED_BYTES_FILE` ([engineEnvironment]),
     * so a download that receives data but writes nothing yet does not count as stalled.
     */
    val receivedBytes = File(runtimeDir, "received-bytes")
    /**
     * The game's sound ([GameAudio]): the ring the app plays, which the game's Wine writes through
     * ALSA, and the ALSA configuration that names it, which Wine finds in `XDG_RUNTIME_DIR`
     * (runtime patch 0003).
     */
    val soundRing = File(runtimeDir, "pd2-audio")
    val alsaConfig = File(runtimeDir, "asound.conf")
    /** The Unix sockets the session binds below [data]: their paths must fit `sun_path`. */
    val sockets: List<File> get() = listOf(presentSocket)

    /** The Launcher's managed root, `<launcher home>/game`. */
    val managedRoot = File(launcherHome, "game")
    /** The runtime's ALSA plugin, which writes the game's sound into [soundRing] (runtime/pd2audio.sh). */
    val alsaPlugin = File(managedRoot, "${AppRuntime.WINE}/lib/wine/aarch64-unix/libasound_module_pcm_pd2.so")

    /**
     * "Diablo II already installed": the picked folder, copied below here and used in place
     * (outside the managed root, which the Launcher refuses as an existing installation).
     */
    val imports = File(files, "import")

    /**
     * One copy of a picked folder, in its own [token] folder below [imports], so only the
     * engine's word on this copy names its path.
     */
    fun importedGame(token: String) = File(imports, "$token/Diablo II")

    /**
     * Where an older (FEX) installation's game folder waits for Import when the update in place
     * failed ([LegacyPrefix.setAside], [UpgradeImport]): one fixed token, so a start that follows a
     * stop finds it again.
     */
    val upgradeGame = importedGame(UpgradeImport.TOKEN)

    /** Debug builds only: with this file the update in place fails on purpose ([LegacyPrefix.failForTest]). */
    val failUpdateForTest = File(files, "debug-fail-update")

    /** Debug builds only: with this file it fails after the new prefix took its place ([LegacyPrefix.failLateForTest]). */
    val failUpdateLateForTest = File(files, "debug-fail-update-late")

    /** Debug builds only: with this file the userland's `resolv.conf` takes Android's DNS servers as none ([Dns]). */
    val emptyDnsForTest = File(files, "debug-empty-dns")

    /**
     * Debug builds only: with this file every program the session starts runs under the seccomp
     * test harness [seccompRun] with Android 11's app seccomp policy ([ProcessStarter]). A getter,
     * so that a release build, which never reads it, has neither path.
     */
    val seccompHarnessForTest: File get() = File(files, "debug-seccomp-api30")

    /** The seccomp test harness (userland/glibc/tests/seccomp-run.c), put there by a development tool. */
    val seccompRun: File get() = File(files, "seccomp-run")

    /** The Managed prefix's Diablo II folder (the engine's `GAME_RELATIVE`). */
    val managedGame = File(managedRoot, "prefix/drive_c/Program Files (x86)/Diablo II")

    /**
     * The player's own files, apart from any install: no install, update, Start over or folder
     * copy touches it.
     */
    val userData = File(files, "userdata")
    /** The player's characters: the game folder's `Save` links here ([SaveKeeper]). */
    val userSave = File(userData, "Save")
    /**
     * Where a character that met another one of its name is kept (`backup-<time>`): `Save/Backups`
     * (owner, 2026-10-27), so the player sees it in the Files app under the game folder's Save, and
     * Save sync copies it to the sync folder. The game reads only the top of Save. [SaveKeeper]
     * never merges it as saves.
     */
    val saveBackups = File(userSave, SaveKeeper.BACKUPS)
    /** Where the backups were up to 0.3.4 (`files/userdata/backups`); [SaveKeeper.moveOldBackups] empties it once. */
    val oldSaveBackups = File(userData, "backups")

    /** Owner-only folders: the app process's umask is 0, so plain mkdirs would make them 0777. */
    fun createFolders() {
        val ownerOnly = PosixFilePermissions.asFileAttribute(PosixFilePermissions.fromString("rwx------"))
        for (dir in listOf(home, launcherHome, tmp, logs, framebufferDir, keymapDir, runtimeDir)) {
            Files.createDirectories(dir.toPath(), ownerOnly)
        }
    }

    /**
     * What older releases left in [runtimeDir] and nothing uses since 0.3.3: PulseAudio's runtime
     * folder (`pulse`), its script (`pulse.pa`) and its pipe sink (`pulse-sink`), and FEXServer's
     * socket of the FEX runtime before 0.3.0 (`pd2launcher-<uid>-<hash>.FEXServer.Socket`, or its
     * fallback `<uid>.FEXServer.Socket`). Only these names go; a link goes as a link, never what
     * it leads to. The names removed.
     */
    fun removeOldRuntimeFiles(): List<String> {
        val old = runtimeDir.list()?.filter { it in OLD_RUNTIME_FILES || FEX_SOCKET.matches(it) }?.sorted() ?: return emptyList()
        for (name in old) UserlandInstaller.deleteTree(File(runtimeDir, name).toPath())
        return old
    }

    /** `KEY=value` lines of `<userland>/etc/userland.env`, in order; empty when it is absent. */
    private fun userlandEnvironment(): List<Pair<String, String>> =
        if (userlandEnvFile.isFile) parseEnvironment(userlandEnvFile.readText()) else emptyList()

    /**
     * Exactly the contract's variables, the userland's library environment (`LD_*` from
     * userland.env), its CA certificates (`SSL_CERT_FILE`: the engine's rustls reads its roots from
     * there, Android has no `/etc/ssl`), the app's marker (`PD2_ANDROID_APP=1`), and what the
     * engine passes on to the game's Wine processes: userland.env's GL and Vulkan values
     * (`PD2_APP_WINE_<NAME>` as `<NAME>`), the Compatibility driver's (`PD2_APP_WINE_COMPATIBILITY_<NAME>`
     * as `PD2_APP_COMPATIBILITY_<NAME>`, which the engine uses for a Play with that driver), the
     * font and X locale configuration, Box64's quiet settings, the [presentCounter] and the
     * [presentSocket]; and the [receivedBytes] file for the engine itself.
     * The GPU values are the Adreno's (Turnip) unless [mali] (a Mali GPU, PanVK: [maliValues]);
     * [devDriver] lets a hand-placed Mali driver build replace the bundled one (debug builds only,
     * [devDriverValues]). On an Adreno, [ubwcFlagHint] (0 or 1, [UbwcFlagHint]) adds Turnip's TP
     * UBWC flag hint as [FD_DEV_FEATURES], which the engine keeps for the standard and the
     * Compatibility driver; null adds nothing (Mesa's own value).
     */
    fun engineEnvironment(
        mali: Boolean = false,
        devDriver: Boolean = BuildConfig.DEBUG,
        noconform: Boolean = false,
        ubwcFlagHint: Int? = null,
    ): List<String> {
        val values = userlandEnvironment()
        val own = values.filter { (key, _) ->
            key.startsWith("LD_") || key == SSL_CERT_FILE || key == "FONTCONFIG_FILE" || key == "XLOCALEDIR"
        }
        val wine = values.filter { (key, _) ->
            key.startsWith(WINE) && !key.startsWith(MALI) && !key.startsWith(COMPATIBILITY)
        }.map { (key, value) -> key.removePrefix(WINE) to value }
        val compatibility = values.filter { (key, _) -> key.startsWith(COMPATIBILITY) }
            .map { (key, value) -> ENGINE_COMPATIBILITY + key.removePrefix(COMPATIBILITY) to value }
            .let { if (noconform) withNoconform(it, ENGINE_COMPATIBILITY + TU_DEBUG) else it }
        val gpu = if (mali) {
            maliValues(values, wine, devDriver)
        } else {
            (if (noconform) withNoconform(wine, TU_DEBUG) else wine) + compatibility +
                listOfNotNull(ubwcFlagHint?.let { FD_DEV_FEATURES to ubwcFlagHintFeature(it) }) +
                if (devDriver) debugHostValues() else emptyList()
        }
        return base() + "XDG_RUNTIME_DIR=${runtimeDir.path}" + "PD2_ANDROID_APP=1" +
            (own + gpu).map { (key, value) -> "$key=$value" } +
            "BOX64_LOG=0" + "BOX64_NOBANNER=1" +
            "PD2_PRESENT_COUNTER=${presentCounter.path}" +
            "PD2_PRESENT_SOCKET=${presentSocket.path}" +
            "PD2_RECEIVED_BYTES_FILE=${receivedBytes.path}"
    }

    /**
     * On a Mali: userland.env's Mali set (`PD2_APP_WINE_MALI_<NAME>`) as `<NAME>`, each replacing
     * the [wine] value of its name, without Turnip's own (`TU_DEBUG` and the Compatibility
     * driver's values, which PanVK does not read); with [devDriver], [devDriverValues] over those
     * (a later value of a name wins).
     */
    private fun maliValues(
        values: List<Pair<String, String>>,
        wine: List<Pair<String, String>>,
        devDriver: Boolean,
    ): List<Pair<String, String>> {
        val mali = values.filter { (key, _) -> key.startsWith(MALI) }.map { (key, value) -> key.removePrefix(MALI) to value }
        val over = (mali + if (devDriver) devDriverValues() else emptyList()).toMap()
        return wine.filterNot { (key, _) -> key in over || key == "TU_DEBUG" } + over.toList()
    }

    /**
     * Debug builds, for work on the GPU drivers: a PanVK build placed by hand as
     * `files/kbase/panfrost_icd.json` (its library beside it) in place of the bundled one, and the
     * `NAME=value` lines of `files/kbase/hostenv` as extra values for the game's Wine processes
     * ([debugHostValues], which an Adreno gets too, for example for Mesa's debug output), in that
     * order. Empty when neither file is there.
     */
    private fun devDriverValues(): List<Pair<String, String>> {
        val icd = File(files, "kbase/panfrost_icd.json").takeIf { it.isFile }
            ?.let { listOf("VK_DRIVER_FILES" to it.path, "VK_ICD_FILENAMES" to it.path) }.orEmpty()
        return icd + debugHostValues()
    }

    /**
     * [values] with `noconform` added to the Turnip debug value [key] (an Adreno without hardware
     * multiview, [adrenoWithoutMultiview]); the value is added when it is missing.
     */
    private fun withNoconform(values: List<Pair<String, String>>, key: String): List<Pair<String, String>> =
        if (values.none { it.first == key }) {
            values + (key to NOCONFORM)
        } else {
            values.map { (name, value) -> if (name == key) name to "$value,$NOCONFORM" else name to value }
        }

    /** The `NAME=value` lines of `files/kbase/hostenv` (debug builds, every GPU); else empty. */
    private fun debugHostValues(): List<Pair<String, String>> =
        File(files, "kbase/hostenv").takeIf { it.isFile }?.let { parseEnvironment(it.readText()) }.orEmpty()

    /** The resolver configuration the userland's glibc reads ([Dns] writes it). */
    val resolvConf = File(userland, "etc/resolv.conf")

    /** The contract's variables plus the userland.env values for Xvfb and X clients. */
    fun userlandProgramEnvironment(): List<String> {
        val extra = userlandEnvironment().filterNot { (key, _) -> key.startsWith(WINE) }
        val overridden = extra.map { it.first }.toSet()
        return base().filter { it.substringBefore('=') !in overridden } + extra.map { (key, value) -> "$key=$value" }
    }

    private fun base(): List<String> = listOf(
        "HOME=${home.path}",
        "PD2_LAUNCHER_HOME=${launcherHome.path}",
        "TMPDIR=${tmp.path}",
        "LANG=C.UTF-8",
        "PATH=${userland.path}/usr/bin",
    )

    companion object {
        /** A value in userland.env for the game's Wine processes only (`PD2_APP_WINE_<NAME>`). */
        private const val WINE = "PD2_APP_WINE_"
        /** Such a value for a Mali GPU (PanVK), given as `<NAME>` on a Mali only. */
        private const val MALI = "PD2_APP_WINE_MALI_"
        /** Such a value of the Compatibility graphics driver (Turnip only). */
        private const val COMPATIBILITY = "PD2_APP_WINE_COMPATIBILITY_"
        /** How the engine reads a Compatibility driver's value (`PD2_APP_COMPATIBILITY_<NAME>`). */
        private const val ENGINE_COMPATIBILITY = "PD2_APP_COMPATIBILITY_"
        private const val TU_DEBUG = "TU_DEBUG"
        private const val NOCONFORM = "noconform"
        private const val SSL_CERT_FILE = "SSL_CERT_FILE"
        private val ASSIGNMENT = Regex("""^([A-Za-z_][A-Za-z0-9_]*)=(.*)$""")
        /** [removeOldRuntimeFiles]: PulseAudio's files of the releases before 0.3.3. */
        private val OLD_RUNTIME_FILES = setOf("pulse", "pulse.pa", "pulse-sink")
        /** [removeOldRuntimeFiles]: FEXServer's socket names of the releases before 0.3.0. */
        private val FEX_SOCKET = Regex("""(pd2launcher-[0-9]+-[0-9a-f]+|[0-9]+)\.FEXServer\.Socket""")

        /** `KEY=value` lines; comments, blank lines and anything else are skipped. Values are literal. */
        fun parseEnvironment(text: String): List<Pair<String, String>> =
            text.lineSequence().mapNotNull { line ->
                ASSIGNMENT.matchEntire(line.trim())?.destructured?.let { (key, value) -> key to value }
            }.toList()
    }
}
