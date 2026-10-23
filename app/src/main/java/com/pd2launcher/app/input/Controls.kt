package com.pd2launcher.app.input

import android.content.Context
import android.view.InputDevice
import android.view.KeyEvent
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

/** A controller button as the keybind screen names it: its Android keycode and XInput bit (0: a trigger). */
enum class PadButton(val keyCode: Int, val bit: Int, val label: String) {
    A(KeyEvent.KEYCODE_BUTTON_A, GamepadLayout.A, "A"),
    B(KeyEvent.KEYCODE_BUTTON_B, GamepadLayout.B, "B"),
    X(KeyEvent.KEYCODE_BUTTON_X, GamepadLayout.X, "X"),
    Y(KeyEvent.KEYCODE_BUTTON_Y, GamepadLayout.Y, "Y"),
    L1(KeyEvent.KEYCODE_BUTTON_L1, GamepadLayout.LEFT_SHOULDER, "L1"),
    R1(KeyEvent.KEYCODE_BUTTON_R1, GamepadLayout.RIGHT_SHOULDER, "R1"),
    L2(KeyEvent.KEYCODE_BUTTON_L2, 0, "L2"),
    R2(KeyEvent.KEYCODE_BUTTON_R2, 0, "R2"),
    L3(KeyEvent.KEYCODE_BUTTON_THUMBL, GamepadLayout.LEFT_THUMB, "L3"),
    R3(KeyEvent.KEYCODE_BUTTON_THUMBR, GamepadLayout.RIGHT_THUMB, "R3"),
    START(KeyEvent.KEYCODE_BUTTON_START, GamepadLayout.START, "Start"),
    SELECT(KeyEvent.KEYCODE_BUTTON_SELECT, GamepadLayout.BACK, "Select"),
    UP(KeyEvent.KEYCODE_DPAD_UP, GamepadLayout.DPAD_UP, "D-pad up"),
    DOWN(KeyEvent.KEYCODE_DPAD_DOWN, GamepadLayout.DPAD_DOWN, "D-pad down"),
    LEFT(KeyEvent.KEYCODE_DPAD_LEFT, GamepadLayout.DPAD_LEFT, "D-pad left"),
    RIGHT(KeyEvent.KEYCODE_DPAD_RIGHT, GamepadLayout.DPAD_RIGHT, "D-pad right"),
    ;

    companion object {
        private val byKeyCode = entries.associateBy { it.keyCode }

        fun of(keyCode: Int): PadButton? = byKeyCode[keyCode]
    }
}

/** What a controller button does: the game's pad button, an X key, an X mouse button, or nothing. */
sealed class PadAction {
    /** The button reaches the game's XInput pad (pad mode only; in mouse mode the pad is neutral). */
    data object Pad : PadAction()

    data object None : PadAction()

    /** X [keysym] held while the button is. */
    data class Key(val keysym: Int) : PadAction()

    /** X mouse [button]: 1 and 3 held while the button is, 4 and 5 (the wheel) one click per press. */
    data class Mouse(val button: Int) : PadAction()

    /** The stored form: `pad`, `none`, `key:<keysym>`, `mouse:<button>`. */
    fun encode(): String = when (this) {
        Pad -> "pad"
        None -> "none"
        is Key -> "key:$keysym"
        is Mouse -> "mouse:$button"
    }

    companion object {
        fun decode(text: String): PadAction? = when {
            text == "pad" -> Pad
            text == "none" -> None
            text.startsWith("key:") -> text.removePrefix("key:").toIntOrNull()?.takeIf { it > 0 }?.let(::Key)
            text.startsWith("mouse:") -> text.removePrefix("mouse:").toIntOrNull()?.takeIf { it in 1..5 }?.let(::Mouse)
            else -> null
        }
    }
}

/**
 * The chord that switches mouse mode (a short press of both) and opens the overlay menu (both
 * held for [PadInput.MENU_HOLD_MS]). One chord for both, so neither takes another button away
 * from the game.
 */
enum class Chord(val first: PadButton, val second: PadButton) {
    SELECT_START(PadButton.SELECT, PadButton.START),
    THUMBS(PadButton.L3, PadButton.R3),
}

/**
 * The player's controls: the controller's bindings in pad and mouse mode, the chord,
 * the pointer speed in mouse mode, and whether the on-screen controls show and how strongly
 * (their layout is [LayoutStore]'s).
 */
data class Controls(
    val padMode: Map<PadButton, PadAction> = DEFAULT_PAD,
    val mouseMode: Map<PadButton, PadAction> = DEFAULT_MOUSE,
    val chord: Chord = Chord.SELECT_START,
    /** Mouse mode's pointer speed, a factor of the sticks' base speeds ([PointerSpeed]). */
    val pointerSpeed: Float = PointerSpeed.DEFAULT,
    /** On-screen controls shown; null: shown only while no controller is connected. */
    val onScreen: Boolean? = null,
    /** The on-screen controls' opacity; each piece's own is a factor of it ([Piece.opacity]). */
    val onScreenOpacity: Float = 0.35f,
) {
    /** The on-screen controls on: the player's choice, or (Auto) while no [controller] is connected. */
    fun onScreenOn(controller: Boolean): Boolean = onScreen ?: !controller

    /**
     * The on-screen controls showing now: Always shows them everywhere; Auto shows them while no
     * [controller] is connected, on PD2's menu screens (the main menu, character select) as in
     * the world, so the [picture] does not change the answer. Never over the official PD2
     * [launcher], a mouse/touch window they would cover. The Game view applies this over the game
     * and beside the in-game menu alike, so the menu's On-screen controls tile shows at once, and
     * hides them while it asks whether to stop and under its Controls screen and editor
     * (`GameLayer.padShown`).
     */
    fun onScreenShown(controller: Boolean, picture: GamePicture, launcher: Boolean = false): Boolean =
        !launcher && (onScreen ?: !controller)

    /**
     * The in-game menu's On-screen controls tile switched [on]: the player's choice, kept as it is
     * (Always or Never) until they switch it again or pick Auto in the on-screen controls' editor,
     * whatever controller comes or goes.
     */
    fun withOnScreen(on: Boolean): Controls = copy(onScreen = on)

    fun action(button: PadButton, mouse: Boolean): PadAction =
        (if (mouse) mouseMode[button] ?: DEFAULT_MOUSE.getValue(button) else padMode[button] ?: PadAction.Pad)
            .let { if (mouse && it == PadAction.Pad) PadAction.None else it }

    fun bind(button: PadButton, mouse: Boolean, action: PadAction): Controls =
        if (mouse) copy(mouseMode = mouseMode + (button to action)) else copy(padMode = padMode + (button to action))

    /** Whether [button] does what it does by default in that mode (Settings mutes those). */
    fun isDefault(button: PadButton, mouse: Boolean): Boolean =
        action(button, mouse) == (if (mouse) DEFAULT_MOUSE.getValue(button) else PadAction.Pad)

    /** Every button of one mode back to its default; the other mode and the rest stay. */
    fun resetMode(mouse: Boolean): Controls = if (mouse) copy(mouseMode = DEFAULT_MOUSE) else copy(padMode = DEFAULT_PAD)

    fun toPreferences(): Map<String, Any> = buildMap {
        PadButton.entries.forEach {
            put(PAD_PREFIX + it.name, action(it, mouse = false).encode())
            put(MOUSE_PREFIX + it.name, action(it, mouse = true).encode())
        }
        put(CHORD, chord.name)
        // Only a speed the player picked: an unchanged one follows the default when it changes.
        if (pointerSpeed != PointerSpeed.DEFAULT) put(POINTER_SPEED, pointerSpeed)
        // The base speeds a stored speed is a factor of, so a later change of them keeps its real speed.
        put(POINTER_SPEED_BASE, PointerSpeed.BASE)
        onScreen?.let { put(ON_SCREEN, it) }
        put(ON_SCREEN_OPACITY, onScreenOpacity)
    }

    companion object {
        const val PAD_PREFIX = "pad."
        const val MOUSE_PREFIX = "mouse."
        const val CHORD = "chord"
        const val POINTER_SPEED = "pointer_speed"
        /** The base speeds [POINTER_SPEED] is a factor of ([PointerSpeed.BASE]); none: the ones before 0.3.3. */
        const val POINTER_SPEED_BASE = "pointer_speed_base"
        const val ON_SCREEN = "on_screen"
        const val ON_SCREEN_OPACITY = "on_screen_opacity"

        val OPACITIES = 0.1f..1f

        /** One step of the editor's opacity slider. */
        const val OPACITY_STEP = 0.05f

        /**
         * [value] moved by [steps] of [step] inside [range], landing on the step grid (a stored
         * value may sit between two steps).
         */
        fun stepped(value: Float, step: Float, steps: Int, range: ClosedFloatingPointRange<Float>): Float {
            val grid = Math.round(value / step)
            return ((grid + steps) * step).coerceIn(range.start, range.endInclusive).let { Math.round(it * 1000f) / 1000f }
        }

        /** How far [value] is along [range], 0..1, for the stepper's bar. */
        fun fraction(value: Float, range: ClosedFloatingPointRange<Float>): Float =
            ((value - range.start) / (range.endInclusive - range.start)).coerceIn(0f, 1f)

        /** Pad mode: every button is the game's. */
        val DEFAULT_PAD: Map<PadButton, PadAction> = PadButton.entries.associateWith { PadAction.Pad }

        /** Mouse mode: A/L2 left button, B/R2 right, X Enter, Y Alt, d-pad arrows, Start Escape, Select Tab, L1/R1 wheel. */
        val DEFAULT_MOUSE: Map<PadButton, PadAction> = mapOf(
            PadButton.A to PadAction.Mouse(RelayEvent.LEFT),
            PadButton.B to PadAction.Mouse(RelayEvent.RIGHT),
            PadButton.X to PadAction.Key(Keys.XK_RETURN),
            PadButton.Y to PadAction.Key(Keys.XK_ALT_L),
            PadButton.L1 to PadAction.Mouse(RelayEvent.WHEEL_UP),
            PadButton.R1 to PadAction.Mouse(RelayEvent.WHEEL_DOWN),
            PadButton.L2 to PadAction.Mouse(RelayEvent.LEFT),
            PadButton.R2 to PadAction.Mouse(RelayEvent.RIGHT),
            PadButton.L3 to PadAction.None,
            PadButton.R3 to PadAction.None,
            PadButton.START to PadAction.Key(Keys.XK_ESCAPE),
            PadButton.SELECT to PadAction.Key(Keys.XK_TAB),
            PadButton.UP to PadAction.Key(Keys.XK_UP),
            PadButton.DOWN to PadAction.Key(Keys.XK_DOWN),
            PadButton.LEFT to PadAction.Key(Keys.XK_LEFT),
            PadButton.RIGHT to PadAction.Key(Keys.XK_RIGHT),
        )

        /**
         * Whether [values] hold a pointer speed stored at other base speeds than today's (before
         * 0.3.3, none stored beside it): read, it is converted ([PointerSpeed.fromStored]), and
         * [ControlsStore] stores it again once, converted.
         */
        fun pointerSpeedToConvert(values: Map<String, *>): Boolean =
            values[POINTER_SPEED] is Number && storedBase(values) != PointerSpeed.BASE

        private fun storedBase(values: Map<String, *>): Float? = (values[POINTER_SPEED_BASE] as? Number)?.toFloat()

        /** A missing or unreadable value is the default, one by one. */
        fun fromPreferences(values: Map<String, *>): Controls {
            val defaults = Controls()
            fun number(key: String, default: Float, range: ClosedFloatingPointRange<Float>) =
                ((values[key] as? Number)?.toFloat() ?: default).coerceIn(range)
            fun bindings(prefix: String, fallback: Map<PadButton, PadAction>) = PadButton.entries.associateWith {
                (values[prefix + it.name] as? String)?.let(PadAction::decode) ?: fallback.getValue(it)
            }
            return Controls(
                padMode = bindings(PAD_PREFIX, DEFAULT_PAD),
                mouseMode = bindings(MOUSE_PREFIX, DEFAULT_MOUSE),
                chord = Chord.entries.firstOrNull { it.name == values[CHORD] } ?: defaults.chord,
                pointerSpeed = (values[POINTER_SPEED] as? Number)?.toFloat()?.let { PointerSpeed.fromStored(it, storedBase(values)) }
                    ?: defaults.pointerSpeed,
                onScreen = values[ON_SCREEN] as? Boolean,
                onScreenOpacity = number(ON_SCREEN_OPACITY, defaults.onScreenOpacity, OPACITIES),
            )
        }
    }
}

/**
 * An input device with [sources] ([InputDevice] source bits) is a controller: a gamepad
 * (SOURCE_GAMEPAD), built in (some handhelds' built-in pads report as an external controller) or
 * attached; Android's [virtual] keyboard never. SOURCE_JOYSTICK alone does not count: keyboards and
 * mice with consumer or system-control parts report it, and real pads have SOURCE_GAMEPAD.
 */
fun isController(sources: Int, virtual: Boolean): Boolean =
    !virtual && sources and InputDevice.SOURCE_GAMEPAD == InputDevice.SOURCE_GAMEPAD

/**
 * Mouse mode's pointer speed, its numbers in one place: the sticks' base speeds at 1.00×, the
 * default, the range and the step a player picks in, and how a speed stored before 0.3.3 carries
 * over. 0.3.3 halved the base speeds (at 1.00× the pointer went too far for small moves on an
 * enlarged picture) and kept 1.00× as the default.
 */
object PointerSpeed {
    /**
     * The left stick fully out at 1.00×, in X screen pixels a second on the game's own 1068
     * columns (`PadInput` scales it to the X screen's width). Half of the 1200 before 0.3.3.
     */
    const val FAST_PIXELS_PER_SECOND = 600f

    /** The right stick fully out at 1.00×, for aiming at small targets: a quarter of the left. */
    const val SLOW_PIXELS_PER_SECOND = 150f

    /** The base a stored speed is a factor of, stored beside it: [FAST_PIXELS_PER_SECOND]. */
    const val BASE = FAST_PIXELS_PER_SECOND

    /** The base before 0.3.3, which a stored speed with no base beside it is a factor of. */
    const val EARLIER_BASE = 1200f

    const val DEFAULT = 1f
    const val STEP = 0.05f
    val RANGE = 0.2f..2.5f

    /**
     * A [stored] speed, a factor of [base] ([EARLIER_BASE] where none was stored), as a factor of
     * today's [BASE]: the same real speed where [RANGE] holds it, on the step grid (twice the factor
     * from before 0.3.3, which held 0.25 to 3.00: up to 1.25 the same speed, a faster one 2.50, the
     * range's top). The default stored with no base, 1.00, which every save of the controls wrote
     * before 0.3.3 whether or not the player picked it, is today's default.
     */
    fun fromStored(stored: Float, base: Float?): Float = when {
        base == BASE -> stored.coerceIn(RANGE)
        base == null && stored == DEFAULT -> DEFAULT
        else -> Controls.stepped(stored * (base ?: EARLIER_BASE) / BASE, STEP, 0, RANGE)
    }

    /**
     * What the player typed into the Pointer speed input ("1.5", "1,5"): a number inside
     * [RANGE], on the step grid; null for anything else.
     */
    fun typed(text: String): Float? =
        text.trim().replace(',', '.').toFloatOrNull()?.takeIf { it in RANGE }?.let { Controls.stepped(it, STEP, 0, RANGE) }
}

/** [Controls] in the app's `controls` preferences, and as a flow the Game view applies live. */
object ControlsStore {
    private const val PREFERENCES = "controls"
    private val _current = MutableStateFlow(Controls())
    private var loaded = false

    val current: StateFlow<Controls> = _current.asStateFlow()

    /**
     * Reads the stored controls once per process; later calls return what is current. A pointer
     * speed stored at other base speeds (before 0.3.3) is stored again once, converted ([PointerSpeed.fromStored]).
     */
    fun load(context: Context): Controls {
        if (!loaded) {
            loaded = true
            val stored = context.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE).all
            _current.value = Controls.fromPreferences(stored)
            if (Controls.pointerSpeedToConvert(stored)) save(context, _current.value)
        }
        return _current.value
    }

    fun save(context: Context, controls: Controls) {
        val editor = context.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE).edit().clear()
        for ((key, value) in controls.toPreferences()) {
            when (value) {
                is Boolean -> editor.putBoolean(key, value)
                is Float -> editor.putFloat(key, value)
                is String -> editor.putString(key, value)
            }
        }
        editor.apply()
        loaded = true
        _current.value = controls
    }
}

/**
 * The on-screen controls' layouts in the app's `on_screen_layouts` preferences, one per screen
 * size and orientation ([ControlsLayout.key]) as JSON in normalised coordinates; a screen with
 * none stored has the default for its size. [version] moves on every save, so the Game view
 * reads the layout again.
 */
object LayoutStore {
    private const val PREFERENCES = "on_screen_layouts"
    private val _version = MutableStateFlow(0)

    val version: StateFlow<Int> = _version.asStateFlow()

    /** The layout for a [width] x [height] dp screen. */
    fun layout(context: Context, width: Int, height: Int): ControlsLayout = ControlsLayout.decode(
        context.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE).getString(ControlsLayout.key(width, height), null),
        ControlsLayout.default(width.toFloat(), height.toFloat()),
    )

    fun save(context: Context, width: Int, height: Int, layout: ControlsLayout) {
        context.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE).edit()
            .putString(ControlsLayout.key(width, height), layout.encode()).apply()
        _version.value++
    }
}
