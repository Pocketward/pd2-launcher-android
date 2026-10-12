package com.pd2launcher.app.ui

import android.app.Activity
import android.content.pm.ActivityInfo
import androidx.compose.runtime.staticCompositionLocalOf

/**
 * Which layout the launcher screens use, chosen once from the display's shape, never from the
 * current orientation or width: a long, narrow display ([PHONE_RATIO] or more, long side over
 * short side) is a phone, laid out as one column in portrait with the actions pinned at the
 * bottom; anything squarer is a handheld, laid out landscape with the step's words left and its
 * evidence right. The game is landscape on both.
 */
enum class FormFactor {
    PHONE,
    HANDHELD,
    ;

    /** The launcher screens' orientation; the Game view is always sensor landscape. */
    val launcherOrientation: Int get() = when (this) {
        PHONE -> ActivityInfo.SCREEN_ORIENTATION_SENSOR_PORTRAIT
        HANDHELD -> ActivityInfo.SCREEN_ORIENTATION_SENSOR_LANDSCAPE
    }

    companion object {
        /** 1604 x 720 phones are 2.23; 16:9 handhelds 1.78, 4:3 ones 1.33. */
        const val PHONE_RATIO = 1.9f

        fun of(widthPx: Int, heightPx: Int): FormFactor {
            val long = maxOf(widthPx, heightPx).toFloat()
            val short = minOf(widthPx, heightPx).coerceAtLeast(1)
            return if (long / short >= PHONE_RATIO) PHONE else HANDHELD
        }

        /** From the physical display: the window's maximum bounds. */
        fun of(activity: Activity): FormFactor {
            val bounds = activity.windowManager.maximumWindowMetrics.bounds
            return of(bounds.width(), bounds.height())
        }
    }
}

/** The form factor the screens lay out for; MainActivity provides it. */
val LocalFormFactor = staticCompositionLocalOf { FormFactor.HANDHELD }
