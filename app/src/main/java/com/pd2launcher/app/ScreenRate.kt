package com.pd2launcher.app

import android.content.Context
import android.hardware.display.DisplayManager
import android.os.Handler
import android.os.Looper
import android.util.Log
import android.view.Display
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import java.util.Locale

/**
 * The screen's full rate: the refresh rate the built-in display really runs at for the app while
 * the Game view asks it for its highest mode. Android lists the modes the panel has
 * ([FrameRateVote.highest]), not the rate the phone's settings let it run at: a phone with
 * Smooth Display off has modes up to 120 Hz and stays at 60, so at 120 fps the game drew twice
 * the frames the screen showed. The Frame rate's top (Full), the clock's rate at Full, the 60 fps
 * default (Full on a screen not faster than 60) and the rate Settings and the in-game menu show
 * all follow it ([FrameRateVote]).
 *
 * The Game view measures it ([ScreenRateWatch]); the app keeps the last measurement with the
 * highest mode of then, for Settings, which ask the display for nothing. Before a measurement, and
 * on a display whose highest mode is another, the full rate is the highest mode. The player's
 * pick stays as it is: only what Full and a pick at or above the full rate mean follows the screen.
 */
object ScreenRate {
    private const val PREFERENCES = "screen_rate"
    private const val HIGHEST = "highest"
    private const val RATE = "rate"
    private const val TAG = "pd2-screen-rate"
    private val _full = MutableStateFlow(0f)

    /**
     * The built-in display's full rate, as [load] read it or the Game view measured it; 0 before
     * [load] and without a display. Set on the main thread only.
     */
    val full: StateFlow<Float> = _full.asStateFlow()

    /** The built-in display's full rate from its modes and the kept measurement, on any thread. */
    fun read(context: Context): Float {
        val kept = context.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE)
        return full(FrameRateVote.highest(context), kept.getFloat(HIGHEST, 0f), kept.getFloat(RATE, 0f))
    }

    /** [read] into [full], on the main thread (Settings and the Game view as they open); returns it. */
    fun load(context: Context): Float = read(context).also { _full.value = it }

    /**
     * The full rate of a screen whose modes go up to [highest], with the [measured] rate kept from
     * a screen whose modes went up to [measuredHighest]: that rate where the highest modes are the
     * same (at most the highest mode), else the highest mode.
     */
    fun full(highest: Float, measuredHighest: Float, measured: Float): Float =
        if (measured > 0f && FrameRateVote.most(measuredHighest) == FrameRateVote.most(highest)) measured.coerceAtMost(highest) else highest

    /**
     * The Game view measured [rate] on the built-in display, whose modes go up to [highest]
     * ([RateWatch]): kept where it is new; true when [full] changed. Main thread.
     */
    fun measured(context: Context, highest: Float, rate: Float): Boolean {
        val kept = context.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE)
        val same = FrameRateVote.most(kept.getFloat(HIGHEST, 0f)) == FrameRateVote.most(highest) &&
            FrameRateVote.most(kept.getFloat(RATE, 0f)) == FrameRateVote.most(rate)
        if (!same) kept.edit().putFloat(HIGHEST, highest).putFloat(RATE, rate).apply()
        val full = full(highest, highest, rate)
        if (FrameRateVote.most(full) == FrameRateVote.most(_full.value)) return false
        Log.i(TAG, String.format(Locale.ROOT, "full rate %.1f (modes up to %.1f)", full, highest))
        _full.value = full
        return true
    }
}

/**
 * The rule of [ScreenRateWatch], from the display's refresh rate as the app reads it
 * (`Display.getRefreshRate`), which has every limit on the app's rate in it: the phone's settings,
 * power saving, a rate Android holds the app to. A rate above the full rate counts once it held
 * [RAISE_NANOS] (the display ran at it; a moment's change of the policy is not the app's rate). A
 * lower one only while the Game view's layers ask for the highest mode ([vote]), once it held
 * [SETTLE_NANOS] (Android's guide to frame rates gives a mode change 2 s) while the game drew
 * ([quiet] starts that time again): with no new frames Android lowers the display's rate (on
 * some phones after 80 ms) whatever the layers ask. Times in nanoseconds.
 */
class RateWatch {
    private var voteSince = -1L
    private var rate = 0f
    private var rateSince = 0L
    private var quietSince = 0L

    /** The layers ask for the highest mode ([top]) from [now] on, or for a lower rate. */
    fun vote(top: Boolean, now: Long) {
        if (!top) voteSince = -1L else if (voteSince < 0) voteSince = now
    }

    /** The display's rate, as the app reads it, is [rate] at [now]. */
    fun reading(rate: Float, now: Long) {
        if (FrameRateVote.most(rate) == FrameRateVote.most(this.rate)) return
        this.rate = rate
        rateSince = now
    }

    /** Too few of the game's frames came by [now]: a lower rate counts only from then on. */
    fun quiet(now: Long) {
        quietSince = maxOf(quietSince, now)
    }

    /** The watch starts again at [now] (the surface is back): nothing read before counts. */
    fun restart(now: Long) {
        rate = 0f
        rateSince = now
        quiet(now)
    }

    /**
     * The measurement under the vote for the highest mode is done by [now] on a screen whose modes
     * go up to [highest]: the rate reached that mode and held [RAISE_NANOS], or a lower one held
     * [SETTLE_NANOS] while the game drew.
     */
    fun settled(highest: Float, now: Long): Boolean {
        if (voteSince < 0 || rate <= 0f) return false
        if (FrameRateVote.most(rate) >= FrameRateVote.most(highest)) return now - rateSince >= RAISE_NANOS
        return now - maxOf(voteSince, rateSince, quietSince) >= SETTLE_NANOS
    }

    /**
     * The full rate seen by [now] on a screen whose full rate is [full] and whose modes go up to
     * [highest]: a rate above [full] that held [RAISE_NANOS], or the rate the measurement [settled]
     * at; null: nothing new.
     */
    fun measured(full: Float, highest: Float, now: Long): Float? {
        if (rate <= 0f) return null
        val seen = rate.coerceAtMost(highest)
        val higher = FrameRateVote.most(seen) > FrameRateVote.most(full) && now - rateSince >= RAISE_NANOS
        return seen.takeIf { higher || settled(highest, now) }
    }

    companion object {
        const val SETTLE_NANOS = 2_000_000_000L
        const val RAISE_NANOS = 1_000_000_000L
    }
}

/**
 * The Game view's watch of its display's rate ([ScreenRate], [RateWatch]) while its surface shows
 * ([start] to [stop]), on the main thread; only on the built-in display, the one [ScreenRate]
 * keeps. It reads the rate when Android tells of a change (`DisplayManager.DisplayListener`, as
 * Android's guide to frame rates says, and again [RateWatch.RAISE_NANOS] later) and, while the
 * layers ask for the highest mode, every [CHECK_MS], when it also counts the buffers that went to
 * the compositor ([frames]). From the first [start] until that measurement is done ([measuring],
 * at most [MEASURE_LIMIT_MS]) the Game view asks for the highest mode whatever the player's rate,
 * so a reduced rate does not hide a change of the phone's settings from the next Play. [onChange]
 * runs when the full rate or [measuring] changed: the Game view sets the clock and its vote again.
 */
class ScreenRateWatch(
    private val context: Context,
    private val display: () -> Display?,
    private val frames: () -> Long?,
    private val onChange: () -> Unit,
) {
    private val displays = context.getSystemService(DisplayManager::class.java)
    private val handler = Handler(Looper.getMainLooper())
    private val rule = RateWatch()
    private var running = false
    private var startNanos = -1L
    private var top = false
    private var lastFrames: Long? = null
    private var lastFramesNanos = 0L
    private var logged = ""

    /** The measurement of the first [start] is not done: the Game view asks for the highest mode meanwhile. */
    var measuring = false
        private set

    private val listener = object : DisplayManager.DisplayListener {
        override fun onDisplayAdded(displayId: Int) {}

        override fun onDisplayRemoved(displayId: Int) {}

        override fun onDisplayChanged(displayId: Int) {
            if (displayId != Display.DEFAULT_DISPLAY) return
            check(counted = false)
            // A higher rate counts once it held: read it again then.
            handler.removeCallbacks(recheck)
            handler.postDelayed(recheck, RateWatch.RAISE_NANOS / 1_000_000 + 50)
        }
    }

    private val recheck = Runnable { check(counted = false) }

    private val tick = object : Runnable {
        override fun run() {
            check(counted = true)
            // Once: the check's change of the vote may have posted it already.
            handler.removeCallbacks(this)
            if (running && top) handler.postDelayed(this, CHECK_MS)
        }
    }

    // Posted, not called: [onChange] sets the vote, which comes back here.
    private val read = Runnable { check(counted = false) }

    /** The surface came: the first time, a measurement starts ([measuring]); what it read before does not count. */
    fun start() {
        if (running) return
        running = true
        val now = System.nanoTime()
        if (startNanos < 0) measuring = true
        // A measurement's time limit counts from the latest start: not the time away.
        if (measuring) startNanos = now
        // The time away (a still picture there, a lower or a higher rate) does not count.
        rule.restart(now)
        displays?.registerDisplayListener(listener, handler)
        restartTick()
    }

    /** The surface went: nothing is read until the next [start]. */
    fun stop() {
        if (!running) return
        running = false
        displays?.unregisterDisplayListener(listener)
        handler.removeCallbacks(tick)
        handler.removeCallbacks(read)
        handler.removeCallbacks(recheck)
    }

    /** The Game view's layers ask for the highest mode ([top]) from now on, or for a lower rate. */
    fun vote(top: Boolean) {
        rule.vote(top, System.nanoTime())
        if (top == this.top) return
        this.top = top
        restartTick()
    }

    private fun restartTick() {
        handler.removeCallbacks(tick)
        handler.removeCallbacks(read)
        if (!running) return
        lastFrames = frames()
        lastFramesNanos = System.nanoTime()
        handler.post(read)
        // Only the built-in display is measured ([check]).
        if (top && display()?.displayId == Display.DEFAULT_DISPLAY) handler.postDelayed(tick, CHECK_MS)
    }

    private fun check(counted: Boolean) {
        if (!running) return
        val display = display() ?: return
        if (display.displayId != Display.DEFAULT_DISPLAY) {
            // Another display (the Game view uses its highest mode there): nothing to measure.
            if (measuring) {
                measuring = false
                onChange()
            }
            return
        }
        val now = System.nanoTime()
        val rate = display.refreshRate
        rule.reading(rate, now)
        if (counted) {
            // Fewer than MIN_FPS buffers a second since the last count: the display may idle.
            val count = frames()
            val last = lastFrames
            if (count == null || last == null || (count - last) * 1_000_000_000L < MIN_FPS * (now - lastFramesNanos)) rule.quiet(now)
            lastFrames = count
            lastFramesNanos = now
        }
        val highest = FrameRateVote.highest(display)
        var changed = rule.measured(ScreenRate.full.value, highest, now)?.let { ScreenRate.measured(context, highest, it) } == true
        // Done, or no frames came for it (a still official launcher, a copy that did not start).
        if (measuring && (rule.settled(highest, now) || now - startNanos >= MEASURE_LIMIT_MS * 1_000_000L)) {
            measuring = false
            changed = true
        }
        log(rate, display.mode.refreshRate, highest)
        if (changed) onChange()
    }

    /** One line when what it reads changes: the rate the app reads, its mode's, the highest mode, the full rate, the vote. */
    private fun log(rate: Float, mode: Float, highest: Float) {
        val line = String.format(
            Locale.ROOT, "display %.1f (mode %.1f, modes up to %.1f), full rate %.1f, %s",
            rate, mode, highest, ScreenRate.full.value,
            if (measuring) "measuring" else if (top) "asks for the highest mode" else "asks for the player's rate",
        )
        if (line == logged) return
        logged = line
        Log.i(TAG, line)
    }

    companion object {
        /** How often it reads the rate and counts the buffers while the layers ask for the highest mode. */
        const val CHECK_MS = 500L

        /**
         * The buffers a second below which a lower rate does not count ([RateWatch.quiet]): below
         * the 25 frames a second of a game in the background, so that still counts as drawing.
         */
        const val MIN_FPS = 20L

        /** The longest first measurement: without frames it never ends ([measuring]). */
        const val MEASURE_LIMIT_MS = 60_000L

        private const val TAG = "pd2-screen-rate"
    }
}
