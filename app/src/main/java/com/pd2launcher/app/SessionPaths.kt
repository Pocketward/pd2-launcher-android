package com.pd2launcher.app

import java.io.File
import java.nio.file.Files
import java.nio.file.attribute.PosixFilePermissions

/**
 * The session's folders under the app's canonical data folder [data] (`/data/data/<package>`,
 * [DeviceCheck.dataDir]) and the environments of the programs it starts.
 */
class SessionPaths(val data: String) {
    val files = File(data, "files")
    val home = File(files, "home")
    val launcherHome = File(files, "launcher")
    val tmp = File(files, "tmp")
    val logs = File(files, "logs")
    val userlandRoot = File(files, "userland")
    /** The stable path every userland program is built for. */
    val userland = File(userlandRoot, UserlandInstaller.CURRENT)
    val loader = File(userland, "lib/ld-linux-aarch64.so.1")
    val userlandEnvFile = File(userland, "etc/userland.env")

    /**
     * Xvfb's framebuffer folder (`-fbdir`, before Android 11; a memfd since, [Framebuffer]),
     * compiled keymaps and the runtime folder for PulseAudio.
     */
    val framebufferDir = File(tmp, "xvfb")
    /** Screen 0's framebuffer file in [framebufferDir] (Xvfb names it), before Android 11. */
    val framebuffer = File(framebufferDir, "Xvfb_screen0")
    val keymapDir = File(tmp, "xkb")
    val runtimeDir = File(tmp, "run")
    /**
     * The game's present counter: the App userland's Mesa (patch 0002) adds one to its first
     * 32-bit word for every frame the game sends to the X screen; the Game view's fps counter
     * reads it ([FrameCopy.presents]). [GameEnvironment] makes it; the engine passes its path to
     * the GL thunk's host side as `PD2_PRESENT_COUNTER` ([engineEnvironment]).
     */
    val presentCounter = File(runtimeDir, "presents")
    /**
     * The game's direct present ([PresentServer], Mesa patch 0004): the app listens on this Unix
     * socket for the game environment's lifetime; the engine passes its path to the GL thunk's
     * host side as `PD2_PRESENT_SOCKET` ([engineEnvironment]).
     */
    val presentSocket = File(runtimeDir, "present.sock")

    /** The Launcher's managed root, `<launcher home>/game`. */
    val managedRoot = File(launcherHome, "game")

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

    /** The Managed prefix's Diablo II folder (the engine's `GAME_RELATIVE`). */
    val managedGame = File(managedRoot, "prefix/drive_c/Program Files (x86)/Diablo II")

    /**
     * The player's own files, apart from any install: no install, update, Start over or folder
     * copy touches it.
     */
    val userData = File(files, "userdata")
    /** The player's characters: the game folder's `Save` links here ([SaveKeeper]). */
    val userSave = File(userData, "Save")
    /** Where a character that met another one of its name is kept (`backup-<time>`). */
    val saveBackups = File(userData, "backups")

    /** Owner-only folders: the app process's umask is 0, so plain mkdirs would make them 0777. */
    fun createFolders() {
        val ownerOnly = PosixFilePermissions.asFileAttribute(PosixFilePermissions.fromString("rwx------"))
        for (dir in listOf(home, launcherHome, tmp, logs, framebufferDir, keymapDir, runtimeDir)) {
            Files.createDirectories(dir.toPath(), ownerOnly)
        }
    }

    /** `KEY=value` lines of `<userland>/etc/userland.env`, in order; empty when it is absent. */
    private fun userlandEnvironment(): List<Pair<String, String>> =
        if (userlandEnvFile.isFile) parseEnvironment(userlandEnvFile.readText()) else emptyList()

    /**
     * Exactly the contract's variables, FEX's runtime folder, the userland's library environment
     * (`LD_*` from userland.env), its CA certificates (`SSL_CERT_FILE`: the engine's rustls reads
     * its roots from there, Android has no `/etc/ssl`) and its App-provided FEX values
     * (`PD2_APP_FEX*`: the entry point, the thunk folders and the GL thunk's host values), which
     * only the engine gets, plus the [presentCounter] and the [presentSocket] as two of those host
     * values.
     * The host values are the Adreno's (Turnip) unless [mali] (a Mali GPU, PanVK:
     * [maliHostValues]); [devDriver] lets a hand-placed Mali driver build replace the bundled one
     * (debug builds only, [devDriverValues]).
     */
    fun engineEnvironment(mali: Boolean = false, devDriver: Boolean = BuildConfig.DEBUG): List<String> {
        val values = userlandEnvironment().filter { (key, _) ->
            key.startsWith("LD_") || key == SSL_CERT_FILE || key.startsWith(APP_FEX)
        }
        val host = if (mali) maliHostValues(values, devDriver) else values.filterNot { (key, _) -> key.startsWith(MALI_HOST) }
        return base() + "XDG_RUNTIME_DIR=${runtimeDir.path}" + host.map { (key, value) -> "$key=$value" } +
            "${HOST}PD2_PRESENT_COUNTER=${presentCounter.path}" +
            "${HOST}PD2_PRESENT_SOCKET=${presentSocket.path}"
    }

    /**
     * On a Mali: userland.env's Mali set (`PD2_APP_FEX_MALI_HOSTENV_<NAME>`) as host values
     * (`PD2_APP_FEX_HOSTENV_<NAME>`), each replacing the host value of its name, without Turnip's
     * own (`TU_DEBUG` and the Compatibility driver's values, which PanVK does not read); with
     * [devDriver], [devDriverValues] over those (a later value of a name wins).
     */
    private fun maliHostValues(values: List<Pair<String, String>>, devDriver: Boolean): List<Pair<String, String>> {
        val mali = values.filter { (key, _) -> key.startsWith(MALI_HOST) }.map { (key, value) -> HOST + key.removePrefix(MALI_HOST) to value }
        val over = (mali + if (devDriver) devDriverValues() else emptyList()).toMap()
        return values.filterNot { (key, _) ->
            key in over || key == "${HOST}TU_DEBUG" || key.startsWith(MALI_HOST) || key.startsWith(COMPATIBILITY_HOST)
        } + over.toList()
    }

    /**
     * Debug builds, for work on the Mali driver: a PanVK build placed by hand as
     * `files/kbase/panfrost_icd.json` (its library beside it) in place of the bundled one, and the
     * `NAME=value` lines of `files/kbase/hostenv` as extra host values (such as GFXReconstruct's
     * capture layer), in that order. Empty when neither file is there.
     */
    private fun devDriverValues(): List<Pair<String, String>> {
        val dir = File(files, "kbase")
        val icd = File(dir, "panfrost_icd.json").takeIf { it.isFile }
            ?.let { listOf("${HOST}VK_DRIVER_FILES" to it.path, "${HOST}VK_ICD_FILENAMES" to it.path) }.orEmpty()
        val extra = File(dir, "hostenv").takeIf { it.isFile }?.let { parseEnvironment(it.readText()) }.orEmpty()
        return icd + extra.map { (name, value) -> HOST + name to value }
    }

    /** The resolver configuration the userland's glibc reads ([Dns] writes it). */
    val resolvConf = File(userland, "etc/resolv.conf")

    /** The one Wine's x86 glibc reads in the FEX RootFS (runtime/pack.sh lays it out). */
    val guestResolvConf = File(managedRoot, "fex/rootfs/Ubuntu_24_04/etc/resolv.conf")

    /** The contract's variables plus the userland.env values for Xvfb, PulseAudio and X clients. */
    fun userlandProgramEnvironment(): List<String> {
        val extra = userlandEnvironment().filterNot { (key, _) -> key.startsWith(APP_FEX) }
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
        /** The prefix of every App-provided FEX value in userland.env. */
        private const val APP_FEX = "PD2_APP_FEX"
        /** A host value of FEX's GL thunk (`PD2_APP_FEX_HOSTENV_<NAME>`, read by the engine). */
        private const val HOST = "PD2_APP_FEX_HOSTENV_"
        /** A host value for a Mali GPU (PanVK) in userland.env, given to the engine as [HOST] on a Mali only. */
        private const val MALI_HOST = "PD2_APP_FEX_MALI_HOSTENV_"
        /** A host value of the Compatibility graphics driver (Turnip only). */
        private const val COMPATIBILITY_HOST = "PD2_APP_FEX_COMPATIBILITY_HOSTENV_"
        private const val SSL_CERT_FILE = "SSL_CERT_FILE"
        private val ASSIGNMENT = Regex("""^([A-Za-z_][A-Za-z0-9_]*)=(.*)$""")

        /** `KEY=value` lines; comments, blank lines and anything else are skipped. Values are literal. */
        fun parseEnvironment(text: String): List<Pair<String, String>> =
            text.lineSequence().mapNotNull { line ->
                ASSIGNMENT.matchEntire(line.trim())?.destructured?.let { (key, value) -> key to value }
            }.toList()
    }
}
