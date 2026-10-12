package com.pd2launcher.app.input

import kotlin.math.abs
import kotlin.math.hypot

/**
 * The on-screen virtual controller: a left stick, the face buttons, shoulders,
 * triggers, d-pad, Back and Start, as one XInput state handed to [out] on every change; the Game
 * view merges it into the gamepad file with the real controller's ([PadInput.onVirtual]), so
 * PD2's own controller support sees one pad.
 */
class VirtualPad(private val out: (PadState) -> Unit) {
    private var state = PadState.NEUTRAL

    /** An XInput button ([GamepadLayout] bit) pressed or released. */
    fun button(bit: Int, down: Boolean) =
        set(state.copy(buttons = if (down) state.buttons or bit else state.buttons and bit.inv()))

    fun trigger(left: Boolean, down: Boolean) {
        val value = if (down) 255 else 0
        set(if (left) state.copy(leftTrigger = value) else state.copy(rightTrigger = value))
    }

    /**
     * The left stick at the finger's offset [x], [y] from the ring's centre, in ring radii (down
     * positive as on screen); see [thumbs].
     */
    fun stick(x: Float, y: Float) {
        val (lx, ly) = thumbs(x, y)
        set(state.copy(lx = lx, ly = ly))
    }

    /** Everything let go (the controls hidden or the menu opened). */
    fun releaseAll() = set(PadState.NEUTRAL)

    private fun set(next: PadState) {
        if (next == state) return
        state = next
        out(next)
    }

    companion object {
        /** Offsets this close to the centre (in ring radii) are the stick at rest. */
        const val DEAD_ZONE = 0.08f

        /**
         * A finger offset (ring radii, down positive) as XInput left-stick values (up positive),
         * fully analog and radial: the length is taken past the [DEAD_ZONE], rescaled to 0..1 and
         * clamped at the ring, the direction kept as is, so 45 degrees at the rim is 0.707 of
         * full scale on both axes.
         */
        fun thumbs(x: Float, y: Float): Pair<Int, Int> {
            val length = hypot(x, y)
            if (length <= DEAD_ZONE) return 0 to 0
            val scale = ((length - DEAD_ZONE) / (1f - DEAD_ZONE)).coerceAtMost(1f) / length
            return PadInput.thumb(x * scale) to PadInput.thumb(-y * scale)
        }
    }
}

/**
 * The player's custom on-screen buttons into X through the relay: a key or a mouse button held
 * while the finger is on it, the wheel one click per press. Held by piece id, so letting go
 * releases what was pressed even if the binding changed meanwhile.
 */
class CustomButtons(private val send: (List<RelayEvent>) -> Unit) {
    private val held = LinkedHashMap<String, PadAction>()

    fun press(id: String, action: PadAction) {
        val events = events(action, down = true)
        if (events.isEmpty() || id in held) return
        held[id] = action
        send(events)
    }

    fun release(id: String) {
        val action = held.remove(id) ?: return
        events(action, down = false).takeIf { it.isNotEmpty() }?.let(send)
    }

    /** Lets go of everything held (the controls were hidden or the game view left). */
    fun releaseAll() {
        if (held.isEmpty()) return
        val events = held.values.flatMap { events(it, down = false) }
        held.clear()
        if (events.isNotEmpty()) send(events)
    }

    companion object {
        /** What pressing ([down]) or letting go of a button bound to [action] sends. */
        fun events(action: PadAction, down: Boolean): List<RelayEvent> = when (action) {
            is PadAction.Key -> listOf(RelayEvent.Key(action.keysym, down))
            is PadAction.Mouse -> when {
                action.button == RelayEvent.WHEEL_UP || action.button == RelayEvent.WHEEL_DOWN ->
                    if (down) listOf(RelayEvent.Button(action.button, true), RelayEvent.Button(action.button, false)) else emptyList()
                else -> listOf(RelayEvent.Button(action.button, down))
            }
            PadAction.Pad, PadAction.None -> emptyList()
        }
    }
}

/** How a finger on the d-pad and on A/B/X/Y picks its buttons. */
object PadGroups {
    /** Offsets this close to the centre (in pad radii) press no direction. */
    const val DPAD_DEAD_ZONE = 0.25f

    /** tan(22.5°): within this slope of an axis, a finger presses only that axis' direction. */
    private const val SECTOR_SLOPE = 0.41421357f

    /**
     * The d-pad's [GamepadLayout] bits for a finger at [x], [y] from its centre (pad radii, down
     * positive): eight 45-degree sectors, so a corner presses both of its directions.
     */
    fun dpad(x: Float, y: Float): Int {
        if (hypot(x, y) <= DPAD_DEAD_ZONE) return 0
        var bits = 0
        if (abs(y) > abs(x) * SECTOR_SLOPE) bits = bits or if (y < 0) GamepadLayout.DPAD_UP else GamepadLayout.DPAD_DOWN
        if (abs(x) > abs(y) * SECTOR_SLOPE) bits = bits or if (x < 0) GamepadLayout.DPAD_LEFT else GamepadLayout.DPAD_RIGHT
        return bits
    }

    /**
     * The face button under a finger at [x], [y] from the cluster's centre (down positive), laid
     * out as on an Xbox pad (Y top, X left, B right, A bottom): the nearest by direction, so the
     * whole square is the four buttons' touch area.
     */
    fun face(x: Float, y: Float): Int = if (abs(x) > abs(y)) {
        if (x < 0) GamepadLayout.X else GamepadLayout.B
    } else {
        if (y < 0) GamepadLayout.Y else GamepadLayout.A
    }

    /** The gap between neighbouring buttons in a group, a fraction of the group's side. */
    const val GAP = 0.03f

    /**
     * A/B/X/Y as a tight diamond in a square of side 1: each button's diameter and its centre's
     * distance from the square's centre, so neighbours are [GAP] apart and the outer ones touch
     * the square's sides.
     */
    val FACE_DIAMETER: Float = (0.5f - GAP / SQRT2) / (1f / SQRT2 + 0.5f)
    val FACE_REACH: Float = (FACE_DIAMETER + GAP) / SQRT2
}

private const val SQRT2 = 1.4142135f
