package com.pd2launcher.app

import android.content.Context
import android.provider.Settings

/**
 * The phone's display settings (Smooth Display, Screen refresh rate) write Android's
 * `Settings.System` values `peak_refresh_rate` and `min_refresh_rate`. On stock Android 13, a
 * request for the 120 Hz mode got the rate of `peak_refresh_rate` (60 or 120).
 */
data class PhoneRefreshCap(val peak: Float?, val min: Float?) {
    /**
     * Android's display policy lets the screen run up to the higher of the two values. A peak
     * value of 0 sets no limit there, and Android's "infinity" stands for the highest mode.
     */
    val limit: Float?
        get() {
            val top = peak?.takeIf { it > 0f } ?: return null
            return maxOf(top, min ?: 0f).takeIf { it.isFinite() }
        }

    /** [RATE_SLACK]: a limit reads 144.00002 for a 144 Hz mode. */
    fun allowed(modes: PanelModes): List<PanelMode> {
        val limit = limit ?: return modes.modes
        return modes.modes.filter { it.rate <= limit + RATE_SLACK }.ifEmpty { listOfNotNull(modes.modes.minByOrNull { it.rate }) }
    }

    companion object {
        private const val RATE_SLACK = 0.5f

        val NONE = PhoneRefreshCap(null, null)

        fun read(context: Context): PhoneRefreshCap = PhoneRefreshCap(value(context, "peak_refresh_rate"), value(context, "min_refresh_rate"))

        private fun value(context: Context, name: String): Float? = try {
            Settings.System.getFloat(context.contentResolver, name)
        } catch (_: Settings.SettingNotFoundException) {
            null
        } catch (_: SecurityException) {
            null
        }
    }
}
