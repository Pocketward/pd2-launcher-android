package com.pd2launcher.app.input

import kotlin.math.hypot

/**
 * Touch on the Game view as a laptop's trackpad ([TouchMode.TRACKPAD]): a finger that goes down
 * never moves the pointer; a finger's moves move it by as much, times [speed] (the player's
 * Pointer speed, [Controls.pointerSpeed]: at 1.00× the pointer goes as far on the game's picture
 * as the finger on the screen). Positions are on the X screen (the view maps them, past its edges
 * too); [slop] is how far, in X screen pixels, a finger may wander and still tap. The pointer
 * moves by [RelayEvent.Relative] (x-input-relay keeps it inside the game's picture,
 * [PointerBounds]), and it goes on from where the pointer is: where a mouse, the controller's mouse
 * mode or the game left it.
 *
 * - **move**: one finger moves the pointer, no button held; a second finger that stays down when
 *   the first lifts goes on moving it
 * - **tap** (lifted within [TAP_MS]): a click of the primary button at the pointer, at once (no
 *   wait for a second touch)
 * - **a finger held still** does nothing: no long press (owner, 2026-10-28), and lifted after
 *   [TAP_MS] it was a resting finger, no tap; a finger that comes while it rests (a thumb that
 *   holds the phone on the game) does all this as if alone
 * - **tap, then touch and move**: a finger down within [SECOND_TOUCH_MS] of a tap's lift holds the
 *   primary button once it moves past the slop (the button first, then the move), or once it has
 *   stayed down [TouchGestures.TWO_FINGER_MS] with no second finger ([later] calls back then: a
 *   hold in place, as for an attack where the character stands); the button goes up as it lifts.
 *   Lifted before either, it was a second click (a double click). Only a tap opens this window,
 *   not a second click or a hold.
 * - **two-finger tap** (as [TouchGestures]: a second finger within [TouchGestures.TWO_FINGER_MS]
 *   of the first, both lifted within that time more, neither moved): a click of the other button
 *   at the pointer, also when the first finger came soon after a tap (no primary button then);
 *   the only way to the other button
 *
 * Diablo II takes a press as aimed at what was under the pointer a frame or two before
 * ([TouchGestures.HOVER_MS]). A finger going down never moves the pointer, so a click or a press
 * comes at once, unless this pointer moved less than that time before (a tap just after a move):
 * then it waits out the rest ([later] calls back then), and what comes before then (a release,
 * the next click) goes after it. A move sends it first, before the pointer moves; a new finger
 * does not (it moves nothing). A cancel drops it ([dropLater]).
 */
class TrackpadGestures(
    private val slop: Float,
    private val speed: () -> Float,
    private val emit: (List<RelayEvent>) -> Unit,
    private val later: (delayMs: Long, send: () -> Unit) -> Unit = { _, send -> send() },
    private val dropLater: () -> Unit = {},
) : TouchMouse {
    /**
     * AGAIN: a touch soon after a tap, its button not pressed yet; HELD: the primary button down
     * since; HELD_MOVE: it moved since.
     */
    private enum class Phase { IDLE, PENDING, MOVE, AGAIN, HELD, HELD_MOVE, TWO_FINGERS, DONE }

    private var phase = Phase.IDLE
    private var primaryId = -1
    private var startX = 0
    private var startY = 0
    private var startTime = 0L
    /** Touches so far: a hold's [later] call acts only for its own touch. */
    private var touches = 0
    /** Where the primary finger was last, for its next move. */
    private var lastX = 0
    private var lastY = 0
    /** The part of a pixel not yet sent ([speed] makes fractions). */
    private var restX = 0f
    private var restY = 0f
    /** When this pointer last moved; null: not yet. */
    private var movedAt: Long? = null
    /** When the last tap lifted; null: the last gesture was no tap. */
    private var tappedAt: Long? = null
    private var secondTime = 0L
    private var secondId = -1
    private var secondX = 0
    private var secondY = 0
    /** Where the second finger was last: in a move it goes on when the first lifts. */
    private var otherX = 0
    private var otherY = 0
    /** What waits for the hover time ([later]): a click or a press, and what came after it. */
    private var waiting: List<RelayEvent>? = null
    /** The [later] call that is the waiting events' own: an earlier one's sends nothing. */
    private var waitingCall = 0

    override fun down(pointerId: Int, x: Int, y: Int, timeMs: Long) {
        // A click still waiting stays: this finger moves nothing yet.
        val again = tappedAt?.let { timeMs - it <= SECOND_TOUCH_MS } == true
        end()
        touches++
        phase = if (again) Phase.AGAIN else Phase.PENDING
        primaryId = pointerId
        startX = x
        startY = y
        startTime = timeMs
        lastX = x
        lastY = y
        restX = 0f
        restY = 0f
        if (again) {
            val touch = touches
            later(TouchGestures.TWO_FINGER_MS) {
                if (touch == touches && phase == Phase.AGAIN) {
                    phase = Phase.HELD
                    afterHover(listOf(PRESS), startTime + TouchGestures.TWO_FINGER_MS)
                }
            }
        }
    }

    override fun secondDown(pointerId: Int, x: Int, y: Int, timeMs: Long) {
        // The gesture ended with its fingers while another one rests: this finger starts anew.
        if (phase == Phase.IDLE) {
            down(pointerId, x, y, timeMs)
            return
        }
        val soon = timeMs - startTime <= TouchGestures.TWO_FINGER_MS
        when {
            (phase == Phase.PENDING || phase == Phase.AGAIN) && soon -> {
                secondTime = timeMs
                secondId = pointerId
                secondX = x
                secondY = y
                phase = Phase.TWO_FINGERS
            }
            // The first finger rests (still, past the two-finger time) and does nothing: this one goes on.
            phase == Phase.PENDING -> {
                primaryId = pointerId
                startX = x
                startY = y
                startTime = timeMs
                lastX = x
                lastY = y
                restX = 0f
                restY = 0f
            }
            phase == Phase.MOVE -> {
                secondId = pointerId
                otherX = x
                otherY = y
            }
            // The first finger goes on: the other one adds nothing.
            phase == Phase.AGAIN || phase == Phase.HELD || phase == Phase.HELD_MOVE -> Unit
            else -> phase = Phase.DONE
        }
    }

    override fun move(pointerId: Int, x: Int, y: Int, timeMs: Long) {
        if (pointerId != primaryId) {
            if (pointerId == secondId) {
                if (phase == Phase.TWO_FINGERS && hypot((x - secondX).toFloat(), (y - secondY).toFloat()) > slop) phase = Phase.DONE
                otherX = x
                otherY = y
            }
            return
        }
        when (phase) {
            // Past the slop: the whole move from where the finger went down.
            Phase.PENDING -> if (moved(x, y)) {
                phase = Phase.MOVE
                moveBy(x, y, timeMs)
            }
            // The button first, then the move (which sends it at once, [moveBy]).
            Phase.AGAIN -> if (moved(x, y)) {
                phase = Phase.HELD_MOVE
                afterHover(listOf(PRESS), timeMs)
                moveBy(x, y, timeMs)
            }
            Phase.HELD -> if (moved(x, y)) {
                phase = Phase.HELD_MOVE
                moveBy(x, y, timeMs)
            }
            Phase.MOVE, Phase.HELD_MOVE -> moveBy(x, y, timeMs)
            Phase.TWO_FINGERS -> if (moved(x, y)) phase = Phase.DONE
            Phase.IDLE, Phase.DONE -> Unit
        }
    }

    /** No long press here: a finger held still does nothing. */
    override fun longPressDue(timeMs: Long) = Unit

    override fun up(timeMs: Long) {
        val tapped = when (phase) {
            // Down longer than a tap: a resting finger, no click.
            Phase.PENDING -> if (timeMs - startTime <= TAP_MS) {
                afterHover(click(RelayEvent.LEFT), timeMs)
                true
            } else {
                false
            }
            // Lifted before its button went down: a second click.
            Phase.AGAIN -> {
                afterHover(click(RelayEvent.LEFT), timeMs)
                false
            }
            Phase.HELD, Phase.HELD_MOVE -> {
                send(listOf(RELEASE))
                false
            }
            Phase.TWO_FINGERS -> {
                if (timeMs - secondTime <= TouchGestures.TWO_FINGER_MS) afterHover(click(RelayEvent.RIGHT), timeMs)
                false
            }
            Phase.MOVE, Phase.DONE -> false
            // The gesture ended already (its first finger lifted, [pointerUp]): this lift adds nothing.
            Phase.IDLE -> return
        }
        tappedAt = if (tapped) timeMs else null
        phase = Phase.IDLE
    }

    /**
     * The primary finger lifting ends the gesture; in a move, a second finger still down goes on
     * moving the pointer. A two-finger tap ends as both its fingers have lifted (a resting one may
     * stay down).
     */
    override fun pointerUp(pointerId: Int, timeMs: Long) {
        when {
            phase == Phase.TWO_FINGERS -> {
                if (pointerId == primaryId) primaryId = -1 else if (pointerId == secondId) secondId = -1
                if (primaryId == -1 && secondId == -1) up(timeMs)
            }
            phase == Phase.MOVE && pointerId == secondId -> secondId = -1
            pointerId != primaryId -> Unit
            phase == Phase.MOVE && secondId != -1 -> {
                primaryId = secondId
                secondId = -1
                lastX = otherX
                lastY = otherY
            }
            else -> up(timeMs)
        }
    }

    override fun cancel() {
        end()
        // No click after an overlay took the gesture, nor a press with its release.
        waiting = null
        dropLater()
        tappedAt = null
    }

    /** The fingers' gesture ends (the view's new size): a held button goes up; a click still to come stays. */
    override fun endTouch() {
        end()
        tappedAt = null
    }

    /** The touch ends with no lift: a held button goes up (after a press still waiting), the fingers are forgotten. */
    private fun end() {
        if (phase == Phase.HELD || phase == Phase.HELD_MOVE) send(listOf(RELEASE))
        phase = Phase.IDLE
        primaryId = -1
        secondId = -1
    }

    /** The pointer by the primary finger's move to [x], [y] since its last, times [speed]; what waits goes first. */
    private fun moveBy(x: Int, y: Int, timeMs: Long) {
        val factor = speed()
        restX += (x - lastX) * factor
        restY += (y - lastY) * factor
        lastX = x
        lastY = y
        val dx = restX.toInt()
        val dy = restY.toInt()
        restX -= dx
        restY -= dy
        if (dx == 0 && dy == 0) return
        sendWaiting()
        emit(listOf(RelayEvent.Relative(dx, dy)))
        movedAt = timeMs
    }

    /** A click or a press: at once where the pointer has hovered [TouchGestures.HOVER_MS], else after the rest of that time. */
    private fun afterHover(events: List<RelayEvent>, timeMs: Long) {
        val wait = movedAt?.let { TouchGestures.HOVER_MS - (timeMs - it) } ?: 0L
        if (waiting != null || wait <= 0) {
            send(events)
            return
        }
        waiting = events
        val call = ++waitingCall
        later(wait) { if (call == waitingCall) sendWaiting() }
    }

    /** [events] after what still waits, if anything; else now. */
    private fun send(events: List<RelayEvent>) {
        val before = waiting
        if (before != null) waiting = before + events else emit(events)
    }

    /** What still waits, if anything, now. */
    private fun sendWaiting() {
        val events = waiting ?: return
        waiting = null
        emit(events)
    }

    private fun moved(x: Int, y: Int) = hypot((x - startX).toFloat(), (y - startY).toFloat()) > slop

    private fun click(button: Int) = listOf(RelayEvent.Button(button, true), RelayEvent.Button(button, false))

    companion object {
        /** How soon after a tap's lift a finger going down holds the primary button (tap, then touch and move). */
        const val SECOND_TOUCH_MS = 300L

        /** The longest a finger held still may stay down and still tap; longer, it was resting. */
        const val TAP_MS = 500L

        private val PRESS = RelayEvent.Button(RelayEvent.LEFT, true)
        private val RELEASE = RelayEvent.Button(RelayEvent.LEFT, false)
    }
}
