package com.pd2launcher.app

import androidx.annotation.StringRes
import com.pd2launcher.app.control.Job

/**
 * Why a Download or Install did not finish, in the app's words. [code] is the engine's stable
 * failure code (`operation.failure`, such as `installer-download-failed`); the screens never show
 * it. A code without words of its own gets the job's general line: the engine's own result card
 * speaks of the desktop (Diagnostics, its folders).
 */
object Failures {
    @StringRes
    fun text(code: String?, job: Job?): Int = when (code) {
        "installer-download-failed" -> R.string.failure_blizzard_download
        "installer-download-stalled" -> R.string.failure_blizzard_stalled
        "installer-downloader-error" -> R.string.failure_blizzard_error
        "installer-download-incomplete" -> R.string.failure_blizzard_incomplete
        "installer-download-rejected" -> R.string.failure_blizzard_rejected
        "installer-downloader-failed" -> R.string.failure_blizzard_ended
        "installer-payload-store-failed" -> R.string.failure_blizzard_store
        "downloads-required" -> R.string.failure_downloads_required
        "download-failed", "client-download-failed" -> R.string.failure_download
        "base-game-key-rejected" -> R.string.failure_d2_key
        "expansion-key-rejected" -> R.string.failure_lod_key
        "base-game-installer-error", "expansion-installer-error",
        "base-game-installer-menu", "expansion-installer-menu" -> R.string.failure_installer
        else -> if (job == Job.DOWNLOAD) R.string.failure_download else R.string.failure_install
    }

    fun line(code: String?, job: Job?): Line = Line(text(code, job))
}
