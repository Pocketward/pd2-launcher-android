package com.pd2launcher.app

import java.util.Locale

/**
 * The game's frames in the service log, which Share logs and Save logs give, from its present
 * counter ([PresentCounter]): while the game runs, lines for each wait of more than [STALL_MS] for
 * a new picture while the game is on the screen (when the wait passes [STALL_MS], [LONG_WAIT_MS]
 * and [VERY_LONG_WAIT_MS], and when it ends; a wait that starts less than [STALL_LINE_MS] after
 * the last one with lines is only counted), and when it stops, a summary: the average frame rate
 * while on the screen, the slowest second, the longest wait (one that has not ended at the stop
 * too) and how many waits there were, with the frame rate setting and the screen's rate. A wait
 * while the app is in the background is not one: the game draws at its background rate there.
 * A renderer that does not use the counter (DirectDraw's) counts no presents: then there are no
 * wait lines, and the summary says so. The service looks every [POLL_MS], so a wait is known to
 * that step: the lines say "about". Not thread-safe: the session service's frame look uses it
 * alone.
 */
class FrameWatch {
    /** Whether the game ran at the last look. */
    var active = false
        private set
    private var lastLook = 0L
    /** Whether the game was on the screen at the last look. */
    private var lastShown = false
    private var lastPresents: Long? = null
    /** When the game last showed a new picture while on the screen, or came on the screen. */
    private var lastChange = 0L
    private var shownMs = 0L
    private var shownPresents = 0L
    private var windowStart = 0L
    private var windowPresents = 0L
    private var lowest: Double? = null
    private var longestMs = 0L
    private var waits = 0
    /** When the last wait with lines began to get them; null: none this game. */
    private var lastWaitLine: Long? = null
    private var waitsUnwritten = 0
    private var counted = false
    /** Whether the counter counted a present this game: a renderer that uses it. */
    private var presented = false
    /** How many of [WAIT_MARKS_MS] the wait that goes on has passed. */
    private var waitMarks = 0
    /** Whether the wait that goes on gets lines. */
    private var waitWritten = false

    /**
     * The line for a look at [now] (elapsed real time, ms), if any: the game [running] or not, its
     * present counter's count [presents] (null: none), and whether it is on the screen [shown];
     * [frameRate] (the setting's words) and [screenHz] (0: not known) go into the summary.
     */
    fun next(now: Long, running: Boolean, presents: Long?, shown: Boolean, frameRate: String, screenHz: Float): String? {
        if (running != active) {
            active = running
            if (running) {
                start(now, presents)
                return null
            }
            return summary(frameRate, screenHz)
        }
        if (!running || presents == null) return null
        val previous = lastPresents
        lastPresents = presents
        val elapsed = now - lastLook
        lastLook = now
        val wasShown = lastShown
        lastShown = shown
        if (previous == null) {
            restart(now)
            return null
        }
        counted = true
        // A 32-bit count, so it wraps.
        val added = (presents - previous) and 0xFFFFFFFFL
        if (added > 0) presented = true
        if (!shown || !wasShown) {
            // A wait that goes on as the app goes to the background ends there.
            val line = if (wasShown && presented && now - lastChange > STALL_MS) {
                waitEnded(now, now - lastChange, ", until the app went to the background")
            } else {
                null
            }
            restart(now)
            return line
        }
        shownMs += elapsed
        shownPresents += added
        windowPresents += added
        if (now - windowStart >= WINDOW_MS) {
            val fps = windowPresents * 1000.0 / (now - windowStart)
            lowest = lowest?.coerceAtMost(fps) ?: fps
            windowStart = now
            windowPresents = 0
        }
        val wait = now - lastChange
        if (added == 0L) return if (presented) waitGoesOn(now, wait) else null
        lastChange = now
        val line = if (wait > STALL_MS) waitEnded(now, wait) else null
        waitMarks = 0
        waitWritten = false
        return line
    }

    /** The line, if any, for a wait of [wait] ms that goes on at [now]. */
    private fun waitGoesOn(now: Long, wait: Long): String? {
        val passed = WAIT_MARKS_MS.count { wait > it }
        if (passed <= waitMarks) return null
        val first = waitMarks == 0
        waitMarks = passed
        if (first || !waitWritten) {
            // A long wait gets its lines even when another wait had lines a short time before.
            waitWritten = mayWrite(now) || passed > 1
            if (!waitWritten) return null
            if (passed > 1) lastWaitLine = now
            return "frames: the game shows no new picture for about ${seconds(wait)} s so far${unwritten()}"
        }
        return "frames: still no new picture after about ${seconds(wait)} s"
    }

    /** The line, if any, for a wait of [wait] ms that ended at [now], with [why] at its end. */
    private fun waitEnded(now: Long, wait: Long, why: String = ""): String? {
        waits++
        longestMs = maxOf(longestMs, wait)
        val write = if (waitMarks == 0) mayWrite(now) else waitWritten
        if (!write) {
            waitsUnwritten++
            return null
        }
        val unwritten = if (waitMarks == 0) unwritten() else ""
        return "frames: the game showed no new picture for about ${seconds(wait)} s$why$unwritten"
    }

    /** Whether a wait that begins to get lines at [now] may: none had them in the last [STALL_LINE_MS]. */
    private fun mayWrite(now: Long): Boolean {
        val may = lastWaitLine?.let { now - it >= STALL_LINE_MS } ?: true
        if (may) lastWaitLine = now
        return may
    }

    private fun unwritten(): String {
        val words = if (waitsUnwritten > 0) " ($waitsUnwritten more since the last such line)" else ""
        waitsUnwritten = 0
        return words
    }

    private fun start(now: Long, presents: Long?) {
        lastLook = now
        lastShown = false
        lastPresents = presents
        shownMs = 0
        shownPresents = 0
        lowest = null
        longestMs = 0
        waits = 0
        waitsUnwritten = 0
        lastWaitLine = null
        counted = false
        presented = false
        restart(now)
    }

    /** The waits and the slowest second count from [now] again: the game came on the screen, or the count did. */
    private fun restart(now: Long) {
        lastChange = now
        windowStart = now
        windowPresents = 0
        waitMarks = 0
        waitWritten = false
    }

    private fun summary(frameRate: String, screenHz: Float): String {
        val setting = "frame rate setting $frameRate, screen ${if (screenHz > 0f) "${String.format(Locale.ROOT, "%.0f", screenHz)} Hz" else "rate unknown"}"
        if (!counted) return "frames at the game's end: no present counter; $setting"
        if (shownMs < WINDOW_MS) return "frames at the game's end: the game was not on the screen; $setting"
        if (!presented) {
            return "frames at the game's end: no presents counted (this renderer does not use the counter, or the game " +
                "showed no picture); $setting"
        }
        // A wait that had not ended when the game stopped, the picture frozen to the end.
        val open = if (lastShown) lastLook - lastChange else 0L
        val frozen = open > STALL_MS
        val allWaits = waits + if (frozen) 1 else 0
        val longest = if (frozen) maxOf(longestMs, open) else longestMs
        val average = shownPresents * 1000.0 / shownMs
        return "frames at the game's end: ${String.format(Locale.ROOT, "%.1f", average)} fps on average over " +
            "${minutes(shownMs)} on the screen, the slowest second ${lowest?.let { String.format(Locale.ROOT, "%.1f", it) } ?: "unknown"} fps, " +
            "the longest wait for a new picture about ${seconds(longest)} s${if (frozen) " (no new picture at the end)" else ""}, " +
            "waits of more than ${STALL_MS / 1000} s: $allWaits; $setting"
    }

    private fun seconds(ms: Long) = String.format(Locale.ROOT, "%.1f", ms / 1000.0)

    private fun minutes(ms: Long): String {
        val seconds = ms / 1000
        return "${seconds / 60} min ${seconds % 60} s"
    }

    companion object {
        /** How often the session service looks at the present counter. */
        const val POLL_MS = 250L
        /** A wait for a new picture longer than this is a wait the log names. */
        const val STALL_MS = 1000L
        /** A wait this long gets another line while it goes on. */
        const val LONG_WAIT_MS = 10_000L
        /** And this long, one more. */
        const val VERY_LONG_WAIT_MS = 60_000L
        /** The shortest time between the starts of two waits with lines. */
        const val STALL_LINE_MS = 10_000L
        private val WAIT_MARKS_MS = longArrayOf(STALL_MS, LONG_WAIT_MS, VERY_LONG_WAIT_MS)
        private const val WINDOW_MS = 1000L
    }
}
