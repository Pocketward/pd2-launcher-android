package com.pd2launcher.app

import androidx.annotation.StringRes
import com.pd2launcher.app.control.Command
import com.pd2launcher.app.control.DirectDrawRenderer
import com.pd2launcher.app.control.Renderer
import com.pd2launcher.app.control.SettingChange
import com.pd2launcher.app.control.Settings
import com.pd2launcher.app.control.SettingsPersistence
import java.io.File
import java.io.IOException

/**
 * The Launcher's settings that apply on Android: read from the
 * engine's snapshot, changed with `change_settings` commands. The Launcher saves each change
 * before its next snapshot and uses it on the next launch.
 */
object LauncherSettings {
    /** What the screen shows of [settings]; null while the engine has sent none. */
    data class View(
        val renderer: Renderer,
        val skipToBnet: Boolean,
        val soundInBackground: Boolean,
        /**
         * Settings → Game's "Check for game updates" ([PlayCheck]); null: the engine has no such
         * setting (it always checks), so the row does not show.
         */
        val gameUpdateChecks: Boolean?,
        /** Settings → Game's "Custom start options": added after the options above when Play starts the game; empty for none. */
        val customArguments: String,
        /** Settings → Game's "Game program": the program Play starts, a path in the game folder. */
        val gameProgram: String,
        /** Settings → Game's "Launcher program": the program the official PD2 launcher button opens. */
        val launcherProgram: String,
        /** The official PD2 launcher's "Custom start options"; empty for none. */
        val launcherCustomArguments: String,
        /** Changes are taken: the engine is connected and has loaded its settings. */
        val editable: Boolean,
        /** The engine could not save or load them (its stable reason code). */
        val failure: String?,
    )

    fun view(settings: Settings?, connected: Boolean): View? = settings?.let {
        View(
            renderer = it.renderer,
            skipToBnet = it.skipToBnet,
            soundInBackground = it.soundInBackground,
            gameUpdateChecks = it.updateChecks,
            customArguments = it.customArguments,
            gameProgram = it.gameProgram,
            launcherProgram = it.launcherProgram,
            launcherCustomArguments = it.launcherCustomArguments,
            editable = connected && it.persistence != SettingsPersistence.Loading,
            failure = (it.persistence as? SettingsPersistence.Failed)?.reason,
        )
    }

    /** Settings' save state beside its title: saved, waiting for the engine, or not saved. */
    enum class SaveState { SAVED, WAITING, FAILED }

    fun saveState(view: View?): SaveState = when {
        view?.failure != null -> SaveState.FAILED
        view?.editable != true -> SaveState.WAITING
        else -> SaveState.SAVED
    }

    fun renderer(renderer: Renderer): Command = Command.ChangeSettings(SettingChange.Renderer(renderer))

    fun skipToBnet(enabled: Boolean): Command = Command.ChangeSettings(SettingChange.SkipToBnet(enabled))

    fun soundInBackground(enabled: Boolean): Command = Command.ChangeSettings(SettingChange.SoundInBackground(enabled))

    fun gameUpdateChecks(enabled: Boolean): Command = Command.ChangeSettings(SettingChange.UpdateChecks(enabled))

    /** The most characters of [View.customArguments], as the engine allows. */
    const val CUSTOM_ARGUMENTS_MAX = 200

    /**
     * The custom start options that the player typed, trimmed, as the engine takes them: at most
     * [CUSTOM_ARGUMENTS_MAX] characters (Unicode code points, as the engine counts them), no
     * control character and no double quote; null for any other text. Empty text is none.
     */
    fun typedCustomArguments(text: String): String? = text.trim().takeIf { typed ->
        typed.codePointCount(0, typed.length) <= CUSTOM_ARGUMENTS_MAX && typed.none { it.isISOControl() || it == '"' }
    }

    /** Sets the custom start options ([typedCustomArguments]'s text). */
    fun customArguments(arguments: String): Command = Command.ChangeSettings(SettingChange.CustomArguments(arguments))

    /** Sets the official PD2 launcher's custom start options ([typedCustomArguments]'s text). */
    fun launcherCustomArguments(arguments: String): Command =
        Command.ChangeSettings(SettingChange.LauncherCustomArguments(arguments))

    /** The program Play starts unless the player chose another one: PD2's own. */
    const val DEFAULT_GAME_PROGRAM = "ProjectD2/Game.exe"

    /** The program the official PD2 launcher button opens unless the player chose another one. */
    const val DEFAULT_LAUNCHER_PROGRAM = "ProjectD2/PD2Launcher.exe"

    /** The most characters of a program path, as the engine allows. */
    const val PROGRAM_PATH_MAX = 200

    /** The program Play starts ([Settings.gameProgram]); the default before the engine sent its settings. */
    fun gameProgram(settings: Settings?): String = settings?.gameProgram ?: DEFAULT_GAME_PROGRAM

    /** The program the official PD2 launcher button opens ([Settings.launcherProgram]); the default before the engine sent its settings. */
    fun launcherProgram(settings: Settings?): String = settings?.launcherProgram ?: DEFAULT_LAUNCHER_PROGRAM

    /** Play starts another program than PD2's: no update check, and the engine does not check or set it up. */
    fun otherGameProgram(settings: Settings?): Boolean = gameProgram(settings) != DEFAULT_GAME_PROGRAM

    /** The official PD2 launcher button opens another program than PD2's launcher. */
    fun otherLauncherProgram(settings: Settings?): Boolean = launcherProgram(settings) != DEFAULT_LAUNCHER_PROGRAM

    /**
     * Whether the app looks for and installs Project Diablo 2's updates: Check for game updates is on (or the engine has
     * no such setting), and both programs are PD2's own. Another Game program or Launcher program (a mod, a translated
     * copy) turns them off, so an update never writes over it (owner, 2026-11-14).
     */
    fun pd2Updates(settings: Settings?): Boolean =
        settings?.updateChecks != false && !otherGameProgram(settings) && !otherLauncherProgram(settings)

    /** The engine sent its settings and PD2's updates are off ([pd2Updates]): the "Update checks off" fact. */
    fun pd2UpdatesOff(settings: Settings?): Boolean = settings != null && !pd2Updates(settings)

    /** A program path that the player typed, as the engine takes it ([path]), or why the engine refuses it ([reason]). */
    sealed interface TypedProgram {
        data class Path(val path: String) : TypedProgram

        data class Refused(val reason: String) : TypedProgram
    }

    /**
     * A program path in the game folder as the engine takes it: trimmed, `\` as `/`, with no empty
     * or `.` part. Refused (the engine's reason codes): empty (`program-path-empty`), more than
     * [PROGRAM_PATH_MAX] characters (`program-path-too-long`), a control character or a `:` after
     * the start (`program-path-invalid-character`), `/` or `\` first or a drive such as `C:`
     * (`program-path-absolute`), a `..` part (`program-path-parent`), or not a `.exe` file
     * (`program-path-not-exe`). Whether the file is there is [programMissing].
     */
    fun typedProgramPath(text: String): TypedProgram {
        val typed = text.trim()
        val refused = when {
            typed.isEmpty() -> "program-path-empty"
            typed.codePointCount(0, typed.length) > PROGRAM_PATH_MAX -> "program-path-too-long"
            typed.any { it.isISOControl() } -> "program-path-invalid-character"
            else -> null
        }
        if (refused != null) return TypedProgram.Refused(refused)
        val slashes = typed.replace('\\', '/')
        if (slashes.startsWith('/') || slashes.getOrNull(1) == ':') return TypedProgram.Refused("program-path-absolute")
        if (':' in slashes) return TypedProgram.Refused("program-path-invalid-character")
        val parts = slashes.split('/').filter { it.isNotEmpty() && it != "." }
        if (".." in parts) return TypedProgram.Refused("program-path-parent")
        val name = parts.lastOrNull()
        if (name == null || slashes.endsWith('/') || name.length <= ".exe".length || !name.lowercase().endsWith(".exe")) {
            return TypedProgram.Refused("program-path-not-exe")
        }
        val path = parts.joinToString("/")
        // PD2's own programs in another case are PD2's programs: the default's spelling (as the engine).
        return TypedProgram.Path(listOf(DEFAULT_GAME_PROGRAM, DEFAULT_LAUNCHER_PROGRAM).firstOrNull { it.equals(path, ignoreCase = true) } ?: path)
    }

    /**
     * Whether the game folder ([gameFolder]; null: not known now) holds no file at the program
     * [path] ([typedProgramPath]'s), or only one through a symbolic link on its way: the engine
     * refuses both (`program-path-missing`; it opens the program with no symbolic link). PD2's own
     * programs are never missing here: Default always saves.
     */
    fun programMissing(gameFolder: File?, path: String): Boolean {
        gameFolder ?: return false
        if (path == DEFAULT_GAME_PROGRAM || path == DEFAULT_LAUNCHER_PROGRAM) return false
        return try {
            val file = File(gameFolder.canonicalFile, path)
            !file.isFile || file.canonicalPath != file.absolutePath
        } catch (_: IOException) {
            true
        }
    }

    /** The engine's refusal of a program path whose file is not in the game folder ([programMissing]). */
    const val PROGRAM_MISSING = "program-path-missing"

    /** Every reason the engine can refuse a program path for. */
    val PROGRAM_PROBLEMS = listOf(
        "program-path-empty", "program-path-too-long", "program-path-invalid-character", "program-path-absolute",
        "program-path-parent", "program-path-not-exe", PROGRAM_MISSING,
    )

    /** The help line of a refused program path ([TypedProgram.Refused.reason], or the engine's refusal). */
    @StringRes
    fun programPathProblem(reason: String): Int = when (reason) {
        "program-path-empty" -> R.string.settings_program_empty
        "program-path-too-long" -> R.string.settings_program_too_long
        "program-path-absolute", "program-path-parent" -> R.string.settings_program_outside
        "program-path-not-exe" -> R.string.settings_program_not_exe
        PROGRAM_MISSING -> R.string.settings_program_missing
        else -> R.string.settings_program_invalid_character
    }

    /** Sets the program Play starts ([typedProgramPath]'s path). */
    fun gameProgram(path: String): Command = Command.ChangeSettings(SettingChange.GameProgram(path))

    /** Sets the program the official PD2 launcher button opens ([typedProgramPath]'s path). */
    fun launcherProgram(path: String): Command = Command.ChangeSettings(SettingChange.LauncherProgram(path))
}

/**
 * The renderer of each Play from the app follows the GPU driver (decision 24, the owner,
 * 2026-11-03: "renderers are now tied to the GPU options"): the App and the Phone driver play with
 * Glide, the Software driver with DirectDraw and cnc-ddraw's GDI renderer ([software]). The engine
 * plays the renderer of its own stored settings, so before each Play the settings that differ go
 * first, through the settings' own commands ([commands]), and Play waits for the engine's snapshot
 * with them (the engine saves a change before its next snapshot). Before each Play, not once: the
 * player can change the driver at any time. The owner accepted that a stored DirectDraw is lost.
 * The official PD2 launcher's own renderer option is its own. Pure, so the JVM tests check it.
 */
object GlideFirst {
    /** How long Play waits for the engine's answer; then it starts as before. */
    const val WAIT_MS = 10_000L

    /**
     * Debug builds only: `files/debug/renderer` with `glide` or `ddraw` sets the renderer of each
     * Play for any driver, over the pick's ([commands]' [DebugRenderer]), for the matrix test of
     * the Phone driver's ways with Glide and DirectDraw. The pick stays. Release builds never read it.
     */
    const val DEBUG_FILE = "debug/renderer"

    /** The renderer of [DEBUG_FILE]. */
    enum class DebugRenderer {
        /** Glide (D2GL), as the App and the Phone driver play. */
        GLIDE,

        /** DirectDraw with cnc-ddraw's OpenGL renderer (`renderer=opengl` in `ddraw.ini`), not GDI. */
        DDRAW,
    }

    /** The debug build's [DEBUG_FILE] (its first word: `glide` or `ddraw`); null in other builds, without the file or with another word. */
    fun debugRenderer(filesDir: File, debug: Boolean = BuildConfig.DEBUG): DebugRenderer? {
        if (!debug) return null
        val word = try {
            File(filesDir, DEBUG_FILE).takeIf { it.isFile }?.readText()?.trim()?.lowercase()?.split(Regex("\\s+"))?.firstOrNull()
        } catch (_: IOException) {
            null
        }
        return when (word) {
            "glide" -> DebugRenderer.GLIDE
            "ddraw" -> DebugRenderer.DDRAW
            else -> null
        }
    }

    enum class Step {
        /** Play now: the settings are the driver's, the engine has sent no settings yet, or no answer came in time, or it refused. */
        LAUNCH,

        /** Send [commands] first. */
        SET,

        /** [commands] went at [step]'s `setAt`: wait for the engine's snapshot. */
        WAIT,
    }

    /**
     * The commands that make the engine's [settings] the driver's: Glide, or on the Software
     * driver ([software]) DirectDraw with the `software` DirectDraw renderer (`renderer=gdi` in
     * `ddraw.ini`, and the engine adds `adjmouse=true`); none where they are already so, or where
     * the engine sent no settings yet. Off the Software driver, a stored `software` DirectDraw
     * renderer goes back to PD2's own ([DirectDrawRenderer.PD2], the default the protocol can set),
     * so the engine's settings keep no software after a pick of another driver (a phone check of
     * 2026-11-04); so does the `open_gl` one, which only a debug build's hook sets ([debugRenderer]),
     * so the hook does not stay stored after its file is removed; another DirectDraw renderer of
     * the player stays. A debug build's [debug]
     * renderer ([debugRenderer]) wins over the driver's: Glide as off the Software driver, or
     * DirectDraw with the `open_gl` DirectDraw renderer (the engine writes `renderer=opengl`).
     */
    fun commands(settings: Settings?, software: Boolean, debug: DebugRenderer? = null): List<Command> {
        settings ?: return emptyList()
        if (debug == DebugRenderer.DDRAW) {
            return listOfNotNull(
                LauncherSettings.renderer(Renderer.DIRECT_DRAW).takeIf { settings.renderer != Renderer.DIRECT_DRAW },
                Command.ChangeSettings(SettingChange.DirectDrawRenderer(DirectDrawRenderer.OPEN_GL))
                    .takeIf { settings.directDrawRenderer != DirectDrawRenderer.OPEN_GL },
            )
        }
        if (!software || debug == DebugRenderer.GLIDE) {
            return listOfNotNull(
                LauncherSettings.renderer(Renderer.GLIDE).takeIf { settings.renderer != Renderer.GLIDE },
                Command.ChangeSettings(SettingChange.DirectDrawRenderer(DirectDrawRenderer.PD2))
                    .takeIf { settings.directDrawRenderer in setOf(DirectDrawRenderer.SOFTWARE, DirectDrawRenderer.OPEN_GL) },
            )
        }
        return listOfNotNull(
            LauncherSettings.renderer(Renderer.DIRECT_DRAW).takeIf { settings.renderer != Renderer.DIRECT_DRAW },
            Command.ChangeSettings(SettingChange.DirectDrawRenderer(DirectDrawRenderer.SOFTWARE))
                .takeIf { settings.directDrawRenderer != DirectDrawRenderer.SOFTWARE },
        )
    }

    /**
     * The next step of a Play with the engine's [settings] on the Software driver or not
     * ([software]) and a debug build's renderer ([debug]), the commands sent at [setAt] (null: not yet), at [now]; [refused]: the engine
     * refused one, so Play goes on at once.
     */
    fun step(
        settings: Settings?,
        setAt: Long?,
        now: Long,
        refused: Boolean = false,
        software: Boolean = false,
        debug: DebugRenderer? = null,
    ): Step = when {
        commands(settings, software, debug).isEmpty() -> Step.LAUNCH
        setAt == null -> Step.SET
        !refused && now - setAt < WAIT_MS -> Step.WAIT
        else -> Step.LAUNCH
    }
}
