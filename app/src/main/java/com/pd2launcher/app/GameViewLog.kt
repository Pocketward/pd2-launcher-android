package com.pd2launcher.app

import android.view.Display
import android.view.InputDevice
import com.pd2launcher.app.input.Controls
import com.pd2launcher.app.input.Keys
import com.pd2launcher.app.input.PadAction
import com.pd2launcher.app.input.PadButton
import com.pd2launcher.app.input.PadState
import com.pd2launcher.app.input.RelayEvent
import java.io.File
import java.util.Locale

/**
 * The Game view's logs in `files/logs`, which Share logs packs (every file there):
 * - `game-view.log`: events only, always: the window's focus, the displays, Android's input
 *   dispatch ([InputDispatch]), whether the app is
 *   the top resumed activity, why the game is told that it is away or back, the overlays over the
 *   game, the controller's mode, the app's pointer and the pointer area, the screen's rates and
 *   the clock ([GameViewLines], [ScreenRateLines]);
 * - `input.log`: while Detailed logging is on (from the moment it is turned on), the controller's
 *   keys and the states the game's pad gets ([PadKeyLines], [PadStateLines]); never a key of a
 *   keyboard or of Android's keyboard. A file of its own, so its many lines never rotate the
 *   events out.
 * Each is [LIMIT] x [KEEP]. [open] comes with the activity's data folder; lines before it (and in
 * the JVM tests) are dropped.
 */
object GameViewLog {
    const val LIMIT = 1L shl 20
    const val KEEP = 2

    @Volatile private var events: RotatingLog? = null
    @Volatile private var input: RotatingLog? = null

    /**
     * Whether the app's activity is the top resumed activity (Android 10's
     * `onTopResumedActivityChanged`; on a phone with two displays, or with a window of another app
     * over it, the input goes elsewhere); null before Android said. The focus lines name it.
     */
    @Volatile var topResumed: Boolean? = null

    @Synchronized
    fun open(logs: File) {
        if (events != null) return
        events = RotatingLog(logs, "game-view", LIMIT, KEEP)
        input = RotatingLog(logs, "input", LIMIT, KEEP)
    }

    /** One line in `game-view.log`. */
    fun line(text: String) {
        events?.line(text)
    }

    /** One line in `input.log`, only while Detailed logging is on. */
    fun input(text: String) {
        if (AppSettingsStore.current.value.detailedLogging) input?.line(text)
    }
}

/** A display as the Game view's lines name it: its id, its mode's size and its refresh rate now. */
data class DisplayFacts(val id: Int, val width: Int, val height: Int, val hertz: Float) {
    override fun toString() = String.format(Locale.ROOT, "%d x %d, %.1f Hz", width, height, hertz)

    companion object {
        fun of(display: Display) = DisplayFacts(display.displayId, display.mode.physicalWidth, display.mode.physicalHeight, display.refreshRate)
    }
}

/** The texts of `game-view.log`'s lines ([GameViewLog]). Display ids, sizes and rates only: no display's name. */
object GameViewLines {
    /** The Game view opens on [display]; [all]: every display Android lists now. */
    fun opens(display: DisplayFacts, all: List<DisplayFacts>): String = buildString {
        append("game view: opens on display ${display.id} (")
        if (display.id == DEFAULT_DISPLAY) append("the default display, ")
        append(display).append("); displays: ")
        append(all.joinToString { if (it.id == display.id) "${it.id}" else "${it.id} ($it)" })
    }

    fun displayAdded(display: DisplayFacts) = "game view: display ${display.id} added ($display)"

    fun displayRemoved(id: Int) = "game view: display $id removed"

    /** The Game view's window got the focus ([has]) or lost it, on display [displayId]; [top]: [GameViewLog.topResumed]. */
    fun focus(has: Boolean, displayId: Int?, top: Boolean?): String = buildString {
        append("game view: window focus ").append(if (has) "back" else "lost")
        displayId?.let { append(" (display $it)") }
        when (top) {
            true -> append("; the app is the top resumed activity")
            false -> append("; the app is not the top resumed activity")
            null -> {}
        }
    }

    fun topResumed(top: Boolean) =
        if (top) "game view: the app is the top resumed activity" else "game view: the app is not the top resumed activity (another display or app has the input)"

    /** x-input-relay tells the game that it is [away] or back on the screen ([RelayEvent.Focus]), for the reason [why]. */
    fun told(away: Boolean, why: String) = "game view: the game is told it is ${if (away) "away" else "back"} ($why)"

    /** The app's pointer over the game's own buffers: [drawn] with its image's size and hot spot, or not and [why]. */
    fun pointer(drawn: Boolean, width: Int, height: Int, hotX: Int, hotY: Int, why: String): String =
        if (drawn) "game view: the app draws the X pointer over the game's own buffers ($width x $height, hot spot $hotX,$hotY)"
        else "game view: the app draws no pointer ($why)"

    /** Where x-input-relay keeps the pointer: [area] of the X screen, which shows [what]. */
    fun pointerArea(area: GameResolution.PictureRect, what: String, display: GameDisplay, touch: String): String =
        "game view: pointer area ${area.width} x ${area.height} at ${area.left},${area.top} ($what); display ${displayName(display)}; touch $touch"

    /** The Game view's place: [view] in its [area] (px), the game's [picture] in it, for the X screen [screen] that shows [what]. */
    fun geometry(display: GameDisplay, what: String, screen: Pair<Int, Int>, area: Pair<Int, Int>, view: Pair<Int, Int>, picture: GameResolution.PictureRect) =
        "game view: shows $what in ${view.first} x ${view.second} of ${area.first} x ${area.second}, the picture ${picture.width} x ${picture.height} " +
            "at ${picture.left},${picture.top} (X screen ${screen.first} x ${screen.second}, display ${displayName(display)})"

    /**
     * An overlay changed: [from] to [to] (null: the game itself, no overlay), [shownMs] after [from]
     * came; [why] names the cause where it is known, [lastKey] the controller's last key
     * ([PadKeyLines.last]).
     */
    fun layer(from: String?, to: String?, shownMs: Long, why: String?, lastKey: String?): String? {
        if (from == to) return null
        val after = String.format(Locale.ROOT, "%.1f s", shownMs / 1000.0)
        val head = when {
            to == null -> "$from: closes after $after"
            from == null -> "$to: opens"
            else -> "$to: opens from the $from after $after"
        }
        val notes = listOfNotNull(why, lastKey?.let { "the last controller key: $it" })
        return if (notes.isEmpty()) head else "$head (${notes.joinToString("; ")})"
    }

    /** [source] moved the pointer for the first time in this Game view: which input the player used. */
    fun firstMove(source: MoveSource) = "game view: the pointer first moves by ${source.firstMove}"

    /**
     * The Game view opens and asks Android for unbuffered dispatch of the source classes
     * [sources] ([InputDispatch]); none: it keeps the buffered dispatch (the debug switch).
     */
    fun inputDispatch(sources: Int): String {
        val classes = listOfNotNull(
            "pointer (touch screen, mouse, touchpad)".takeIf { sources and InputDevice.SOURCE_CLASS_POINTER != 0 },
            "joystick (controllers' sticks)".takeIf { sources and InputDevice.SOURCE_CLASS_JOYSTICK != 0 },
        )
        return if (classes.isEmpty()) "game view: input dispatch: buffered (debug switch)"
        else "game view: input dispatch: unbuffered for ${classes.joinToString(" and ")}"
    }

    /** The controller's mode changed to mouse mode ([mouse]) or pad mode, because of [why]; [deviceId]: the controller, where known. */
    fun mode(mouse: Boolean, deviceId: Int?, why: String): String =
        "${deviceId?.let { "controller $it" } ?: "controller"}: ${if (mouse) "mouse mode on" else "pad mode"} ($why)"

    /**
     * The clock's rate and the layers' vote changed: the clock at [clock] ticks a second, the
     * layers ask the display [displayId] for [vote] Hz, for the player's [frameRate]
     * ([AppSettings.FULL_FRAME_RATE]: Full) on the screen's [full] rate; [capped]: below it;
     * [multiple]: the vote is this many times the frame rate (the whole multiple, [FrameRateVote.vote]).
     */
    fun pacing(clock: Float, vote: Float, frameRate: Int, full: Float, displayId: Int?, capped: Boolean, multiple: Int = 1): String = String.format(
        Locale.ROOT, "pacing: clock %.1f ticks/s, the layers ask for %.1f Hz (%s, full rate %.1f Hz, display %s)%s",
        clock, vote, (if (multiple > 1) "$multiple x the " else "") + "frame rate setting " +
            (if (frameRate == AppSettings.FULL_FRAME_RATE) "Full" else frameRate.toString()), full,
        displayId?.toString() ?: "unknown", if (capped) ", capped" else "",
    )

    fun displayName(display: GameDisplay) = when (display) {
        GameDisplay.FIT -> "Fit to screen"
        GameDisplay.CENTER -> "Center"
    }

    private const val DEFAULT_DISPLAY = 0
}

/**
 * Detailed logging's key lines in `input.log` ([GameViewLog.input]): only for a key event of a
 * gamepad or joystick source ([source], Android's bits) whose key code is a controller's (a
 * button, the d-pad or Back), so never a keyboard's key, Android's keyboard or typed text. A key's
 * release names how long it was down. [last] names the controller's last key for the overlay lines,
 * also with Detailed logging off. Main thread.
 */
class PadKeyLines {
    private val downAt = HashMap<Pair<Int, Int>, Long>()
    private var last: Pair<String, Long>? = null

    /**
     * The line for the key [rawKeyCode] that the controller's layout ([layout]) reads as [keyCode],
     * [down] or up at [now] (ms), of [deviceId]; [target]: what the game gets for it
     * ([target] of the binding). Null for a key that is not a controller's.
     */
    fun line(source: Int, deviceId: Int, rawKeyCode: Int, keyCode: Int, down: Boolean, now: Long, layout: String?, target: String): String? {
        if (!controllerKey(source, rawKeyCode) || !controllerKey(source, keyCode)) return null
        val name = keyName(keyCode)
        val raw = if (rawKeyCode == keyCode) "$keyCode" else "$keyCode, raw $rawKeyCode"
        last = "controller $deviceId $name ${if (down) "down" else "up"}" to now
        val held = if (down) {
            downAt[deviceId to keyCode] = now
            ""
        } else {
            downAt.remove(deviceId to keyCode)?.let { " after ${now - it} ms" } ?: ""
        }
        return "pad key: controller $deviceId $name ($raw) ${if (down) "down" else "up"}$held -> $target" +
            (if (down && layout != null) " (layout $layout)" else "")
    }

    /** The controller's last key and how long before [now] it came; null for none. */
    fun last(now: Long): String? = last?.let { (key, at) -> "$key ${now - at} ms before" }

    companion object {
        /**
         * Whether [keyCode] of an event from [source] is a controller's key: the source has the
         * gamepad or the joystick bits, and the code is a button, the d-pad or Back.
         */
        fun controllerKey(source: Int, keyCode: Int): Boolean {
            val pad = (source and SOURCE_GAMEPAD) == SOURCE_GAMEPAD || (source and SOURCE_JOYSTICK) == SOURCE_JOYSTICK
            return pad && (keyCode == KEYCODE_BACK || keyCode in KEYCODE_DPAD_UP..KEYCODE_DPAD_CENTER ||
                keyCode in KEYCODE_BUTTON_A..KEYCODE_BUTTON_MODE || keyCode in KEYCODE_BUTTON_1..KEYCODE_BUTTON_16)
        }

        /** What the game gets for [button] with [controls] in mouse mode ([mouse]) or pad mode. */
        fun target(button: PadButton?, controls: Controls, mouse: Boolean): String {
            if (button == null) return "nothing"
            return when (val action = controls.action(button, mouse)) {
                PadAction.Pad -> "pad ${button.label}"
                PadAction.None -> "nothing"
                is PadAction.Key -> "key ${keysymName(action.keysym)}"
                is PadAction.Mouse -> "mouse ${mouseName(action.button)}"
            }
        }

        private fun keyName(keyCode: Int): String = PadButton.of(keyCode)?.label ?: when (keyCode) {
            KEYCODE_BACK -> "Back"
            KEYCODE_DPAD_CENTER -> "D-pad centre"
            else -> "key"
        }

        private fun keysymName(keysym: Int): String = when (keysym) {
            Keys.XK_ESCAPE -> "Escape"
            Keys.XK_RETURN -> "Enter"
            Keys.XK_TAB -> "Tab"
            Keys.XK_ALT_L -> "Alt"
            Keys.XK_SHIFT_L -> "Shift"
            Keys.XK_CONTROL_L -> "Ctrl"
            ' '.code -> "Space"
            in 0x21..0x7e -> keysym.toChar().uppercase()
            else -> "0x${Integer.toHexString(keysym)}"
        }

        private fun mouseName(button: Int): String = when (button) {
            RelayEvent.LEFT -> "left"
            RelayEvent.RIGHT -> "right"
            RelayEvent.WHEEL_UP -> "wheel up"
            RelayEvent.WHEEL_DOWN -> "wheel down"
            else -> "button $button"
        }

        // Android's values (InputDevice, KeyEvent), here so the JVM tests need no Android class.
        private const val SOURCE_GAMEPAD = 0x00000401
        private const val SOURCE_JOYSTICK = 0x01000010
        private const val KEYCODE_BACK = 4
        private const val KEYCODE_DPAD_UP = 19
        private const val KEYCODE_DPAD_CENTER = 23
        private const val KEYCODE_BUTTON_A = 96
        private const val KEYCODE_BUTTON_MODE = 110
        private const val KEYCODE_BUTTON_1 = 188
        private const val KEYCODE_BUTTON_16 = 203
    }
}

/**
 * Detailed logging's pad state lines in `input.log`: the state the game's XInput pad gets (all
 * controllers and the on-screen controls together), on each change of a button or a trigger, and
 * for a stick at most every [STICK_MS]: at most 10 lines a second while only the sticks move.
 */
class PadStateLines {
    private var last: PadState? = null
    private var lastAt = 0L

    /** The line for [state] at [now] (ms), or null. */
    fun next(state: PadState, now: Long): String? {
        val before = last
        if (before == state) return null
        val keys = before == null || state.buttons != before.buttons || state.leftTrigger != before.leftTrigger ||
            state.rightTrigger != before.rightTrigger
        if (!keys && now - lastAt < STICK_MS) return null
        last = state
        lastAt = now
        return String.format(
            Locale.ROOT, "pad state: buttons 0x%04x, LT %.2f, RT %.2f, left %.2f,%.2f, right %.2f,%.2f",
            state.buttons, state.leftTrigger / 255f, state.rightTrigger / 255f,
            state.lx / 32767f, state.ly / 32767f, state.rx / 32767f, state.ry / 32767f,
        )
    }

    companion object {
        const val STICK_MS = 100L
    }
}
