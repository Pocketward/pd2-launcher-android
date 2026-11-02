package com.pd2launcher.app

import android.view.InputDevice
import com.pd2launcher.app.input.Chord
import com.pd2launcher.app.input.TouchMode
import com.pd2launcher.app.input.isController
import java.util.Locale

/** One input device as [InputSources] counts it: Android's [sources] bits, whether it is [virtual], and whether its keys are a keyboard's letters ([alphabetic]). */
data class InputFacts(val sources: Int, val virtual: Boolean, val alphabetic: Boolean) {
    companion object {
        fun of(device: InputDevice) = InputFacts(device.sources, device.isVirtual, device.keyboardType == InputDevice.KEYBOARD_TYPE_ALPHABETIC)
    }
}

/**
 * The input of a Play and of a press of the official launcher, for the service log beside the
 * settings line: the kinds of input device there are now (a touch screen; mice, touchpads and
 * other pointer devices as "mouse"; controllers; keyboards with letters), by kind and count only,
 * never a device's name or id; and what can move the pointer with them: touch as the Touch setting
 * says (Trackpad or Direct), a mouse 1:1, a controller in mouse mode (its chord switches it). The
 * Game view's log says what moved it first ([GameViewLines.firstMove]).
 */
object InputSources {
    fun line(devices: List<InputFacts>, touch: TouchMode, chord: Chord): String {
        val real = devices.filter { !it.virtual }
        val touchScreen = real.any { has(it.sources, InputDevice.SOURCE_TOUCHSCREEN) }
        val controllers = real.count { isController(it.sources, it.virtual) }
        val mice = real.count { !isController(it.sources, it.virtual) && pointer(it.sources) }
        val keyboards = real.count {
            !isController(it.sources, it.virtual) && !pointer(it.sources) && it.alphabetic && has(it.sources, InputDevice.SOURCE_KEYBOARD)
        }
        val kinds = listOf(
            if (touchScreen) "touch screen" else "no touch screen",
            count(mice, "mouse", "mice"),
            count(controllers, "controller", "controllers"),
            count(keyboards, "keyboard", "keyboards"),
        )
        val movers = listOfNotNull(
            if (touchScreen) "touch (${SettingsLog.touch(touch)})" else null,
            if (mice > 0) "the mouse" else null,
            if (controllers > 0) "a controller in mouse mode (${SettingsLog.chord(chord)})" else null,
        )
        return "input: ${kinds.joinToString()}; the pointer moves by ${movers.ifEmpty { listOf("nothing") }.joinToString()}"
    }

    private fun has(sources: Int, source: Int) = sources and source == source

    /** A device that moves a pointer as a mouse does: a mouse, a mouse that sends moves only, or a touchpad. */
    private fun pointer(sources: Int) =
        has(sources, InputDevice.SOURCE_MOUSE) || has(sources, InputDevice.SOURCE_MOUSE_RELATIVE) || has(sources, InputDevice.SOURCE_TOUCHPAD)

    private fun count(n: Int, one: String, many: String) = when (n) {
        0 -> "no $one"
        1 -> "1 $one"
        else -> String.format(Locale.ROOT, "%d %s", n, many)
    }
}

/** What moved the pointer, as the Game view's and the input log's lines name it. */
enum class MoveSource(val log: String, val firstMove: String) {
    TRACKPAD("Trackpad", "touch (Trackpad)"),
    DIRECT("Direct", "touch (Direct)"),
    MOUSE("mouse", "the mouse"),
    CONTROLLER("controller", "a controller in mouse mode"),
}

/**
 * Detailed logging's input delay in `input.log`: for each pointer move, the time from Android's
 * time of its input ([add]'s delay: the MotionEvent's event time, or the controller's last stick
 * event, to the write of the move into x-input-relay's socket), as one line for each source each
 * [WINDOW_NANOS] while the pointer moves, never a line for each move: the count, the median, the
 * 95th percentile and the longest, in ms. A window starts with its first move; it ends at the first
 * move [WINDOW_NANOS] or more after that, or at [flush], and its lines say how long its moves went
 * on. Thread-safe: the Game view's sending thread adds, its end flushes.
 */
class MoveDelays {
    private var since = 0L
    private var last = 0L
    private val delays = HashMap<MoveSource, MutableList<Long>>()

    /** A move from [source] whose input came [delay] ns before its write at [now] (ns); returns the lines of a window that ended. */
    @Synchronized
    fun add(source: MoveSource, delay: Long, now: Long): List<String> {
        val ended = if (delays.isNotEmpty() && now - since >= WINDOW_NANOS) lines() else emptyList()
        if (delays.isEmpty()) since = now
        last = now
        delays.getOrPut(source) { ArrayList() } += delay.coerceAtLeast(0)
        return ended
    }

    /** The lines of the window that goes on, if any (the Game view closes). */
    @Synchronized
    fun flush(): List<String> = if (delays.isEmpty()) emptyList() else lines()

    private fun lines(): List<String> {
        val span = (last - since) / 1e9
        val out = MoveSource.entries.mapNotNull { source ->
            val all = delays[source]?.sorted() ?: return@mapNotNull null
            String.format(
                Locale.ROOT, "pointer moves in %.1f s: %s %d, from the input to the relay's socket: median %.1f ms, p95 %.1f ms, max %.1f ms",
                span, source.log, all.size, rank(all, 0.5) / 1e6, rank(all, 0.95) / 1e6, all.last() / 1e6,
            )
        }
        delays.clear()
        return out
    }

    companion object {
        const val WINDOW_NANOS = 5_000_000_000L

        /** The value at [fraction] of [sorted] by the nearest rank. */
        fun rank(sorted: List<Long>, fraction: Double): Long = sorted[(Math.ceil(fraction * sorted.size).toInt() - 1).coerceIn(0, sorted.size - 1)]
    }
}
