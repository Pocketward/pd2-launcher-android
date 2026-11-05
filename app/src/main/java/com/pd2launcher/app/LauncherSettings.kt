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
