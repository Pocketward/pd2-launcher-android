package com.pd2launcher.app

import android.content.Context
import android.hardware.display.DisplayManager
import android.view.Display
import java.util.Locale

data class PanelMode(val id: Int, val rate: Float)

/** The modes are what the panel can do, whatever the phone's settings let it do ([PhoneRefreshCap]). */
data class PanelModes(val modes: List<PanelMode>) {
    val highest: PanelMode? get() = modes.maxByOrNull { it.rate }

    /** The rates of the modes, sorted: other rates are another panel, and a measurement of one is not a fact of the other ([ScreenCapability]). */
    val key: String get() = modes.map { it.rate }.sorted().map { String.format(Locale.ROOT, "%.2f", it) }.distinct().joinToString(",")

    companion object {
        fun of(display: Display?): PanelModes {
            display ?: return PanelModes(emptyList())
            val now = display.mode
            return PanelModes(
                display.supportedModes
                    .filter { it.physicalWidth == now.physicalWidth && it.physicalHeight == now.physicalHeight }
                    .map { PanelMode(it.modeId, it.refreshRate) },
            )
        }

        fun builtIn(context: Context): PanelModes =
            of(context.getSystemService(DisplayManager::class.java)?.getDisplay(Display.DEFAULT_DISPLAY))
    }
}
