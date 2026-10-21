package com.pd2launcher.app.input

import android.view.MotionEvent
import kotlin.math.abs

/**
 * A mouse on the Game view, passed 1:1 to the X screen, with no gestures: every event moves the
 * pointer to where the mouse is (the view maps it to X screen pixels, as for touch), a change of
 * Android's buttons presses or releases X button 1 (primary), 2 (tertiary) or 3 (secondary), and
 * each step of the wheel is a click of X button 4 (up) or 5 (down).
 */
class Mouse {
    /** Android's buttons (`MotionEvent.BUTTON_*`) that X holds pressed. */
    private var held = 0
    /** The wheel's turn not yet a whole step (a high-resolution wheel reports parts of one). */
    private var wheelRest = 0f

    /** A mouse event at [x], [y] on the X screen with Android's [buttons] and [wheel] (`AXIS_VSCROLL`, up positive). */
    fun events(x: Int, y: Int, buttons: Int, wheel: Float = 0f): List<RelayEvent> {
        val events = mutableListOf<RelayEvent>(RelayEvent.Motion(x, y))
        events += press(buttons)
        // A turn the other way starts a new step.
        if (wheel * wheelRest < 0f) wheelRest = 0f
        wheelRest += wheel
        val steps = wheelRest.toInt()
        wheelRest -= steps
        val step = if (steps > 0) RelayEvent.WHEEL_UP else RelayEvent.WHEEL_DOWN
        repeat(abs(steps)) {
            events += RelayEvent.Button(step, true)
            events += RelayEvent.Button(step, false)
        }
        return events
    }

    /** The releases of the buttons X holds, which X then holds no more. */
    fun release(): List<RelayEvent> = press(0)

    /** Presses or releases the X buttons whose Android buttons differ between [held] and [buttons]; X then holds [buttons]. */
    private fun press(buttons: Int): List<RelayEvent> {
        val changes = BUTTONS.filter { (android, _) -> (buttons xor held) and android != 0 }
            .map { (android, button) -> RelayEvent.Button(button, buttons and android != 0) }
        held = buttons
        return changes
    }

    private companion object {
        val BUTTONS = listOf(
            MotionEvent.BUTTON_PRIMARY to RelayEvent.LEFT,
            MotionEvent.BUTTON_TERTIARY to RelayEvent.MIDDLE,
            MotionEvent.BUTTON_SECONDARY to RelayEvent.RIGHT,
        )
    }
}
