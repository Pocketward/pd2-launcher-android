package com.pd2launcher.app

import com.pd2launcher.app.input.Chord
import com.pd2launcher.app.input.Controls
import com.pd2launcher.app.input.PadButton
import com.pd2launcher.app.input.TouchMode
import java.util.Locale

/**
 * Every setting that changes how the game or the official PD2 launcher runs, or what the logs
 * hold, as one line for the service log at each Play and each press of the official launcher, and
 * for `device.txt`: a report then says which setting was on. The app's own ([AppSettings]) first,
 * then the controls ([Controls] and the Touch setting: how touch and the controller move the
 * pointer), then the engine's ([LauncherSettings.View]; "not known yet" before its first snapshot).
 */
object SettingsLog {
    fun line(app: AppSettings, controls: Controls, launcher: LauncherSettings.View?): String = buildString {
        append("frame rate ").append(if (app.frameRate == AppSettings.FULL_FRAME_RATE) "Full" else app.frameRate.toString())
        append(", low frame rate in the background ").append(onOff(app.lowFrameRateInBackground))
        append(", game resolution ").append(app.customSize?.let { (width, height) -> "Custom $width x $height" } ?: "the game's own size")
        append(", display ").append(
            when (app.gameDisplay) {
                GameDisplay.FIT -> "Fit to screen"
                GameDisplay.CENTER -> "Center"
            },
        )
        append(", graphics profile ").append(app.graphicsProfile?.name?.lowercase() ?: "Automatic")
        append(", keep screen on ").append(onOff(app.keepScreenOn))
        append(", show fps ").append(onOff(app.showFps))
        append(", update channel ").append(AppUpdate.channel(app.updateChannel, BuildConfig.VERSION_NAME).name.lowercase())
        append(", detailed logging ").append(onOff(app.detailedLogging))
        append(", save sync ").append(onOff(app.syncFolder != null))
        append("; controls: touch ").append(touch(app.touchMode))
        append(", pointer speed ").append(String.format(Locale.ROOT, "%.2f×", controls.pointerSpeed))
        append(", on-screen controls ").append(
            when (controls.onScreen) {
                null -> "Auto"
                true -> "Always"
                false -> "Never"
            },
        )
        append(", chord ").append(chord(controls.chord))
        // The buttons of both modes that do not do what they do by default.
        append(", bindings changed ").append(PadButton.entries.sumOf { button -> listOf(false, true).count { !controls.isDefault(button, it) } })
        append("; engine: ")
        if (launcher == null) {
            append("not known yet")
        } else {
            append("renderer ").append(
                when (launcher.renderer) {
                    com.pd2launcher.app.control.Renderer.GLIDE -> "Glide"
                    com.pd2launcher.app.control.Renderer.DIRECT_DRAW -> "DirectDraw"
                },
            )
            append(", skip to Battle.net ").append(onOff(launcher.skipToBnet))
            append(", sound in the background ").append(onOff(launcher.soundInBackground))
            append(", game update checks ").append(launcher.gameUpdateChecks?.let(::onOff) ?: "always")
        }
    }

    /** The Touch setting's name, as the logs write it. */
    fun touch(mode: TouchMode): String = when (mode) {
        TouchMode.TRACKPAD -> "Trackpad"
        TouchMode.DIRECT -> "Direct"
    }

    /** The chord's name, as the logs write it. */
    fun chord(chord: Chord): String = when (chord) {
        Chord.SELECT_START -> "Select+Start"
        Chord.THUMBS -> "L3+R3"
    }

    private fun onOff(on: Boolean) = if (on) "on" else "off"
}
