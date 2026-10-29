package com.pd2launcher.app

import androidx.annotation.StringRes
import com.pd2launcher.app.control.Game
import com.pd2launcher.app.control.Job
import com.pd2launcher.app.control.Snapshot

/**
 * The engine did not take command [seq]: its `command_error`, from the Control channel or the
 * Launcher's own refusal (a guard failure such as `install-not-permitted`). [reason] is the
 * engine's stable code; the screens say it in the app's words ([Refusals.text]).
 */
data class Refusal(val seq: Long, val reason: String)

object Refusals {
    /** The app's own refusal of Start over: the player's characters could not be moved to their own folder first. */
    const val SAVES_NOT_KEPT = "saves-not-kept"

    /**
     * Whether the refusal of [refused] stops the game environment: it was started for the
     * install start [installStart] (Authorize, Install Project Diablo 2, an import) and that
     * start was refused, while no install job or game runs in it. A key refused while Download
     * runs counts too, once the engine holds no keys for it ([Snapshot.authorized]): no Install
     * Diablo II follows that Download.
     */
    fun stopsGameEnvironment(refused: Long, installStart: Long?, snapshot: Snapshot?): Boolean =
        installStart != null && refused == installStart && snapshot?.game != Game.RUNNING &&
            (snapshot?.activeJob == null || (snapshot.activeJob == Job.DOWNLOAD && !snapshot.authorized))

    /** The app's words for [reason]; null: the generic line with the code. */
    @StringRes
    fun text(reason: String): Int? = when (reason) {
        "install-not-permitted" -> R.string.refusal_install
        "start-over-not-permitted" -> R.string.refusal_start_over
        "official-launcher-not-permitted" -> R.string.refusal_official_launcher
        "update-not-permitted" -> R.string.update_refused
        SAVES_NOT_KEPT -> R.string.refusal_saves_not_kept
        "mutation-already-active", "installer-media-locked-while-installing" -> R.string.refusal_busy
        "downloads-required" -> R.string.refusal_downloads_required
        // Also while the removal for a picked folder runs.
        "download-not-permitted" -> R.string.refusal_download
        "engine-busy" -> R.string.refusal_engine_busy
        "service-unavailable", "engine-failed" -> R.string.refusal_engine_gone
        else -> null
    }
}
