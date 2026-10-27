package com.pd2launcher.app.input

import android.view.KeyEvent
import android.view.MotionEvent
import java.util.Locale

/**
 * One joystick axis as Android describes it (`InputDevice.MotionRange` on the joystick source), in
 * Android's units: its range, its flat (the dead zone around the centre) and its fuzz (the noise
 * Android filters out), both scaled from the driver's raw values as the range is.
 */
data class AxisRange(val min: Float, val max: Float, val flat: Float = 0f, val fuzz: Float = 0f)

/**
 * One controller as Android describes it, the facts [PadLayout.pick] reads: its USB/Bluetooth
 * vendor and product ids, which of [PadLayout.PROBE_KEYS] Android says it has
 * (`InputDevice.hasKeys`), and its joystick axes ([AxisRange], `InputDevice.getMotionRange`).
 */
data class PadDevice(
    val vendorId: Int,
    val productId: Int,
    val keys: Set<Int>,
    val axes: Map<Int, AxisRange>,
)

/**
 * How a controller's buttons are read. Android maps each raw button (a Linux key code, the key
 * event's scan code) to a keycode with a key layout file for the controller's vendor and product
 * ids, or with its generic file when it has none.
 */
enum class ButtonTable {
    /** Android's keycodes as they come. */
    ANDROID {
        override fun keyCode(androidKeyCode: Int, scanCode: Int): Int = androidKeyCode
    },

    /**
     * HID buttons 1 to 10 in the order Windows gives an Xbox controller (A, B, X, Y, L1, R1,
     * Select, Start, L3, R3), read by scan code: Linux names a "Game Pad" descriptor's button n
     * BTN_GAMEPAD + n - 1 and a "Joystick" one's BTN_JOYSTICK + n - 1. A scan code it does not know
     * keeps Android's keycode.
     */
    HID_ORDER {
        override fun keyCode(androidKeyCode: Int, scanCode: Int): Int = HID_BUTTONS[scanCode] ?: androidKeyCode
    },

    /**
     * Linux's gamepad codes, as an Xbox controller in Android mode or over Bluetooth LE sends them,
     * read by scan code: BTN_A, BTN_B, BTN_X, BTN_Y, BTN_TL, BTN_TR, BTN_SELECT, BTN_START,
     * BTN_MODE, BTN_THUMBL, BTN_THUMBR as A, B, X, Y, L1, R1, Select, Start, Mode (Guide; the
     * game's pad has none, so it does nothing), L3, R3. For a pad whose firmware sends this order
     * while Android reads it with a layout file for the old order. A scan code it does not know
     * keeps Android's keycode.
     */
    LINUX_ORDER {
        override fun keyCode(androidKeyCode: Int, scanCode: Int): Int = LINUX_BUTTONS[scanCode] ?: androidKeyCode
    },
    ;

    /** The keycode a button means: Android's [androidKeyCode], or what its [scanCode] is in this table. */
    abstract fun keyCode(androidKeyCode: Int, scanCode: Int): Int

    private companion object {
        const val BTN_GAMEPAD = 0x130
        const val BTN_JOYSTICK = 0x120

        /** Linux's gamepad codes (`input-event-codes.h`) and the keycodes they mean. */
        val LINUX_BUTTONS: Map<Int, Int> = mapOf(
            0x130 to KeyEvent.KEYCODE_BUTTON_A, // BTN_A
            0x131 to KeyEvent.KEYCODE_BUTTON_B, // BTN_B
            0x133 to KeyEvent.KEYCODE_BUTTON_X, // BTN_X
            0x134 to KeyEvent.KEYCODE_BUTTON_Y, // BTN_Y
            0x136 to KeyEvent.KEYCODE_BUTTON_L1, // BTN_TL
            0x137 to KeyEvent.KEYCODE_BUTTON_R1, // BTN_TR
            0x13a to KeyEvent.KEYCODE_BUTTON_SELECT, // BTN_SELECT
            0x13b to KeyEvent.KEYCODE_BUTTON_START, // BTN_START
            0x13c to KeyEvent.KEYCODE_BUTTON_MODE, // BTN_MODE
            0x13d to KeyEvent.KEYCODE_BUTTON_THUMBL, // BTN_THUMBL
            0x13e to KeyEvent.KEYCODE_BUTTON_THUMBR, // BTN_THUMBR
        )

        val HID_ORDER_KEYS = intArrayOf(
            KeyEvent.KEYCODE_BUTTON_A, KeyEvent.KEYCODE_BUTTON_B, KeyEvent.KEYCODE_BUTTON_X, KeyEvent.KEYCODE_BUTTON_Y,
            KeyEvent.KEYCODE_BUTTON_L1, KeyEvent.KEYCODE_BUTTON_R1, KeyEvent.KEYCODE_BUTTON_SELECT,
            KeyEvent.KEYCODE_BUTTON_START, KeyEvent.KEYCODE_BUTTON_THUMBL, KeyEvent.KEYCODE_BUTTON_THUMBR,
        )

        val HID_BUTTONS: Map<Int, Int> = buildMap {
            HID_ORDER_KEYS.forEachIndexed { index, key ->
                put(BTN_GAMEPAD + index, key)
                put(BTN_JOYSTICK + index, key)
            }
        }
    }
}

/** How a controller's sticks, triggers and hat are read from a joystick motion event. */
enum class AxisTable {
    /**
     * Android's: the sticks on X/Y and Z/RZ, the triggers the larger of LTRIGGER and BRAKE and
     * of RTRIGGER and GAS, the hat on HAT_X/HAT_Y.
     */
    ANDROID {
        override val needs = emptyList<Int>()

        override fun read(value: (Int) -> Float, moved: MutableSet<Int>): PadAxes = PadAxes(
            lx = value(MotionEvent.AXIS_X),
            ly = value(MotionEvent.AXIS_Y),
            rx = value(MotionEvent.AXIS_Z),
            ry = value(MotionEvent.AXIS_RZ),
            leftTrigger = maxOf(value(MotionEvent.AXIS_LTRIGGER), value(MotionEvent.AXIS_BRAKE)),
            rightTrigger = maxOf(value(MotionEvent.AXIS_RTRIGGER), value(MotionEvent.AXIS_GAS)),
            hatX = value(MotionEvent.AXIS_HAT_X),
            hatY = value(MotionEvent.AXIS_HAT_Y),
        )
    },

    /**
     * The right stick on RX/RY and the triggers on Z and RZ, which Android gives as centred axes:
     * -1 let go, +1 pressed fully, read as (value + 1) / 2. A controller whose triggers Android
     * has no layout file for comes this way (Android's generic file names Z, RZ, RX and RY as
     * they are). The left stick and the hat as [ANDROID].
     *
     * Android gives each axis 0 until the controller sends its first value for it, and a trigger
     * at rest sends none: a trigger axis reads let go until it has given a value other than 0
     * ([moved] keeps which have), so a trigger never reads half pressed before it moves.
     */
    RX_RY_STICK {
        override val needs = listOf(MotionEvent.AXIS_RX, MotionEvent.AXIS_RY, MotionEvent.AXIS_Z, MotionEvent.AXIS_RZ)

        override fun read(value: (Int) -> Float, moved: MutableSet<Int>): PadAxes = PadAxes(
            lx = value(MotionEvent.AXIS_X),
            ly = value(MotionEvent.AXIS_Y),
            rx = value(MotionEvent.AXIS_RX),
            ry = value(MotionEvent.AXIS_RY),
            leftTrigger = centredTrigger(MotionEvent.AXIS_Z, value, moved),
            rightTrigger = centredTrigger(MotionEvent.AXIS_RZ, value, moved),
            hatX = value(MotionEvent.AXIS_HAT_X),
            hatY = value(MotionEvent.AXIS_HAT_Y),
        )

        private fun centredTrigger(axis: Int, value: (Int) -> Float, moved: MutableSet<Int>): Float {
            val now = value(axis)
            if (now != 0f) moved += axis else if (axis !in moved) return 0f
            return ((now + 1f) / 2f).coerceIn(0f, 1f)
        }
    },
    ;

    /** The axes a controller must have (a motion range on the joystick source) for this table to read it. */
    abstract val needs: List<Int>

    /**
     * The axes of one motion event, [value] reading one MotionEvent axis of it; [moved]: the axes
     * of this controller that have given a value other than 0 so far (kept per controller).
     */
    abstract fun read(value: (Int) -> Float, moved: MutableSet<Int> = HashSet()): PadAxes

    /** Whether a controller with [axes] can be read with this table. */
    fun fits(axes: Set<Int>): Boolean = needs.all { it in axes }
}

/** The layout picked for a controller and why, as the controllers log says it. */
data class LayoutPick(val layout: PadLayout, val reason: String)

/**
 * A whole controller layout, a [ButtonTable] and an [AxisTable], applied where Android's events
 * come in, before [PadInput]. Each has a name a later per-controller choice can offer; [pick] is
 * the automatic one.
 */
enum class PadLayout(val buttons: ButtonTable, val axisTable: AxisTable, val label: String) {
    /** Android's own mapping: right for every controller Android has a layout file for. */
    STANDARD(ButtonTable.ANDROID, AxisTable.ANDROID, "Standard"),

    /** Android's buttons; the right stick on RX/RY and the triggers on Z/RZ. */
    WIRED(ButtonTable.ANDROID, AxisTable.RX_RY_STICK, "Wired (right stick on RX/RY, triggers on Z/RZ)"),

    /**
     * Buttons in HID order and the axes as [WIRED]: an Xbox controller in its Windows mode (and
     * pads that act as one) under any product id but 02e0, the one Android has a layout file for.
     */
    BLUETOOTH_GENERIC(ButtonTable.HID_ORDER, AxisTable.RX_RY_STICK, "Bluetooth generic (HID button order)"),

    /**
     * Buttons by Linux's gamepad codes and Android's axes: for an updated-firmware Xbox controller
     * that Android reads with a layout file for the old order (it names every button, but the
     * wrong ones). That case is not proven (no firmware, model or file is known for the SDL rows
     * it rests on), so it is never picked automatically, only a later per-controller choice. It
     * reads the standard codes and does not copy SDL's swapped stick buttons.
     */
    XBOX_LINUX_ORDER(ButtonTable.LINUX_ORDER, AxisTable.ANDROID, "Xbox, Linux order"),
    ;

    fun keyCode(androidKeyCode: Int, scanCode: Int): Int = buttons.keyCode(androidKeyCode, scanCode)

    /**
     * A motion event's axes from [device]: with this layout's axis table where the device has the
     * axes it needs, else with Android's. [moved]: the device's axes that have moved so far
     * ([AxisTable.read]).
     */
    fun axes(device: PadDevice?, value: (Int) -> Float, moved: MutableSet<Int> = HashSet()): PadAxes {
        val table = if (device != null && axisTable.fits(device.axes.keys)) axisTable else AxisTable.ANDROID
        return table.read(value, moved)
    }

    companion object {
        /** USB vendor id of Microsoft, which Xbox controllers and pads in Xbox mode report. */
        const val MICROSOFT = 0x045e

        /** The keycodes [pick] asks Android about. */
        val PROBE_KEYS = intArrayOf(
            KeyEvent.KEYCODE_BUTTON_A, KeyEvent.KEYCODE_BUTTON_B, KeyEvent.KEYCODE_BUTTON_C,
            KeyEvent.KEYCODE_BUTTON_X, KeyEvent.KEYCODE_BUTTON_Y, KeyEvent.KEYCODE_BUTTON_Z,
            KeyEvent.KEYCODE_BUTTON_L1, KeyEvent.KEYCODE_BUTTON_R1, KeyEvent.KEYCODE_BUTTON_L2,
            KeyEvent.KEYCODE_BUTTON_R2, KeyEvent.KEYCODE_BUTTON_SELECT, KeyEvent.KEYCODE_BUTTON_START,
            KeyEvent.KEYCODE_BUTTON_MODE, KeyEvent.KEYCODE_BUTTON_THUMBL, KeyEvent.KEYCODE_BUTTON_THUMBR,
        )

        /**
         * Wired controllers known to send the right stick on RX/RY and the triggers on Z/RZ with
         * no layout file in Android: the PowerA Wired Controller for Xbox Series X|S (20d6:2063)
         * and the PDP Wired Controller for Xbox (0e6f:02f0).
         */
        val WIRED_IDS = setOf(0x20d6 to 0x2063, 0x0e6f to 0x02f0)

        /** The pick for a device that is not a controller, or of which Android says nothing. */
        val DEFAULT = LayoutPick(STANDARD, "Android's own mapping")

        /**
         * The automatic layout, Android's own mapping unless a sign says otherwise:
         * - [WIRED] for the [WIRED_IDS], only when the device has RX, RY, Z and RZ.
         * - [BLUETOOTH_GENERIC] for a controller in Windows' button order that Android left
         *   unmapped: it has A and B but no Start and no left stick button, and it is a Microsoft
         *   one (vendor 045e) or has exactly what Android's generic file gives such a pad (A, B,
         *   C, X, Y, Z, L1, R1, L2, R2 and no Select or Mode; X, Y, Z, RZ, RX and RY and no
         *   trigger axis). Every Xbox controller Android maps right has a Start key (its Menu
         *   button is Linux's start button in Android mode and over Bluetooth LE, and Android's
         *   02e0 file names Start in Windows mode); in Windows' order without that file Menu is
         *   button 8, Linux's right shoulder button, so the pad has none.
         */
        fun pick(device: PadDevice): LayoutPick {
            val id = String.format(Locale.ROOT, "%04x:%04x", device.vendorId, device.productId)
            val axes = device.axes.keys
            if ((device.vendorId to device.productId) in WIRED_IDS) {
                return if (AxisTable.RX_RY_STICK.fits(axes)) {
                    LayoutPick(WIRED, "wired controller $id with RX, RY, Z and RZ")
                } else {
                    LayoutPick(STANDARD, "wired controller $id without RX, RY, Z and RZ: Android's own mapping")
                }
            }
            val keys = device.keys
            val noStart = KeyEvent.KEYCODE_BUTTON_START !in keys && KeyEvent.KEYCODE_BUTTON_THUMBL !in keys &&
                KeyEvent.KEYCODE_BUTTON_A in keys && KeyEvent.KEYCODE_BUTTON_B in keys
            if (noStart && device.vendorId == MICROSOFT) {
                return LayoutPick(BLUETOOTH_GENERIC, "Microsoft controller $id with A and B but no Start or left stick button: Windows button order")
            }
            if (noStart && GENERIC_WINDOWS_KEYS.all { it in keys } && GENERIC_WINDOWS_ABSENT.none { it in keys } &&
                AxisTable.RX_RY_STICK.fits(axes) && TRIGGER_AXES.none { it in axes }
            ) {
                return LayoutPick(BLUETOOTH_GENERIC, "controller $id with the generic file's Windows-order buttons and axes")
            }
            return DEFAULT
        }

        /** The keycodes Android's generic file gives a Windows-order pad's ten buttons. */
        private val GENERIC_WINDOWS_KEYS = listOf(
            KeyEvent.KEYCODE_BUTTON_C, KeyEvent.KEYCODE_BUTTON_X, KeyEvent.KEYCODE_BUTTON_Y, KeyEvent.KEYCODE_BUTTON_Z,
            KeyEvent.KEYCODE_BUTTON_L1, KeyEvent.KEYCODE_BUTTON_R1, KeyEvent.KEYCODE_BUTTON_L2, KeyEvent.KEYCODE_BUTTON_R2,
        )
        private val GENERIC_WINDOWS_ABSENT = listOf(
            KeyEvent.KEYCODE_BUTTON_SELECT, KeyEvent.KEYCODE_BUTTON_MODE, KeyEvent.KEYCODE_BUTTON_THUMBR,
        )

        /** The axes Android gives triggers. */
        private val TRIGGER_AXES = listOf(
            MotionEvent.AXIS_LTRIGGER, MotionEvent.AXIS_RTRIGGER, MotionEvent.AXIS_BRAKE, MotionEvent.AXIS_GAS,
        )

        /**
         * The keycode the game's pad gets for a key in the Game view: Back from a physical
         * Microsoft controller ([vendorId] 045e; [physicalPad]: a gamepad or joystick source on a
         * real device) is Select, for an Xbox pad that sends its View button as the HID "AC Back"
         * usage, which Android's generic file names KEYCODE_BACK (SDL's Android code treats Back and
         * Select alike); a [fallback] key Android made of an unused one (Back from an unused B)
         * stays Back. Other vendors' Back stays Back, so a handheld's own Back button (the RP Mini
         * V2's pad, 2022:3002) keeps doing nothing in the game (the user's choice). Any other key as
         * it is. For the game only: the app's own screens, dialogs and menus keep Back for going back.
         */
        fun gameKeyCode(keyCode: Int, vendorId: Int, physicalPad: Boolean, fallback: Boolean): Int =
            if (keyCode == KeyEvent.KEYCODE_BACK && vendorId == MICROSOFT && physicalPad && !fallback) {
                KeyEvent.KEYCODE_BUTTON_SELECT
            } else {
                keyCode
            }

        /**
         * Whether [device]'s axes look like those of Linux's xpad driver read with Android's
         * generic file: Z and RZ with no flat (xpad's triggers), RX and RY with a flat (xpad's right
         * stick), and no trigger axis. Evidence for the controllers log only: it never changes the
         * pick ([pick] gives [WIRED] only to the [WIRED_IDS]). It describes the driver's metadata,
         * not which control is which: Sony's driver, for one, gives its sticks no flat.
         */
        fun xpadAxes(device: PadDevice): Boolean {
            val axes = device.axes
            val z = axes[MotionEvent.AXIS_Z] ?: return false
            val rz = axes[MotionEvent.AXIS_RZ] ?: return false
            val rx = axes[MotionEvent.AXIS_RX] ?: return false
            val ry = axes[MotionEvent.AXIS_RY] ?: return false
            return z.flat == 0f && rz.flat == 0f && rx.flat > 0f && ry.flat > 0f && TRIGGER_AXES.none { it in axes }
        }

        /**
         * A controller's ids, [PROBE_KEYS] and axes with their ranges, flat and fuzz, for the
         * controllers log: `045e:02e0 keys=[A,B,…] axes=[X(-1..1 flat=0.00390631 fuzz=0.000488289),…]`.
         */
        fun describe(device: PadDevice): String {
            val keys = device.keys.sorted().joinToString(",") { KEY_NAMES[it] ?: "$it" }
            val axes = device.axes.entries.sortedBy { it.key }.joinToString(",") { (axis, range) ->
                "${axisName(axis)}(${number(range.min)}..${number(range.max)} flat=${number(range.flat)} fuzz=${number(range.fuzz)})"
            }
            return String.format(Locale.ROOT, "%04x:%04x keys=[%s] axes=[%s]", device.vendorId, device.productId, keys, axes)
        }

        /** [value] in at most six significant digits: `1`, `-0.5`, `0.00390631`. */
        private fun number(value: Float): String {
            if (value == value.toLong().toFloat()) return value.toLong().toString()
            val text = String.format(Locale.ROOT, "%.6g", value)
            // Trailing zeros only after a decimal point, never in an exponent or a whole number.
            return if ('.' in text && 'e' !in text) text.trimEnd('0').trimEnd('.') else text
        }

        private fun axisName(axis: Int): String = AXIS_NAMES[axis]
            ?: if (axis in MotionEvent.AXIS_GENERIC_1..MotionEvent.AXIS_GENERIC_16) "GENERIC_${axis - MotionEvent.AXIS_GENERIC_1 + 1}" else "$axis"

        private val KEY_NAMES = mapOf(
            KeyEvent.KEYCODE_BUTTON_A to "A", KeyEvent.KEYCODE_BUTTON_B to "B", KeyEvent.KEYCODE_BUTTON_C to "C",
            KeyEvent.KEYCODE_BUTTON_X to "X", KeyEvent.KEYCODE_BUTTON_Y to "Y", KeyEvent.KEYCODE_BUTTON_Z to "Z",
            KeyEvent.KEYCODE_BUTTON_L1 to "L1", KeyEvent.KEYCODE_BUTTON_R1 to "R1", KeyEvent.KEYCODE_BUTTON_L2 to "L2",
            KeyEvent.KEYCODE_BUTTON_R2 to "R2", KeyEvent.KEYCODE_BUTTON_SELECT to "SELECT", KeyEvent.KEYCODE_BUTTON_START to "START",
            KeyEvent.KEYCODE_BUTTON_MODE to "MODE", KeyEvent.KEYCODE_BUTTON_THUMBL to "THUMBL", KeyEvent.KEYCODE_BUTTON_THUMBR to "THUMBR",
        )

        private val AXIS_NAMES = mapOf(
            MotionEvent.AXIS_X to "X", MotionEvent.AXIS_Y to "Y", MotionEvent.AXIS_Z to "Z", MotionEvent.AXIS_RX to "RX",
            MotionEvent.AXIS_RY to "RY", MotionEvent.AXIS_RZ to "RZ", MotionEvent.AXIS_HAT_X to "HAT_X", MotionEvent.AXIS_HAT_Y to "HAT_Y",
            MotionEvent.AXIS_LTRIGGER to "LTRIGGER", MotionEvent.AXIS_RTRIGGER to "RTRIGGER", MotionEvent.AXIS_THROTTLE to "THROTTLE",
            MotionEvent.AXIS_RUDDER to "RUDDER", MotionEvent.AXIS_WHEEL to "WHEEL", MotionEvent.AXIS_GAS to "GAS", MotionEvent.AXIS_BRAKE to "BRAKE",
        )
    }
}

/**
 * Each device's [LayoutPick], made once per device object: Android gives a device that changes
 * or connects again a new object (one that connects again also a new id), which is read and picked
 * anew. The pick follows what the device is (its ids, keys and axes), never its id.
 */
class LayoutCache {
    /** A device's facts (null: not a controller), its pick, and its axes that have moved ([AxisTable.read]). */
    class Entry(val token: Any, val device: PadDevice?, val pick: LayoutPick) {
        val moved = HashSet<Int>()
    }

    private val entries = HashMap<Int, Entry>()

    /** The entry for device [id] as [token] (its device object) is; [read] gives its facts (null: not a controller). */
    fun of(id: Int, token: Any, read: () -> PadDevice?): Entry {
        entries[id]?.takeIf { it.token === token }?.let { return it }
        val device = read()
        return Entry(token, device, device?.let(PadLayout::pick) ?: PadLayout.DEFAULT).also { entries[id] = it }
    }

    /** Device [id] is gone. */
    fun forget(id: Int) {
        entries.remove(id)
    }
}

/**
 * The keycode each held button was given when it went down, so its release gives the same one
 * even if the controller's layout changed in between (a device read again): the game's pad never
 * keeps a button that was let go.
 */
class HeldButtons {
    private val held = HashMap<Long, Int>()

    /**
     * The keycode for a key event of device [deviceId] with [scanCode]: [mapped] (the layout's
     * keycode now) for a first press, which it keeps; the kept one for a repeat and the release.
     * A key with no scan code (an injected one) is not kept.
     */
    fun code(deviceId: Int, scanCode: Int, down: Boolean, repeat: Boolean, mapped: Int): Int {
        if (scanCode <= 0) return mapped
        val key = (deviceId.toLong() shl 32) or scanCode.toLong()
        return when {
            down && !repeat -> mapped.also { held[key] = it }
            down -> held[key] ?: mapped
            else -> held.remove(key) ?: mapped
        }
    }

    /** Device [id] is gone: what it held is forgotten. */
    fun forget(id: Int) {
        held.keys.removeAll { (it ushr 32).toInt() == id }
    }
}

/**
 * The keycode of each controller key as its layout reads it, once per key event. The Game view
 * reads an event first, and its output comes back through the activity's own dispatch (and
 * `navigationKey`): that second pass returns the keycode unchanged, so a release whose pick changed
 * during the press keeps its press's keycode on every path. A release and a repeat get the keycode
 * their press got ([HeldButtons]).
 */
class PadKeys {
    private val held = HeldButtons()

    /** One key event as this class tells events apart. */
    private data class KeyId(val deviceId: Int, val eventTime: Long, val action: Int, val repeatCount: Int, val scanCode: Int, val keyCode: Int)

    /** The last event this class gave out, with its keycode. */
    private var lastOut: KeyId? = null

    /**
     * The keycode for a key event: [keyCode] itself for a key that is not from a controller
     * ([padSource] false, or [layout] null), a fallback key Android made of an unused one
     * ([flags] has FLAG_FALLBACK), or this class's own output read a second time; else [layout]'s
     * keycode, a release's and a repeat's the one their press got.
     */
    fun keyCode(
        deviceId: Int, eventTime: Long, action: Int, repeatCount: Int, flags: Int, scanCode: Int, keyCode: Int,
        padSource: Boolean, layout: PadLayout?,
    ): Int {
        if (!padSource || layout == null || (flags and KeyEvent.FLAG_FALLBACK) != 0) return keyCode
        val id = KeyId(deviceId, eventTime, action, repeatCount, scanCode, keyCode)
        if (id == lastOut) return keyCode
        val down = action == KeyEvent.ACTION_DOWN
        val code = held.code(deviceId, scanCode, down, down && repeatCount > 0, layout.keyCode(keyCode, scanCode))
        lastOut = id.copy(keyCode = code)
        return code
    }

    /** Device [id] is gone. */
    fun forget(id: Int) = held.forget(id)
}
