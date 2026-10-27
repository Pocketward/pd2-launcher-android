package com.pd2launcher.app

import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale
import java.util.TimeZone

/**
 * The service log's `engine snapshot` lines: one for each change of what a snapshot says
 * ([SessionService]'s summary, without its `seq`). The engine sends a snapshot for each change of its
 * state, many a second while a job downloads, and most say what the one before said; those are
 * counted, and the next line says how many came and until when. Share logs and Save logs write the
 * count so far ([checkpoint]), and so does a new engine ([restart]), whose first snapshot always gets
 * a line. Thread-safe: the snapshot collection, Share logs and the service's end use it, and each
 * writes its line with [write] under the same lock, so the log has the lines in their order.
 */
class SnapshotLines(private val write: (String) -> Unit, private val zone: TimeZone = TimeZone.getDefault()) {
    private var last: String? = null
    private var repeats = 0
    private var lastRepeat = 0L

    /** The snapshot [seq] whose summary is [summary], received at [now] (wall clock): its line, unless it says what the last line said. */
    @Synchronized
    fun next(summary: String, seq: Long, now: Long) {
        if (summary == last) {
            repeats++
            lastRepeat = now
            return
        }
        val before = if (repeats > 0) " (${repeated()})" else ""
        last = summary
        repeats = 0
        write("engine snapshot seq=$seq $summary$before")
    }

    /** The line with the repeats counted since the last line, if any (for Share logs, the service's end); the next repeats count from 0. */
    @Synchronized
    fun checkpoint() {
        if (repeats == 0) return
        write("engine snapshot: ${repeated()}")
        repeats = 0
    }

    /** [checkpoint] for an engine that ends; the next engine's first snapshot gets a line whatever it says. */
    @Synchronized
    fun restart() {
        checkpoint()
        last = null
    }

    private fun repeated() = "the line before came $repeats more times, the last at ${time(lastRepeat)}"

    private fun time(at: Long): String = SimpleDateFormat("yyyy-MM-dd HH:mm:ss.SSSXXX", Locale.ROOT).apply { timeZone = zone }.format(Date(at))
}
