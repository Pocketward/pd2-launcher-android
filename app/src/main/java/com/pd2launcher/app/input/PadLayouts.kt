package com.pd2launcher.app.input

import android.content.Context
import android.hardware.input.InputManager
import android.os.Handler
import android.os.Looper
import android.view.InputDevice
import android.view.KeyEvent
import android.view.MotionEvent

/** What Android tells of [device], as [PadLayout.pick] reads it (InputDevice.hasKeys asks the system). */
fun padDevice(device: InputDevice): PadDevice {
    val has = device.hasKeys(*PadLayout.PROBE_KEYS)
    val keys = PadLayout.PROBE_KEYS.filterIndexedTo(HashSet()) { index, _ -> has[index] }
    val axes = device.motionRanges
        .filter { it.isFromSource(InputDevice.SOURCE_JOYSTICK) }
        .associate { it.axis to AxisRange(it.min, it.max, it.flat, it.fuzz) }
    return PadDevice(device.vendorId, device.productId, keys, axes)
}

/** [device]'s facts if it is a controller ([isController]); null for any other device. */
fun controllerOf(device: InputDevice): PadDevice? =
    if (isController(device.sources, device.isVirtual)) padDevice(device) else null

/**
 * Each controller's layout ([PadLayout.pick]) for the whole app, applied where Android's events
 * come in: its keys' keycodes and its axes, before [PadInput]. A device is read once per device
 * object, as early as possible: from [watch] on, each one as it connects or changes, also while
 * the app is in the background; the controllers log ([ControllerLog]) and the Game view use that
 * same pick, so the log names the layout the game reads it with. Read early because some systems
 * take a controller over after it connects and then answer for it with other keys and axes
 * without telling apps (the RP Mini V2's firmware gives an attached controller its built-in
 * pad's). Main thread only.
 */
object PadLayouts {
    private val cache = LayoutCache()
    private val keys = PadKeys()
    private var watching = false

    private val listener = object : InputManager.InputDeviceListener {
        override fun onInputDeviceAdded(deviceId: Int) = read(deviceId)
        override fun onInputDeviceChanged(deviceId: Int) = read(deviceId)
        override fun onInputDeviceRemoved(deviceId: Int) {
            cache.forget(deviceId)
            keys.forget(deviceId)
        }
    }

    private fun read(deviceId: Int) {
        InputDevice.getDevice(deviceId)?.let(::entry)
    }

    /** From now on, for the life of the process, reads each device as it connects or changes; reads those there now. */
    fun watch(context: Context) {
        if (watching) return
        watching = true
        context.applicationContext.getSystemService(InputManager::class.java)
            .registerInputDeviceListener(listener, Handler(Looper.getMainLooper()))
        InputDevice.getDeviceIds().forEach(::read)
    }

    /** [device]'s entry: its facts (null: not a controller) and its pick. */
    fun entry(device: InputDevice): LayoutCache.Entry = cache.of(device.id, device) { controllerOf(device) }

    /**
     * [event] with the keycode its controller's layout gives it ([PadKeys]); [event] itself for a
     * key of another device, a fallback key Android made of an unused one, an event this object
     * already gave out (the Game view's output, back through the activity's dispatch), or when the
     * layout gives the same keycode. A release gets the keycode its press got.
     */
    fun keyEvent(event: KeyEvent): KeyEvent {
        val pad = event.isFromSource(InputDevice.SOURCE_GAMEPAD) || event.isFromSource(InputDevice.SOURCE_JOYSTICK)
        val layout = if (pad) event.device?.let(::entry)?.pick?.layout else null
        val code = keys.keyCode(
            event.deviceId, event.eventTime, event.action, event.repeatCount, event.flags, event.scanCode, event.keyCode, pad, layout,
        )
        if (code == event.keyCode) return event
        return KeyEvent(
            event.downTime, event.eventTime, event.action, code, event.repeatCount, event.metaState,
            event.deviceId, event.scanCode, event.flags, event.source,
        )
    }

    /**
     * The keycode the game's pad gets for [event] (after [keyEvent]): a physical Microsoft
     * controller's Back as Select ([PadLayout.gameKeyCode]). The Game view uses it only for keys
     * that go to the game.
     */
    fun gameKeyCode(event: KeyEvent): Int {
        val device = event.device
        val pad = event.isFromSource(InputDevice.SOURCE_GAMEPAD) || event.isFromSource(InputDevice.SOURCE_JOYSTICK)
        val physical = device?.isVirtual == false
        return PadLayout.gameKeyCode(event.keyCode, device?.vendorId ?: 0, pad && physical, (event.flags and KeyEvent.FLAG_FALLBACK) != 0)
    }

    /** A joystick motion event's sticks, triggers and hat, as its controller's layout reads them. */
    fun axes(event: MotionEvent): PadAxes {
        val entry = event.device?.let(::entry)
        val layout = entry?.pick?.layout ?: PadLayout.STANDARD
        return layout.axes(entry?.device, { event.getAxisValue(it) }, entry?.moved ?: HashSet())
    }
}
