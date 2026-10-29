package com.pd2launcher.app.input

import com.pd2launcher.app.GameResolution

/**
 * The X screen's pointer as the app moves it, kept inside the game's picture for the moves by a
 * distance (owner, 2026-10-28: the pointer never goes past the screen's edges or where the game
 * draws no cursor). The app makes every move of the pointer, and all of them go through [place]
 * (the Game view's `send`): a move to a place ([RelayEvent.Motion]: Direct touch, a mouse) puts it
 * there; a move by a distance ([RelayEvent.Relative]: Trackpad, the controller's mouse mode) goes
 * only as far as the area's edge, and one from outside the area (a Direct touch or a mouse put it
 * there, or the area changed) comes in at the nearest edge. The area itself lies inside the X
 * screen, so X never stops a move short and this position stays X's own. It starts in the X
 * screen's middle ([width] x [height]), where Xvfb puts the pointer. One per relay (its X screen).
 */
class PointerBounds(width: Int, height: Int) {
    private var x = width / 2
    private var y = height / 2

    /**
     * [events] as they go to X: each move by a distance kept inside [area] (X screen pixels;
     * [GameResolution.pointerArea]), or left out where it moves nothing; the other events as they are.
     */
    @Synchronized
    fun place(events: List<RelayEvent>, area: GameResolution.PictureRect): List<RelayEvent> {
        if (events.none { it is RelayEvent.Motion || it is RelayEvent.Relative }) return events
        val placed = ArrayList<RelayEvent>(events.size)
        for (event in events) {
            when (event) {
                is RelayEvent.Motion -> {
                    x = event.x
                    y = event.y
                    placed += event
                }
                is RelayEvent.Relative -> {
                    val toX = (x + event.dx).coerceIn(area.left, area.left + area.width - 1)
                    val toY = (y + event.dy).coerceIn(area.top, area.top + area.height - 1)
                    if (toX != x || toY != y) placed += RelayEvent.Relative(toX - x, toY - y)
                    x = toX
                    y = toY
                }
                else -> placed += event
            }
        }
        return placed
    }
}
