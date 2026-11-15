package com.pd2launcher.app

/**
 * The request follows from the pick, the panel's modes and the phone's own limit only ([of]),
 * never from a reading of the rate the screen runs at ([ScreenRate]): a request that followed
 * readings changed the screen's mode during play. Android and the phone's maker decide what the
 * screen then runs at (OxygenOS gives a window that asks for any mode its own rate for the app).
 */
data class ScreenRequest(val pick: Int, val mode: PanelMode?) {
    val rate: Float get() = mode?.rate ?: 0f

    /** 0 is `preferredDisplayModeId`'s "no preference". */
    val modeId: Int get() = mode?.id ?: 0

    val multiple: Int? get() = mode?.let { multiple(it.rate, pick) }

    /** The screen is asked to show more frames a second than the game draws: the present server then holds frames with vsync off to the clock too ([PresentServer.attach]). */
    val abovePick: Boolean get() = pick < Math.round(rate)

    companion object {
        /**
         * A mode whose rate is a whole multiple of the pick shows every frame for the same number
         * of refreshes. With no such mode, the highest mode shows the most of the game's frames.
         */
        fun of(pick: Int, modes: PanelModes, cap: PhoneRefreshCap): ScreenRequest {
            val allowed = cap.allowed(modes)
            return ScreenRequest(pick, allowed.filter { multiple(it.rate, pick) != null }.maxByOrNull { it.rate } ?: allowed.maxByOrNull { it.rate })
        }

        /** [MULTIPLE_SLACK] is for 59.94 and 119.88 Hz modes. */
        private fun multiple(rate: Float, pick: Int): Int? {
            val ratio = rate / pick
            return Math.round(ratio).takeIf { it >= 1 && Math.abs(ratio - it) <= MULTIPLE_SLACK }
        }

        private const val MULTIPLE_SLACK = 0.01f
    }
}
