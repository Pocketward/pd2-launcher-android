package com.pd2launcher.app

object FramePick {
    /** Diablo II's own 25 steps a second. */
    const val MIN = 25

    const val DEFAULT = 60

    const val STEP = 5

    /** What earlier versions stored for "Full", the screen's full rate that they measured: it becomes what the screen can show ([ScreenCapability]). */
    const val OLDER_FULL = 0

    /** Rounded, because a 60 Hz panel's modes read 60.000004 or 59.94. A panel that cannot be read counts as a [DEFAULT] Hz panel. */
    fun most(modes: PanelModes): Int = modes.modes.maxOfOrNull { Math.round(it.rate) }?.takeIf { it >= MIN } ?: DEFAULT

    fun onPanel(stored: Int, modes: PanelModes): Int = stored.coerceIn(MIN, most(modes))

    fun typed(text: String, modes: PanelModes): Int? = text.trim().toIntOrNull()?.takeIf { it in MIN..most(modes) }

    fun stepped(value: Int, steps: Int, modes: PanelModes): Int {
        val grid = if (steps > 0) Math.floorDiv(value, STEP) else -Math.floorDiv(-value, STEP)
        return ((grid + steps) * STEP).coerceIn(MIN, most(modes))
    }

    fun intervalNanos(pick: Int): Long = 1_000_000_000L / pick
}
