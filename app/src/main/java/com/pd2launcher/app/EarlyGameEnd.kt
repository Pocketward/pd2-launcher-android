package com.pd2launcher.app

/**
 * A game that ended within [WINDOW_MS] of starting. The engine reports any end of the game's
 * process as the game having exited, without its exit status, and Wine logs nothing by default
 * (WINEDEBUG=-all), so such an end leaves no error of its own. The service log then gets what can
 * still be read before the game environment stops: the app's processes still running (a Wine
 * server left from before, say).
 */
object EarlyGameEnd {
    const val WINDOW_MS = 10_000L

    /** A game session that started at [startedAt] (null: not seen starting) ended at [endedAt] early. */
    fun early(startedAt: Long?, endedAt: Long): Boolean = startedAt != null && endedAt - startedAt < WINDOW_MS
}
