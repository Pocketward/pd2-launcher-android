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
 * The chord's buttons are buttons of their own too: bound to the pad, the game sees one only once
 * it has been held alone for [CHORD_WINDOW_MS] (let go sooner, it shows as a press that long), so
 * the first button of a chord formed within that time never reaches the game; the game sees it
 * until the chord forms. Bound to a key or mouse button, they act once when released without
 * having been part of the chord.
 *
 * [pointerScale]: the X screen's width over the game's own 1068 columns, which the pointer's speed
 * follows, so the pointer crosses the picture in the same time on every X screen. [onMode] hears
 * each change of mode, from the chord or from [useMouseMode] (the in-game Controls screen).
 */
class PadInput(
    private val pad: (PadState) -> Unit,
    private val relay: (List<RelayEvent>) -> Unit,
    private val onMenu: () -> Unit = {},
    controls: Controls = Controls(),
    private val pointerScale: Float = 1f,
    private val onMode: (mouse: Boolean) -> Unit = {},
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

    /**
     * Mouse mode on ([on]) or off, as the chord pressed briefly switches it (the in-game Controls
     * screen's Use pad mode and Use mouse mode): what the old mode held is released at once, and a
     * chord in progress is forgotten (its release would switch back).
     */
    fun useMouseMode(on: Boolean) {
        if (on == mouseMode) return
        resetChord()
        toggle()
        val out = ArrayList<RelayEvent>()
        update(out)
        flush(out)
        onMode(mouseMode)
    }

    private val held = HashSet<PadButton>() // digital buttons down
    private var axes = PadAxes()
    private var chordFormed = false // both chord buttons went down together and neither went up yet
    private var chordHeldMs = 0L
    private var chordMenu = false // this chord opened the menu: its release does not switch the mode
    private val inChord = HashSet<PadButton>() // chord buttons that were part of a chord, until released
    private val heldBack = HashMap<PadButton, Long>() // pad-bound chord buttons held alone, not shown yet: ms so far
    private val tapped = HashMap<PadButton, Long>() // ones let go while held back, shown as a press: ms so far
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
        val mode = mouseMode
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
                heldBack.remove(other)
            } else if (down && !was && controls.action(button, mouseMode) == PadAction.Pad) {
                heldBack[button] = 0
            } else if (!down && was) {
                if (heldBack.remove(button) != null) tapped[button] = 0
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
        // Heard once the switch is done: X and the game's pad already follow the new mode.
        if (mouseMode != mode) onMode(mouseMode)
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
     * Call once per display frame with the time since the last one: a chord button held back
     * long enough shows (and a tap of one ends); the chord held long enough opens the menu; in
     * mouse mode the sticks move the pointer for [elapsedMs] of deflection (the left stick fast,
     * the right one slow, for aiming at small targets), times [Controls.pointerSpeed].
     */
    fun tick(elapsedMs: Long) {
        if (elapsedMs <= 0) return
        if (age(heldBack, elapsedMs) or age(tapped, elapsedMs)) {
            val out = ArrayList<RelayEvent>()
            update(out)
            flush(out)
        }
        if (chordFormed && !chordMenu) {
            chordHeldMs += elapsedMs
            if (chordHeldMs >= MENU_HOLD_MS) {
                chordMenu = true
                onMenu()
            }
        }
        if (!mouseMode) return
        val seconds = elapsedMs / 1000f
        val factor = controls.pointerSpeed * pointerScale
        val (lx, ly) = speed(axes.lx, axes.ly, PointerSpeed.FAST_PIXELS_PER_SECOND * factor)
        val (rx, ry) = speed(axes.rx, axes.ry, PointerSpeed.SLOW_PIXELS_PER_SECOND * factor)
        restX += (lx + rx) * seconds
        restY += (ly + ry) * seconds
        val dx = restX.toInt()
        val dy = restY.toInt()
        restX -= dx
        restY -= dy
        if (dx != 0 || dy != 0) relay(listOf(RelayEvent.Relative(dx, dy)))
    }

    /**
     * Whether [tick] has work to do: a chord button held back or the tap of one showing, the chord
     * held toward the menu, or in mouse mode a stick out of its dead zone. While it has none, the
     * Game view runs no display-frame clock for it (each display frame woke the main thread).
     */
    val ticking: Boolean
        get() = heldBack.isNotEmpty() || tapped.isNotEmpty() || (chordFormed && !chordMenu) ||
            (mouseMode && (deflected(axes.lx, axes.ly) || deflected(axes.rx, axes.ry)))

    /**
     * Lets go of the controller (an overlay layer opened): its buttons and sticks go neutral, X
     * keys and buttons are released, a chord in progress is forgotten. Buttons still held count
     * again once pressed anew. The on-screen controls' share ([onVirtual]) stays: they play on
     * beside the in-game menu, and let go themselves when they hide.
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
        heldBack.clear()
        tapped.clear()
    }

    /** Adds [elapsedMs] to each of [waits]; removes those that reached [CHORD_WINDOW_MS] and says whether any did. */
    private fun age(waits: MutableMap<PadButton, Long>, elapsedMs: Long): Boolean {
        val before = waits.size
        waits.replaceAll { _, ms -> ms + elapsedMs }
        waits.values.removeAll { it >= CHORD_WINDOW_MS }
        return waits.size != before
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

    /**
     * The pressed buttons that act while held: not chord buttons in a chord or still held back,
     * nor ones that act on release; and a held-back chord button's tap while it shows.
     */
    private fun holding(): Set<PadButton> {
        val chord = controls.chord
        return (pressed() + tapped.keys).filterTo(HashSet()) { button ->
            val chordButton = button == chord.first || button == chord.second
            button !in inChord && button !in heldBack && (!chordButton || controls.action(button, mouseMode) == PadAction.Pad)
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
        const val TRIGGER_PRESSED = 0.5f
        const val HAT_PRESSED = 0.5f

        /** How long the chord is held to open the overlay menu rather than switch mouse mode. */
        const val MENU_HOLD_MS = 600L

        /**
         * How long a pad-bound chord button is held alone before the game sees it: time for its
         * partner to come, as a chord pressed by a person forms within a few dozen milliseconds.
         */
        const val CHORD_WINDOW_MS = 60L

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

        /** Whether a stick is out of its dead zone, where [speed] moves the pointer. */
        private fun deflected(x: Float, y: Float): Boolean = hypot(x, y).coerceAtMost(1f) > DEAD_ZONE

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
