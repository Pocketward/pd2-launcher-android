package com.pd2launcher.app.ui

import com.pd2launcher.app.Line
import com.pd2launcher.app.R
import com.pd2launcher.app.control.Job

/**
 * A figure's caption from the engine's progress summary: its bytes, speed, time left and counts,
 * joined by " · ". In a Project Diablo 2 job (Update, Check files, Install's Project Diablo 2) the
 * bytes are those of the one file that downloads now: the engine counts none for the whole job,
 * and on Android an update's summary has no file count either. Alone they read as the whole
 * update (for example "26.0 MB of 373.6 MB" for one file of a 555 MB update), so the caption
 * says whose they are, in the app's "26 of 530 MB" form: "This file: 26.0 of 373.6 MB · 5.0 MB/s ·
 * about 1 minute left" (the engine's "for this file" after the time left goes: the start says
 * it). A summary that starts with no bytes ("3 of 10 files · 3 of 10 checked"), and any other
 * job's, as the engine wrote it.
 */
object ProgressCaption {
    private val CLIENT_JOBS = setOf(Job.UPDATE, Job.INSTALL_PROJECT_DIABLO)

    /** An amount as the engine writes it (human_bytes): "512 bytes", "26.0 MB". */
    private const val AMOUNT = """(\d+(?:\.\d+)?) (bytes|KB|MB|GB)"""

    /** "26.0 MB of 373.6 MB" while the file's size is known, else "373.6 MB downloaded". */
    private val BYTES = Regex("""$AMOUNT of $AMOUNT""")
    private val DOWNLOADED = Regex("""$AMOUNT downloaded""")

    private const val SEPARATOR = " · "
    private const val FOR_THIS_FILE = " for this file"

    fun of(summary: String, job: Job?): Line {
        val parts = summary.split(SEPARATOR)
        val first = parts.first()
        val bytes = BYTES.matchEntire(first)
        if (job !in CLIENT_JOBS || (bytes == null && !DOWNLOADED.matches(first))) return Line(R.string.progress_summary, summary)
        // One unit for both amounts where they share it: "26.0 of 373.6 MB".
        val amount = bytes?.destructured?.let { (done, doneUnit, total, totalUnit) ->
            if (doneUnit == totalUnit) "$done of $total $totalUnit" else first
        } ?: first
        val rest = parts.drop(1).map { it.removeSuffix(FOR_THIS_FILE) }
        return Line(R.string.progress_this_file, (listOf(amount) + rest).joinToString(SEPARATOR))
    }
}
