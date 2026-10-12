package com.pd2launcher.app.input

import kotlin.math.hypot

/**
 * Touch on the Game view as the X screen's mouse. Positions are on the X screen already (the
 * view maps them); [slop] is how far, in X screen pixels, a finger may wander and still tap.
 *
 * - **tap**: a click of the primary button where the finger was
 * - **long press** (held still for [LONG_PRESS_MS]; the view calls [longPressDue] then): a
 *   click of the other button there
 * - **two-finger tap** (a second finger within [TWO_FINGER_MS] of the first, both lifted within
 *   [TWO_FINGER_MS] more, neither moved): a click of the other button at the first finger
 * - **drag**: the left button held from where the finger went down, the pointer following it
 *
 * The pointer moves to every finger that goes down, so the game sees hover first, as from a
 * mouse. Diablo II takes a press as aimed at what was under the pointer a frame or two before,
 * so a click comes at least [HOVER_MS] after the pointer got there: a quicker tap's click goes
 * to [later] with the time still to wait.
 */
class TouchGestures(
    private val slop: Float,
    private val emit: (List<RelayEvent>) -> Unit,
    private val later: (delayMs: Long, events: List<RelayEvent>) -> Unit = { _, events -> emit(events) },
) {
    private enum class Phase { IDLE, PENDING, DRAG, TWO_FINGERS, DONE }

    private var phase = Phase.IDLE
    private var startX = 0
    private var startY = 0
    private var startTime = 0L
    private var secondTime = 0L

    /** The first finger went down. */
    fun down(x: Int, y: Int, timeMs: Long) {
        cancel()
        phase = Phase.PENDING
        startX = x
        startY = y
        startTime = timeMs
        emit(listOf(RelayEvent.Motion(x, y)))
    }

    /** Another finger went down while one is already down. */
    fun secondDown(timeMs: Long) {
        phase = when {
            phase == Phase.PENDING && timeMs - startTime <= TWO_FINGER_MS -> {
                secondTime = timeMs
                Phase.TWO_FINGERS
            }
            phase == Phase.DRAG -> Phase.DRAG
            else -> Phase.DONE
        }
    }

    /** The first finger moved. */
    fun move(x: Int, y: Int, timeMs: Long) {
        when (phase) {
            Phase.PENDING -> if (moved(x, y)) {
                phase = Phase.DRAG
                emit(listOf(RelayEvent.Button(RelayEvent.LEFT, true), RelayEvent.Motion(x, y)))
            }
            Phase.DRAG -> emit(listOf(RelayEvent.Motion(x, y)))
            Phase.TWO_FINGERS -> if (moved(x, y)) phase = Phase.DONE
            Phase.IDLE, Phase.DONE -> Unit
        }
        if (phase == Phase.PENDING && timeMs - startTime >= LONG_PRESS_MS) longPressDue(timeMs)
    }

    /** The view's long-press timer ran out ([LONG_PRESS_MS] after [down]). */
    fun longPressDue(timeMs: Long) {
        if (phase != Phase.PENDING || timeMs - startTime < LONG_PRESS_MS) return
        phase = Phase.DONE
        emit(click(RelayEvent.RIGHT))
    }

    /** The last finger went up. */
    fun up(timeMs: Long) {
        when (phase) {
            Phase.PENDING -> clickAfterHover(RelayEvent.LEFT, timeMs)
            Phase.DRAG -> emit(listOf(RelayEvent.Button(RelayEvent.LEFT, false)))
            Phase.TWO_FINGERS -> if (timeMs - secondTime <= TWO_FINGER_MS) clickAfterHover(RelayEvent.RIGHT, timeMs)
            Phase.IDLE, Phase.DONE -> Unit
        }
        phase = Phase.IDLE
    }

    /** The gesture was taken away (`ACTION_CANCEL`): a held button is released. */
    fun cancel() {
        if (phase == Phase.DRAG) emit(listOf(RelayEvent.Button(RelayEvent.LEFT, false)))
        phase = Phase.IDLE
    }

    private fun clickAfterHover(button: Int, timeMs: Long) {
        val wait = HOVER_MS - (timeMs - startTime)
        if (wait > 0) later(wait, click(button)) else emit(click(button))
    }

    private fun moved(x: Int, y: Int) = hypot((x - startX).toFloat(), (y - startY).toFloat()) > slop

    private fun click(button: Int) = listOf(
        RelayEvent.Motion(startX, startY),
        RelayEvent.Button(button, true),
        RelayEvent.Button(button, false),
    )

    companion object {
        const val LONG_PRESS_MS = 500L
        const val TWO_FINGER_MS = 300L

        /** The least time between the pointer reaching a spot and a click there. */
        const val HOVER_MS = 80L
    }
}
