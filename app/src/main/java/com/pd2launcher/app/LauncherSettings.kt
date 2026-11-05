package com.pd2launcher.app

import androidx.annotation.StringRes
import com.pd2launcher.app.control.Command
import com.pd2launcher.app.control.Renderer
import com.pd2launcher.app.control.SettingChange
import com.pd2launcher.app.control.Settings
import com.pd2launcher.app.control.SettingsPersistence

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
 * Every Play from the app runs Glide (decision 24, the owner, 2026-11-03: "renderers are now tied
 * to the GPU options"; the App and the Phone driver play with Glide). The engine plays the
 * renderer of its own stored setting, so before each Play a stored DirectDraw goes to Glide first,
 * through the Renderer sheet's own command ([command]), and Play waits for the engine's snapshot
 * with Glide (the engine saves a change before its next snapshot). Before each Play, not once: a
 * player back from a 0.4.0 beta on the Software driver has DirectDraw stored. The owner accepted
 * that a stored DirectDraw is lost. The official PD2 launcher's own renderer option is its own.
 * Pure, so the JVM tests check it.
 */
object GlideFirst {
    /** How long Play waits for the engine's answer; then it starts as before. */
    const val WAIT_MS = 10_000L

    enum class Step {
        /** Play now: Glide is stored, the engine has sent no settings yet, or no answer came in time, or it refused. */
        LAUNCH,

        /** Send [command] first. */
        SET_GLIDE,

        /** [command] went at [step]'s `setAt`: wait for the engine's snapshot. */
        WAIT,
    }

    val command: Command = LauncherSettings.renderer(Renderer.GLIDE)

    /**
     * The next step of a Play with the engine's [settings], Glide asked for at [setAt] (null: not
     * yet), at [now]; [refused]: the engine refused that command, so Play goes on at once.
     */
    fun step(settings: Settings?, setAt: Long?, now: Long, refused: Boolean = false): Step = when {
        settings == null || settings.renderer == Renderer.GLIDE -> Step.LAUNCH
        setAt == null -> Step.SET_GLIDE
        !refused && now - setAt < WAIT_MS -> Step.WAIT
        else -> Step.LAUNCH
    }
}
