package com.pd2launcher.app.input

import kotlin.math.hypot
import kotlin.math.roundToInt

/**
 * The controller's axes as Android reports them (`MotionEvent.getAxisValue`): sticks -1..1 with
 * down positive, triggers 0..1 (the larger of `AXIS_LTRIGGER`/`AXIS_BRAKE` and of
 * `AXIS_RTRIGGER`/`AXIS_GAS`), the hat -1..1 (`AXIS_HAT_X`/`AXIS_HAT_Y`).
 */
data class PadAxes(
    val lx: Float = 0f,
    val ly: Float = 0f,
    val rx: Float = 0f,
    val ry: Float = 0f,
    val leftTrigger: Float = 0f,
    val rightTrigger: Float = 0f,
    val hatX: Float = 0f,
    val hatY: Float = 0f,
)

/**
 * The controller in the Game view. Normally it is the game's XInput pad: every change goes to
 * [pad] (the gamepad file). The chord ([Controls.chord],
 * Select + Start by default) pressed briefly switches to **mouse mode** and back: the game then
 * sees a neutral, still connected pad, and the controller drives X through [relay]: the sticks
 * move the pointer ([tick]) and the buttons do what [Controls.mouseMode] binds them to (by
 * default A or L2 the left button, B or R2 the right one, X Enter, Y Alt, the d-pad the arrow
 * keys, Start Escape, Select Tab, L1/R1 the wheel). The chord held for [MENU_HOLD_MS] calls
 * [onMenu] instead (the overlay menu); the mode switches when the chord is let go. In pad mode
 * a button may be bound to an X key or mouse button too ([Controls.padMode]).
 *
 * The chord's buttons are buttons of their own too: bound to the pad, the game sees them while
 * they are held, until the chord forms; bound to a key or mouse button, they act once when
 * released without having been part of the chord.
 */
class PadInput(
    private val pad: (PadState) -> Unit,
    private val relay: (List<RelayEvent>) -> Unit,
    private val onMenu: () -> Unit = {},
    controls: Controls = Controls(),
) {
    /** The bindings in use; a change applies at once (what the old ones held is released). */
    var controls: Controls = controls
        set(value) {
            if (value == field) return
            if (value.chord != field.chord) resetChord()
            field = value
            val out = ArrayList<RelayEvent>()
            update(out)
            flush(out)
        }

    var mouseMode = false
        private set

    private val held = HashSet<PadButton>() // digital buttons down
    private var axes = PadAxes()
    private var chordFormed = false // both chord buttons went down together and neither went up yet
    private var chordHeldMs = 0L
    private var chordMenu = false // this chord opened the menu: its release does not switch the mode
    private val inChord = HashSet<PadButton>() // chord buttons that were part of a chord, until released
    private var sent: PadState? = null
    private var virtual = PadState.NEUTRAL

    // What is pressed in X, so a change of mode or binding releases it.
    private var active = emptySet<PadButton>()
    private val buttonsDown = HashSet<Int>()
    private val keysDown = HashSet<Int>()
    private var restX = 0f
    private var restY = 0f

    /**
     * A controller key; returns false for a key this class does not map (the caller still
     * consumes every key from a gamepad, so none reaches Android's Back).
     */
    fun onKey(keyCode: Int, down: Boolean): Boolean {
        val button = PadButton.of(keyCode) ?: return false
        val out = ArrayList<RelayEvent>()
        val was = button in held
        if (down) held += button else held -= button
        val chord = controls.chord
        if (button == chord.first || button == chord.second) {
            val other = if (button == chord.first) chord.second else chord.first
            if (down && !was && other in held) {
                chordFormed = true
                chordHeldMs = 0
                chordMenu = false
                inChord += button
                inChord += other
            } else if (!down && was) {
                if (chordFormed) {
                    chordFormed = false
                    if (!chordMenu) toggle()
                } else if (button !in inChord) {
                    tapAction(controls.action(button, mouseMode), out)
                }
                inChord -= button
            }
        }
        update(out)
        flush(out)
        return true
    }

    /**
     * The on-screen virtual controller's state, merged into what the game's pad shows (in either
     * mode): buttons and triggers add up, and its sticks count while the real ones rest.
     */
    fun onVirtual(state: PadState) {
        virtual = state
        flush(emptyList())
    }

    /** New axis values from a joystick motion event. */
    fun onAxes(next: PadAxes) {
        axes = next
        val out = ArrayList<RelayEvent>()
        update(out)
        flush(out)
    }

    /**
     * Call once per display frame with the time since the last one: the chord held long enough
     * opens the menu; in mouse mode the sticks move the pointer for [elapsedMs] of deflection
     * (the left stick fast, the right one slow, for aiming at small targets), times
     * [Controls.pointerSpeed].
     */
    fun tick(elapsedMs: Long) {
        if (elapsedMs <= 0) return
        if (chordFormed && !chordMenu) {
            chordHeldMs += elapsedMs
            if (chordHeldMs >= MENU_HOLD_MS) {
                chordMenu = true
                onMenu()
            }
        }
        if (!mouseMode) return
        val seconds = elapsedMs / 1000f
        val factor = controls.pointerSpeed
        val (lx, ly) = speed(axes.lx, axes.ly, FAST_PIXELS_PER_SECOND * factor)
        val (rx, ry) = speed(axes.rx, axes.ry, SLOW_PIXELS_PER_SECOND * factor)
        restX += (lx + rx) * seconds
        restY += (ly + ry) * seconds
        val dx = restX.toInt()
        val dy = restY.toInt()
        restX -= dx
        restY -= dy
        if (dx != 0 || dy != 0) relay(listOf(RelayEvent.Relative(dx, dy)))
    }

    /**
     * Lets go of the controller (an overlay layer opened): its buttons and sticks go neutral, X
     * keys and buttons are released, a chord in progress is forgotten. Buttons still held count
     * again once pressed anew. The on-screen controls' share ([onVirtual]) stays: they play on
     * beside the menu rail, and let go themselves when they hide.
     */
    fun releaseAll() {
        held.clear()
        axes = PadAxes()
        resetChord()
        val out = ArrayList<RelayEvent>()
        update(out)
        flush(out)
    }

    private fun resetChord() {
        chordFormed = false
        chordMenu = false
        inChord.clear()
    }

    private fun toggle() {
        mouseMode = !mouseMode
        restX = 0f
        restY = 0f
    }

    /** Every button pressed now: keys, triggers past half way and the hat as the d-pad. */
    private fun pressed(): Set<PadButton> = buildSet {
        addAll(held)
        if (axes.leftTrigger >= TRIGGER_PRESSED) add(PadButton.L2)
        if (axes.rightTrigger >= TRIGGER_PRESSED) add(PadButton.R2)
        if (axes.hatX <= -HAT_PRESSED) add(PadButton.LEFT)
        if (axes.hatX >= HAT_PRESSED) add(PadButton.RIGHT)
        if (axes.hatY <= -HAT_PRESSED) add(PadButton.UP)
        if (axes.hatY >= HAT_PRESSED) add(PadButton.DOWN)
    }

    /** The pressed buttons that act while held: not chord buttons in a chord, nor ones that act on release. */
    private fun holding(): Set<PadButton> {
        val chord = controls.chord
        return pressed().filterTo(HashSet()) { button ->
            val chordButton = button == chord.first || button == chord.second
            button !in inChord && (!chordButton || controls.action(button, mouseMode) == PadAction.Pad)
        }
    }

    /** Brings X's keys and buttons in line with what is held now; a new press of a wheel binding clicks. */
    private fun update(out: MutableList<RelayEvent>) {
        val now = holding()
        val keys = HashSet<Int>()
        val buttons = HashSet<Int>()
        for (button in now) {
            when (val action = controls.action(button, mouseMode)) {
                is PadAction.Key -> keys += action.keysym
                is PadAction.Mouse -> if (action.button == RelayEvent.WHEEL_UP || action.button == RelayEvent.WHEEL_DOWN) {
                    if (button !in active) wheel(action.button, out)
                } else {
                    buttons += action.button
                }
                PadAction.Pad, PadAction.None -> Unit
            }
        }
        active = now
        (keysDown - keys).forEach { out += RelayEvent.Key(it, false) }
        (buttonsDown - buttons).forEach { out += RelayEvent.Button(it, false) }
        (keys - keysDown).forEach { out += RelayEvent.Key(it, true) }
        (buttons - buttonsDown).forEach { out += RelayEvent.Button(it, true) }
        keysDown.retainAll(keys)
        keysDown.addAll(keys)
        buttonsDown.retainAll(buttons)
        buttonsDown.addAll(buttons)
    }

    /** A chord button released on its own: its key or mouse button once. */
    private fun tapAction(action: PadAction, out: MutableList<RelayEvent>) {
        when (action) {
            is PadAction.Key -> {
                out += RelayEvent.Key(action.keysym, true)
                out += RelayEvent.Key(action.keysym, false)
            }
            is PadAction.Mouse -> wheel(action.button, out)
            PadAction.Pad, PadAction.None -> Unit
        }
    }

    private fun wheel(button: Int, out: MutableList<RelayEvent>) {
        out += RelayEvent.Button(button, true)
        out += RelayEvent.Button(button, false)
    }

    /** The pad state the game should see now: the real controller's, merged with the virtual one. */
    fun padState(): PadState = merge(if (mouseMode) PadState.NEUTRAL else realState(), virtual)

    private fun realState(): PadState {
        var buttons = 0
        for (button in holding()) {
            if (controls.action(button, mouse = false) == PadAction.Pad) buttons = buttons or button.bit
        }
        fun triggerOf(button: PadButton, value: Float): Int = when {
            controls.action(button, mouse = false) != PadAction.Pad -> 0
            button in held -> 255
            else -> trigger(value)
        }
        return PadState(
            buttons = buttons,
            leftTrigger = triggerOf(PadButton.L2, axes.leftTrigger),
            rightTrigger = triggerOf(PadButton.R2, axes.rightTrigger),
            lx = thumb(axes.lx),
            ly = thumb(-axes.ly),
            rx = thumb(axes.rx),
            ry = thumb(-axes.ry),
        )
    }

    private fun flush(out: List<RelayEvent>) {
        if (out.isNotEmpty()) relay(out)
        val state = padState()
        if (state != sent) {
            sent = state
            pad(state)
        }
    }

    companion object {
        const val DEAD_ZONE = 0.15f
        const val FAST_PIXELS_PER_SECOND = 1200f
        const val SLOW_PIXELS_PER_SECOND = 300f
        const val TRIGGER_PRESSED = 0.5f
        const val HAT_PRESSED = 0.5f

        /** How long the chord is held to open the overlay menu rather than switch mouse mode. */
        const val MENU_HOLD_MS = 600L

        /** A stick at rest, in XInput thumb units: inside the dead zone. */
        private val REST = (DEAD_ZONE * Short.MAX_VALUE).toInt()

        /** Buttons and triggers of both; each stick from the real pad unless it rests, then the virtual one's. */
        fun merge(real: PadState, virtual: PadState): PadState {
            val leftReal = hypot(real.lx.toFloat(), real.ly.toFloat()) > REST
            val rightReal = hypot(real.rx.toFloat(), real.ry.toFloat()) > REST
            return PadState(
                buttons = real.buttons or virtual.buttons,
                leftTrigger = maxOf(real.leftTrigger, virtual.leftTrigger),
                rightTrigger = maxOf(real.rightTrigger, virtual.rightTrigger),
                lx = if (leftReal) real.lx else virtual.lx,
                ly = if (leftReal) real.ly else virtual.ly,
                rx = if (rightReal) real.rx else virtual.rx,
                ry = if (rightReal) real.ry else virtual.ry,
            )
        }

        /** An Android stick axis (-1..1) as an XInput thumb value. */
        fun thumb(value: Float): Int = (value.coerceIn(-1f, 1f) * Short.MAX_VALUE).roundToInt()

        /** An Android trigger axis (0..1) as an XInput trigger value. */
        fun trigger(value: Float): Int = (value.coerceIn(0f, 1f) * 255).roundToInt()

        /** Pointer speed for a stick: zero inside the dead zone, then growing with the square. */
        private fun speed(x: Float, y: Float, max: Float): Pair<Float, Float> {
            val magnitude = hypot(x, y).coerceAtMost(1f)
            if (magnitude <= DEAD_ZONE) return 0f to 0f
            val scaled = (magnitude - DEAD_ZONE) / (1f - DEAD_ZONE)
            val factor = max * scaled * scaled / magnitude
            return x * factor to y * factor
        }
    }
}
