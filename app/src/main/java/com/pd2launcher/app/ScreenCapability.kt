package com.pd2launcher.app

/**
 * What the screen can show the app: the rate that the screen held for the app, measured for the
 * panel whose modes have the [PanelModes.key] [panel]. Android's modes say what the panel can do,
 * and `peak_refresh_rate` is not always there or right (no value on a phone with Smooth Display
 * off; 120.00001 on a 144 Hz tablet while OxygenOS gave the app 144 Hz): only a
 * measurement says what the phone gives the app.
 *
 * It informs the player (the help line of the Frame rate input) and `device.txt`. Its one other
 * use is the number that an older "Full" becomes, which is then the player's stored pick. It is
 * never an input of the clock ([FrameClock]) or of the request ([ScreenRequest]) during a game.
 */
data class ScreenCapability(val hertz: Int, val panel: String) {
    fun isOf(modes: PanelModes): Boolean = panel == modes.key

    companion object {
        /** The measurement on these [modes]; before one, the highest mode that the phone's limit allows, which is the panel's highest where the phone sets no limit. */
        fun upTo(measured: ScreenCapability?, modes: PanelModes, cap: PhoneRefreshCap): Int =
            measured?.takeIf { it.isOf(modes) }?.hertz ?: cap.allowed(modes).maxOfOrNull { Math.round(it.rate) } ?: FramePick.most(modes)

        /**
         * The measurement after the screen held [hertz]. While the window asked for the panel's
         * highest mode ([asksHighest]) the reading is what the phone gives the app that asks for
         * all that the panel can do, so it replaces what was known. Under another request (the game's own) a lower reading says
         * nothing about what the screen can do, so only a higher one counts. A rate that is none of the panel's modes is the
         * phone slowing a still screen for a moment (45 Hz on a 60/90 Hz panel), not what it can show: it
         * counts for nothing. Null: nothing new.
         */
        fun after(measured: ScreenCapability?, modes: PanelModes, cap: PhoneRefreshCap, hertz: Int, asksHighest: Boolean): ScreenCapability? {
            if (modes.modes.isNotEmpty() && modes.modes.none { Math.round(it.rate) == hertz }) return null
            val next = ScreenCapability(hertz, modes.key)
            return next.takeIf { it != measured && (asksHighest || hertz > upTo(measured, modes, cap)) }
        }
    }
}

/**
 * The rule of one measurement that starts at [start] (nanoseconds): a rate counts once the screen
 * held it [HOLD_NANOS] and [CHANGE_NANOS] have passed since the start, so that a request for
 * another mode had the time to change it. No rate after [LIMIT_NANOS]. Whole rates: 59.94 and
 * 60.000004 Hz are one.
 */
class SettledRate(private val start: Long) {
    private var hertz = 0
    private var since = start

    fun reading(rate: Float, now: Long) {
        val read = Math.round(rate)
        if (read == hertz) return
        hertz = read
        since = now
    }

    fun settled(now: Long): Int? = hertz.takeIf { it > 0 && now - since >= HOLD_NANOS && now - start >= CHANGE_NANOS && !over(now) }

    fun over(now: Long): Boolean = now - start > LIMIT_NANOS

    companion object {
        const val HOLD_NANOS = 1_000_000_000L

        /** A margin, not a measured time: a 120 Hz handheld changed from 60 to 120 Hz in less than 1 s. */
        const val CHANGE_NANOS = 2_000_000_000L

        const val LIMIT_NANOS = 4_000_000_000L
    }
}
