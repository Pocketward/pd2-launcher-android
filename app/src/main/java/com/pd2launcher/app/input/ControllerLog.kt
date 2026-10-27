package com.pd2launcher.app.input

import android.content.Context
import android.hardware.input.InputManager
import android.os.Handler
import android.os.Looper
import android.view.InputDevice

/**
 * The controllers log (`files/logs/controllers.log`, which Share logs packs): one line for each
 * controller ([isController]) when it connects, and again only when its line changes (the device
 * changed); when the app comes to the front, lines for controllers it has not written yet and
 * "gone" for those removed while it was away. A line has the name, Android's vendor and product
 * ids, the controller keys and the axes (with their ranges, flat and fuzz) Android gives it,
 * whether those axes look like the xpad driver's ([PadLayout.xpadAxes], evidence only), its
 * sources, whether Android calls it external, and the layout the Game view reads it with and why
 * ([PadLayout.pick]). No device descriptor: Android derives it from ids that can include the
 * controller's Bluetooth address, and players share these logs. A player's shared logs so tell
 * what their controller sends. [write] takes one line; [start] and [stop] follow the activity.
 */
class ControllerLog(context: Context, private val write: (String) -> Unit) : InputManager.InputDeviceListener {
    private val manager = context.getSystemService(InputManager::class.java)

    /** The line last written for each device id: a device is written again only when it changes. */
    private val written = HashMap<Int, String>()

    fun start() {
        manager.registerInputDeviceListener(this, Handler(Looper.getMainLooper()))
        // Gone while the app was away.
        written.keys.filter { InputDevice.getDevice(it) == null }.forEach(::onInputDeviceRemoved)
        InputDevice.getDeviceIds().forEach(::log)
    }

    fun stop() = manager.unregisterInputDeviceListener(this)

    override fun onInputDeviceAdded(deviceId: Int) = log(deviceId)

    override fun onInputDeviceChanged(deviceId: Int) = log(deviceId)

    override fun onInputDeviceRemoved(deviceId: Int) {
        if (written.remove(deviceId) != null) write("controller $deviceId gone")
    }

    private fun log(deviceId: Int) {
        val device = InputDevice.getDevice(deviceId) ?: return
        // The Game view's own entry ([PadLayouts]): read now, as the controller connects.
        val entry = PadLayouts.entry(device)
        val pad = entry.device ?: return
        val pick = entry.pick
        // Android gives apps no bus type. "external" is Android's word only: some built-in pads say
        // true (the RP Mini V2's does). xpadAxes is evidence only; the pick does not use it.
        val line = "controller $deviceId \"${device.name}\" ${PadLayout.describe(pad)} " +
            "xpadAxes=${if (PadLayout.xpadAxes(pad)) "yes" else "no"} " +
            "sources=0x${Integer.toHexString(device.sources)} external=${device.isExternal} " +
            "layout=${pick.layout} (${pick.reason})"
        if (written.put(deviceId, line) != line) write(line)
    }
}
