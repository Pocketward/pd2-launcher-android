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

/**
 * Only text uses the reading: the logs, and the hint of the Frame rate input ([slowerThan]). It is
 * never an input of the clock ([FrameClock]) or of the request ([ScreenRequest]): the phone
 * changes it for its own reasons (a still picture, a touch, power saving), and an app that
 * follows it changes the screen's mode during play.
 */
object ScreenRate {
    private val _last = MutableStateFlow(0f)

    /** 0 before the first reading. */
    val last: StateFlow<Float> = _last.asStateFlow()

    internal fun set(rate: Float) {
        _last.value = rate
    }

    /** The whole rate for the hint; null where the screen shows every frame of a game at [pick], and before the first reading. */
    fun slowerThan(pick: Int, read: Float): Int? = Math.round(read).takeIf { read > 0f && it < pick }
}

class ScreenRateReader(context: Context, private val display: () -> Display?, private val onRead: (Float) -> Unit) {
    private val displays = context.getSystemService(DisplayManager::class.java)
    private val handler = Handler(Looper.getMainLooper())
    private var running = false
    private var logged = ""

    private val listener = object : DisplayManager.DisplayListener {
        override fun onDisplayAdded(displayId: Int) {}

        override fun onDisplayRemoved(displayId: Int) {}

        override fun onDisplayChanged(displayId: Int) {
            if (displayId == display()?.displayId) read()
        }
    }

    fun start() {
        if (running) return
        running = true
        displays?.registerDisplayListener(listener, handler)
        read()
    }

    fun stop() {
        if (!running) return
        running = false
        displays?.unregisterDisplayListener(listener)
    }

    private fun read() {
        val display = display() ?: return
        val rate = display.refreshRate
        val line = GameViewLines.screen(display.displayId, rate, display.mode.refreshRate)
        if (line == logged) return
        logged = line
        Log.i(TAG, line)
        GameViewLog.line(line)
        ScreenRate.set(rate)
        onRead(rate)
    }

    private companion object {
        const val TAG = "pd2-screen-rate"
    }
}
