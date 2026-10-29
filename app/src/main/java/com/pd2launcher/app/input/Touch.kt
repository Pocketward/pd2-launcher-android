package com.pd2launcher.app.input

import kotlin.math.hypot

/**
 * How touch on the game moves the X screen's pointer: the player's Touch setting
 * (`AppSettings.touchMode`), in the order the screens offer them.
 */
enum class TouchMode {
    /** The pointer moves by the finger's moves, as on a laptop's trackpad ([TrackpadGestures]); the default. */
    TRACKPAD,

    /** The pointer goes to the finger ([TouchGestures]). */
    DIRECT,
}

/**
 * Touch on the Game view as the X screen's mouse, as the view calls it: [TouchGestures] for
 * [TouchMode.DIRECT], [TrackpadGestures] for [TouchMode.TRACKPAD]. Positions are on the X screen
 * (the view maps them); times are the events' own.
 */
interface TouchMouse {
    /** The first finger went down. */
    fun down(pointerId: Int, x: Int, y: Int, timeMs: Long)

    /** Another finger went down while one is already down. */
    fun secondDown(pointerId: Int, x: Int, y: Int, timeMs: Long)

    /** A tracked finger moved, including samples batched into a later Android event. */
    fun move(pointerId: Int, x: Int, y: Int, timeMs: Long)

    /** The view's long-press timer ran out ([TouchGestures.LONG_PRESS_MS] after [down]). */
    fun longPressDue(timeMs: Long)

    /** The last finger went up. */
    fun up(timeMs: Long)

    /** A finger went up while another stays down. */
    fun pointerUp(pointerId: Int, timeMs: Long)

    /** The gesture was taken away (`ACTION_CANCEL`): a held button is released, a click still to come dropped. */
    fun cancel()

    /** The view took a new size under the fingers: their gesture ends there (by default as [cancel]). */
    fun endTouch() = cancel()
}

/**
 * Touch on the Game view as the X screen's mouse, the pointer at the finger ([TouchMode.DIRECT]).
 * Positions are on the X screen already (the view maps them); [slop] is how far, in X screen
 * pixels, a finger may wander and still tap.
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
 * so a click comes at least [HOVER_MS] after the pointer got there: a quicker tap's click waits
 * out the rest ([later] calls back then). A new finger sends a click still waiting at once,
 * before the pointer moves to it: one left to come after would move the pointer back to the old
 * place in the middle of the new gesture, and one dropped would lose a fast double tap's first
 * click. A cancel drops it ([dropLater]): no click reaches the game after an overlay took it.
 */
class TouchGestures(
    private val slop: Float,
    private val emit: (List<RelayEvent>) -> Unit,
    private val later: (delayMs: Long, send: () -> Unit) -> Unit = { _, send -> send() },
    private val dropLater: () -> Unit = {},
) : TouchMouse {
    private enum class Phase { IDLE, PENDING, DRAG, TWO_FINGERS, DONE }

    private var phase = Phase.IDLE
    private var primaryId = -1
    private var startX = 0
    private var startY = 0
    private var startTime = 0L
    private var secondTime = 0L
    private var secondId = -1
    private var secondX = 0
    private var secondY = 0
    /** A tap's click waiting for the hover time ([later]). */
    private var waiting: List<RelayEvent>? = null

    /** The first finger went down. */
    override fun down(pointerId: Int, x: Int, y: Int, timeMs: Long) {
        sendWaiting()
        cancel()
        phase = Phase.PENDING
        primaryId = pointerId
        startX = x
        startY = y
        startTime = timeMs
        emit(listOf(RelayEvent.Motion(x, y)))
    }

    /** Another finger went down while one is already down. */
    override fun secondDown(pointerId: Int, x: Int, y: Int, timeMs: Long) {
        phase = when {
            phase == Phase.PENDING && timeMs - startTime <= TWO_FINGER_MS -> {
                secondTime = timeMs
                secondId = pointerId
                secondX = x
                secondY = y
                Phase.TWO_FINGERS
            }
            phase == Phase.DRAG -> Phase.DRAG
            else -> Phase.DONE
        }
    }

    /** A tracked finger moved, including samples batched into a later Android event. */
    override fun move(pointerId: Int, x: Int, y: Int, timeMs: Long) {
        if (pointerId != primaryId) {
            if (pointerId == secondId && phase == Phase.TWO_FINGERS &&
                hypot((x - secondX).toFloat(), (y - secondY).toFloat()) > slop
            ) phase = Phase.DONE
            return
        }
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
    override fun longPressDue(timeMs: Long) {
        if (phase != Phase.PENDING || timeMs - startTime < LONG_PRESS_MS) return
        phase = Phase.DONE
        emit(click(RelayEvent.RIGHT))
    }

    /** The last finger went up. */
    override fun up(timeMs: Long) {
        when (phase) {
            Phase.PENDING -> clickAfterHover(RelayEvent.LEFT, timeMs)
            Phase.DRAG -> emit(listOf(RelayEvent.Button(RelayEvent.LEFT, false)))
            Phase.TWO_FINGERS -> if (timeMs - secondTime <= TWO_FINGER_MS) clickAfterHover(RelayEvent.RIGHT, timeMs)
            Phase.IDLE, Phase.DONE -> Unit
        }
        phase = Phase.IDLE
    }

    /** The primary finger lifting ends a drag. A two-finger tap waits for the last finger. */
    override fun pointerUp(pointerId: Int, timeMs: Long) {
        if (pointerId == primaryId && phase != Phase.TWO_FINGERS) up(timeMs)
    }

    /** The gesture was taken away (`ACTION_CANCEL`): a held button is released, a click still to come dropped. */
    override fun cancel() {
        waiting = null
        dropLater()
        if (phase == Phase.DRAG) emit(listOf(RelayEvent.Button(RelayEvent.LEFT, false)))
        phase = Phase.IDLE
        primaryId = -1
        secondId = -1
    }

    private fun clickAfterHover(button: Int, timeMs: Long) {
        val wait = HOVER_MS - (timeMs - startTime)
        if (wait <= 0) {
            emit(click(button))
            return
        }
        waiting = click(button)
        later(wait, ::sendWaiting)
    }

    /** The click still waiting, if any, now. */
    private fun sendWaiting() {
        val events = waiting ?: return
        waiting = null
        emit(events)
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
