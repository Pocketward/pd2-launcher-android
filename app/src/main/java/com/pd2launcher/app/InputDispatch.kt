package com.pd2launcher.app

import android.view.InputDevice
import java.io.File

/**
 * How Android gives the Game view its motion events (issue #9; the owner, 2026-10-30: "implement
 * the unbuffer input dispatch"). By default Android gives an app the motion events of a source
 * once a display frame ("buffered": the samples that came since the frame before come together,
 * as one event with history), which can add up to one display frame to each move. On request it
 * gives each event as it comes ("unbuffered"). While the Game view is in its window, it asks for
 * unbuffered dispatch of [SOURCES]. Debug builds keep the buffered dispatch while the file
 * [SessionPaths.bufferedInputForTest] is there, which the Game view reads each time it opens, so
 * that one APK can measure both. Release builds always ask; another release default is a change
 * of [buffered].
 */
object InputDispatch {
    /**
     * The touch screen, a mouse and a touchpad (Android's pointer class), and the controllers'
     * sticks, triggers and hat (the joystick class). Key events are never buffered.
     */
    const val SOURCES = InputDevice.SOURCE_CLASS_POINTER or InputDevice.SOURCE_CLASS_JOYSTICK

    /**
     * Whether the Game view keeps Android's buffered dispatch: only in a [debug] build, and only
     * while the debug switch [switch] is there. A release build does not look for the file.
     */
    fun buffered(debug: Boolean, switch: () -> File): Boolean = debug && switch().exists()

    /** The source classes the Game view asks Android for unbuffered dispatch of: none when [buffered]. */
    fun sources(buffered: Boolean): Int = if (buffered) InputDevice.SOURCE_CLASS_NONE else SOURCES
}
