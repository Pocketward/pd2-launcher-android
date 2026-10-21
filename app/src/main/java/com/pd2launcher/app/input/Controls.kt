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
    /** Mouse mode's pointer speed, a factor of the default (0.25..3). */
    val pointerSpeed: Float = 1f,
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
     * and beside the in-game menu rail alike, so the rail's Touch pad switch shows at once, and
     * hides them while it asks whether to stop and under its Controls panel and editor
     * (`GameLayer.padShown`).
     */
    fun onScreenShown(controller: Boolean, picture: GamePicture, launcher: Boolean = false): Boolean =
        !launcher && (onScreen ?: !controller)

    /**
     * The in-game menu's Touch pad switched [on]: the player's choice, kept as it is (Always or
     * Never) until they switch it again or pick Auto in Settings, whatever controller comes or
     * goes; [controller] is not consulted.
     */
    @Suppress("UNUSED_PARAMETER")
    fun withTouchPad(on: Boolean, controller: Boolean): Controls = copy(onScreen = on)

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
        put(POINTER_SPEED, pointerSpeed)
        onScreen?.let { put(ON_SCREEN, it) }
        put(ON_SCREEN_OPACITY, onScreenOpacity)
    }

    companion object {
        const val PAD_PREFIX = "pad."
        const val MOUSE_PREFIX = "mouse."
        const val CHORD = "chord"
        const val POINTER_SPEED = "pointer_speed"
        const val ON_SCREEN = "on_screen"
        const val ON_SCREEN_OPACITY = "on_screen_opacity"

        val POINTER_SPEEDS = 0.25f..3f
        val OPACITIES = 0.1f..1f

        /** One step of each value: pointer speed's − and +, the editor's opacity slider. */
        const val POINTER_SPEED_STEP = 0.25f
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
                pointerSpeed = number(POINTER_SPEED, defaults.pointerSpeed, POINTER_SPEEDS),
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

/** [Controls] in the app's `controls` preferences, and as a flow the Game view applies live. */
object ControlsStore {
    private const val PREFERENCES = "controls"
    private val _current = MutableStateFlow(Controls())
    private var loaded = false

    val current: StateFlow<Controls> = _current.asStateFlow()

    /** Reads the stored controls once per process; later calls return what is current. */
    fun load(context: Context): Controls {
        if (!loaded) {
            loaded = true
            _current.value = Controls.fromPreferences(context.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE).all)
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
