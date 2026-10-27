package com.pd2launcher.app

/**
 * Every setting that changes how the game or the official PD2 launcher runs, or what the logs
 * hold, as one line for the service log at each Play and each press of the official launcher, and
 * for `device.txt`: a report then says which setting was on. The app's own ([AppSettings]) first,
 * then the engine's ([LauncherSettings.View]; "not known yet" before its first snapshot).
 */
object SettingsLog {
    fun line(app: AppSettings, launcher: LauncherSettings.View?): String = buildString {
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
        append(", app update checks ").append(onOff(app.updateChecks))
        append(", detailed logging ").append(onOff(app.detailedLogging))
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

    private fun onOff(on: Boolean) = if (on) "on" else "off"
}
