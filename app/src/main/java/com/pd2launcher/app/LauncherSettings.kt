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
}
