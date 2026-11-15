package com.pd2launcher.app

import android.content.Context
import android.os.Handler
import android.os.Looper
import android.util.Log
import android.view.Choreographer
import android.view.View
import android.view.Window
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

/** The last [ScreenCapability] measured, in the app's `screen_capability` preferences, and as a flow that the Frame rate input follows. */
object ScreenCapabilityStore {
    private const val PREFERENCES = "screen_capability"
    private const val HERTZ = "hertz"
    private const val PANEL = "panel"
    private const val TAG = "pd2-screen-rate"
    private val _measured = MutableStateFlow<ScreenCapability?>(null)

    /** The stored measurement, and each new one. */
    fun measured(context: Context): StateFlow<ScreenCapability?> {
        load(context)
        return _measured.asStateFlow()
    }

    fun load(context: Context): ScreenCapability? {
        val kept = context.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE)
        val panel = kept.getString(PANEL, null)
        return panel?.let { ScreenCapability(kept.getInt(HERTZ, 0), it) }?.takeIf { it.hertz > 0 }.also { _measured.value = it }
    }

    fun upTo(context: Context, modes: PanelModes): Int = ScreenCapability.upTo(load(context), modes, PhoneRefreshCap.read(context))

    fun held(context: Context, modes: PanelModes, hertz: Int, asksHighest: Boolean) {
        val before = load(context)
        val cap = PhoneRefreshCap.read(context)
        val next = ScreenCapability.after(before, modes, cap, hertz, asksHighest)
        if (next != null) {
            context.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE).edit().putInt(HERTZ, next.hertz).putString(PANEL, next.panel).apply()
            _measured.value = next
        }
        log(GameViewLines.capability(hertz, asksHighest, ScreenCapability.upTo(before, modes, cap), ScreenCapability.upTo(next ?: before, modes, cap)))
    }

    fun noRate(asksHighest: Boolean) = log(GameViewLines.noCapability(asksHighest))

    private fun log(line: String) {
        Log.i(TAG, line)
        GameViewLog.line(line)
    }
}

/**
 * One measurement of the [ScreenCapability] of [view]'s display, on the main thread: it reads the
 * screen's rate every [READ_MS] until a rate settles ([SettledRate]) or the time is over.
 *
 * With a [window] (the Frame rate input in Settings, where no game shows) the window asks for the
 * panel's highest mode while it measures, and [view] draws a frame on each display frame: on some
 * phones Android lowers the rate of a still screen. Without one (the Game view) nothing is asked:
 * the game's own request stays, so the screen's mode does not change during a game.
 */
class ScreenCapabilityProbe(private val view: View, private val window: Window?) {
    private val handler = Handler(Looper.getMainLooper())
    private var settle: SettledRate? = null
    private var modes = PanelModes(emptyList())
    private val tick = Runnable { read() }
    private val frame = Choreographer.FrameCallback { draw() }

    fun start() {
        if (settle != null) return
        val display = view.display ?: return
        modes = PanelModes.of(display)
        val highest = if (window != null) modes.highest ?: return else null
        settle = SettledRate(System.nanoTime())
        if (window != null && highest != null) {
            window.attributes = window.attributes.also { it.preferredDisplayModeId = highest.id }
            draw()
        }
        read()
    }

    fun stop() {
        settle ?: return
        settle = null
        handler.removeCallbacks(tick)
        Choreographer.getInstance().removeFrameCallback(frame)
        window?.let { it.attributes = it.attributes.also { attributes -> attributes.preferredDisplayModeId = 0 } }
    }

    private fun read() {
        val settle = settle ?: return
        val now = System.nanoTime()
        view.display?.let { settle.reading(it.refreshRate, now) }
        val hertz = settle.settled(now)
        when {
            hertz != null -> {
                stop()
                ScreenCapabilityStore.held(view.context, modes, hertz, asksHighest = window != null)
            }
            settle.over(now) -> {
                stop()
                ScreenCapabilityStore.noRate(asksHighest = window != null)
            }
            else -> handler.postDelayed(tick, READ_MS)
        }
    }

    private fun draw() {
        settle ?: return
        view.invalidate()
        Choreographer.getInstance().postFrameCallback(frame)
    }

    private companion object {
        const val READ_MS = 250L
    }
}
