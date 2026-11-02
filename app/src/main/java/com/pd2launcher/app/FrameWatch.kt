package com.pd2launcher.app

import java.util.Locale

/**
 * The game's frames in the service log, which Share logs and Save logs give, from its present
 * counter ([PresentCounter]): while the game runs, lines for each wait of more than [STALL_MS] for
 * a new picture while the game is on the screen (when the wait passes [STALL_MS], [LONG_WAIT_MS]
 * and [VERY_LONG_WAIT_MS], and when it ends; a wait that starts less than [STALL_LINE_MS] after
 * the last one with lines is only counted), and when it stops, a summary: the average frame rate
 * while on the screen, the slowest second that no time of more than [STALL_MS] without a new
 * picture overlaps (so not the game's first one, while it loads), the longest wait (one that has
 * not ended at the stop too) and how many waits there were, with the frame rate setting and the
 * screen's rate. A wait while the app is in the background is not one: the game draws at its
 * background rate there. Both renderers present through Mesa (DirectDraw's too); a game that shows
 * no picture, or a renderer that does not use the counter, counts no presents: then there are no
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
    /**
     * The second that ended last ([pendingEnd]) and its frame rate: it counts for [lowest] once no
     * wait of more than [STALL_MS] that began in it is possible any more ([keepSecond]).
     */
    private var pendingFps: Double? = null
    private var pendingEnd = 0L
    /** A wait of more than [STALL_MS] overlaps the second that goes on: it does not count. */
    private var windowWaited = false
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
            val waited = wasShown && presented && now - lastChange > STALL_MS
            val line = if (waited) waitEnded(now, now - lastChange, ", until the app went to the background") else null
            keepSecond(waited)
            restart(now)
            return line
        }
        shownMs += elapsed
        shownPresents += added
        windowPresents += added
        val wait = now - lastChange
        if (wait > STALL_MS) {
            // A wait of more than 1 s goes on, or ends at this look: no second it overlaps counts.
            windowWaited = true
            if (pendingFps != null && lastChange < pendingEnd) pendingFps = null
        }
        if (now - windowStart >= WINDOW_MS) {
            // The second before this one: a wait that began in it would be longer than 1 s by now.
            keepSecond(false)
            if (!windowWaited) {
                pendingFps = windowPresents * 1000.0 / (now - windowStart)
                pendingEnd = now
            }
            windowStart = now
            windowPresents = 0
            windowWaited = added == 0L && wait > STALL_MS
        }
        if (added == 0L) return if (presented) waitGoesOn(now, wait) else null
        lastChange = now
        // A new picture after the second's end, no wait of more than 1 s before it: that second counts.
        if (wait <= STALL_MS && lastChange >= pendingEnd) keepSecond(false)
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

    /**
     * The second that ended last counts for [lowest], unless a wait of more than [STALL_MS] that
     * began before its end is known now ([waited]: the wait from [lastChange] is one).
     */
    private fun keepSecond(waited: Boolean) {
        val fps = pendingFps ?: return
        pendingFps = null
        if (waited && lastChange < pendingEnd) return
        lowest = lowest?.coerceAtMost(fps) ?: fps
    }

    private fun start(now: Long, presents: Long?) {
        lastLook = now
        lastShown = false
        lastPresents = presents
        shownMs = 0
        shownPresents = 0
        lowest = null
        pendingFps = null
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
        windowWaited = false
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
        keepSecond(frozen)
        val allWaits = waits + if (frozen) 1 else 0
        val longest = if (frozen) maxOf(longestMs, open) else longestMs
        val average = shownPresents * 1000.0 / shownMs
        val slowest = lowest?.let { "the slowest second ${String.format(Locale.ROOT, "%.1f", it)} fps (not in a wait)" } ?: "no second outside the waits"
        return "frames at the game's end: ${String.format(Locale.ROOT, "%.1f", average)} fps on average over " +
            "${minutes(shownMs)} on the screen, $slowest, " +
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

/**
 * The game's frames as the direct present shows them ([PresentServer.counts], [PresentCounts]),
 * in the service log while the game runs, beside [FrameWatch]:
 * - at the first PRESENT of a swapchain and at each change of its present mode, the swapchain's
 *   size and mode (V-Sync on is FIFO or FIFO_RELAXED, off is IMMEDIATE or MAILBOX);
 * - every [PERIOD_MS] while it runs (so a kill keeps a part) and at its end, from its start: the
 *   frames on the screen, each frame's time on the screen in refreshes of the display (1, 2, 3, 4,
 *   5 or more: a frame held 33 or 42 ms on a 120 Hz screen counts as 4 or 5), the PRESENTs by mode,
 *   the app's clock (its ticks a second, from the present counter's page), and how much of the
 *   game's time on the screen ([shown], as for [FrameWatch]) the game's own buffers showed; the X
 *   screen's copy has no present fences, so its frames are not in these counts;
 * - with Detailed logging ([next]'s detailed, read at each look), each 5 s line of the direct
 *   present (`pd2-present` in logcat) with the clock's rate since the line before, and each window
 *   of the pointer split ([PointerSplit.line]).
 * Not thread-safe: the session service's frame look uses it alone.
 */
class PresentWatch {
    /** Whether the game ran at the last look. */
    var active = false
        private set
    private var start = PresentCounts.NONE
    private var last: PresentCounts? = null
    private var startTicks: Long? = null
    private var startAt = 0L
    private var lastLook = 0L
    private var lastShown = false
    private var shownMs = 0L
    private var lastPeriod = 0L
    /** The swapchain and mode of the last mode line. */
    private var lastMode: Pair<Long, Int>? = null
    private var lastStats = 0L
    private var statsTicks: Pair<Long, Long>? = null
    private var lastSplit = 0L

    /**
     * The lines for a look at [now] (elapsed real time, ms): the game [running] or not, the direct
     * present's [counts] (null: no server), the clock's [ticks] (null: no counter), whether the
     * game is on the screen [shown], whether Detailed logging is on, and the direct present's
     * pointer split ([split]; null: no server).
     */
    fun next(
        now: Long, running: Boolean, counts: PresentCounts?, ticks: Long?, shown: Boolean, detailed: Boolean, split: PointerSplit? = null,
    ): List<String> {
        if (counts != null) last = counts
        if (running != active) {
            active = running
            if (!running) return listOf(summary("at the game's end", now, ticks))
            start = counts ?: PresentCounts.NONE
            startTicks = ticks
            startAt = now
            lastLook = now
            lastShown = shown
            shownMs = 0
            lastPeriod = now
            // A swapchain of an earlier game is gone (its width is 0): the next one gets its line.
            lastMode = null
            lastStats = counts?.stats ?: 0
            statsTicks = ticks?.let { it to now }
            lastSplit = split?.serial ?: 0
            return emptyList()
        }
        if (!running) return emptyList()
        if (shown && lastShown) shownMs += now - lastLook
        lastLook = now
        lastShown = shown
        val lines = mutableListOf<String>()
        if (counts != null && counts.swapchainPresents > 0 && counts.width > 0 && lastMode != counts.swapchain to counts.mode) {
            lastMode = counts.swapchain to counts.mode
            lines += modeLine(counts)
        }
        if (now - lastPeriod >= PERIOD_MS) {
            lastPeriod = now
            lines += summary("so far", now, ticks)
        }
        if (counts != null && counts.stats != lastStats) {
            lastStats = counts.stats
            val clock = rate(statsTicks, ticks, now)
            statsTicks = ticks?.let { it to now }
            if (detailed && counts.stats > 0) lines += statsLine(counts, clock)
        }
        if (split != null && split.serial != lastSplit) {
            lastSplit = split.serial
            if (detailed && split.serial > 0) lines += split.line()
        }
        return lines
    }

    private fun summary(time: String, now: Long, ticks: Long?): String {
        val end = last ?: PresentCounts.NONE
        // A server that started again meanwhile counts from 0.
        val from = if (end.swapchain < start.swapchain || end.onScreenNanos < start.onScreenNanos) PresentCounts.NONE else start
        val onScreen = end.onScreen.zip(from.onScreen) { a, b -> (a - b).coerceAtLeast(0) }
        val byMode = end.byMode.zip(from.byMode) { a, b -> (a - b).coerceAtLeast(0) }
        val frames = onScreen.sum()
        val presents = byMode.sum()
        val clock = rate(startTicks?.let { it to startAt }, ticks, now)?.let { String.format(Locale.ROOT, "clock %.1f ticks/s", it) } ?: "no clock"
        if (frames == 0L && presents == 0L) {
            return "frames on the screen $time: none through the game's own buffers (the X screen's copy shows the game); $clock"
        }
        val refresh = end.refreshNanos
        val groups = if (refresh > 0) {
            String.format(Locale.ROOT, "time on the screen in refreshes of %.1f ms (%.0f Hz): ", refresh / 1e6, 1e9 / refresh) +
                onScreen.mapIndexed { i, n -> "${if (i == onScreen.size - 1) "${i + 1} or more" else "${i + 1}"} ×$n" }.joinToString()
        } else {
            "time on the screen not counted (the display's rate is not known)"
        }
        val modes = if (presents == 0L) {
            "no presents"
        } else {
            "presents " + byMode.withIndex().filter { it.value > 0 }.sortedByDescending { it.value }
                .joinToString { String.format(Locale.ROOT, "%s %.0f %%", PresentCounts.MODES[it.index], it.value * 100.0 / presents) }
        }
        val visible = (end.visibleNanos - from.visibleNanos).coerceAtLeast(0) / 1_000_000
        val share = if (shownMs > 0) String.format(Locale.ROOT, "%.0f %%", minOf(100.0, visible * 100.0 / shownMs)) else "unknown"
        return String.format(Locale.ROOT, "frames on the screen %s: %d in %.0f s; ", time, frames, (end.onScreenNanos - from.onScreenNanos).coerceAtLeast(0) / 1e9) +
            "$groups; $modes; $clock; the game's own buffers on the screen $share of the time"
    }

    companion object {
        /** How often a summary comes while the game runs. */
        const val PERIOD_MS = 10 * 60_000L

        /** The line for a new swapchain or present mode of [counts]. */
        fun modeLine(counts: PresentCounts): String {
            val name = PresentCounts.MODES.getOrNull(counts.mode - 1)
            val mode = when (counts.mode) {
                1, 2 -> "with V-Sync off ($name)"
                3, 4 -> "with V-Sync on ($name)"
                else -> "with no present mode named (Mesa sends none)"
            }
            return "frames: the game's swapchain ${counts.width} x ${counts.height} presents $mode, straight to the screen"
        }

        /** The Detailed 5 s line of [counts], with the clock's [clock] ticks a second (null: unknown). */
        fun statsLine(counts: PresentCounts, clock: Double?): String = String.format(
            Locale.ROOT, "frames (5 s): %dx%d: %.1f presents/s, %.1f shown/s, longest gap %.1f ms, %.1f/s back at once, %s; %s",
            counts.width, counts.height, counts.presentsPerSecond, counts.shownPerSecond, counts.longestGapMs, counts.backPerSecond,
            PresentCounts.WHERE.getOrElse(counts.where) { "" }, clock?.let { String.format(Locale.ROOT, "clock %.1f ticks/s", it) } ?: "no clock",
        )

        /** Ticks a second from [from] (ticks, ms) to [ticks] at [now]; the 32-bit word wraps. Null without both or no time between. */
        private fun rate(from: Pair<Long, Long>?, ticks: Long?, now: Long): Double? {
            if (from == null || ticks == null || now <= from.second) return null
            return ((ticks - from.first) and 0xFFFFFFFFL) * 1000.0 / (now - from.second)
        }
    }
}
