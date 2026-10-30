package com.pd2launcher.app.input

import com.pd2launcher.app.GameResolution

/**
 * The pointer area that one relay (its X screen) has: the game's picture, where the pointer's moves
 * by a distance stop (owner, 2026-10-28: the pointer never goes past the screen's edges or where
 * the game draws no cursor). x-input-relay stops each [RelayEvent.Relative] (Trackpad, the
 * controller's mouse mode) at the area's edges, from where the X screen's pointer really is. The
 * game and Wine move the pointer too (PD2 puts it on the character when the controller's stick
 * walks), so a copy of the pointer's place in the app goes wrong and stops moves at a false edge,
 * as in 0.3.5. All events go through [withArea] (the Game view's `send`): it gives the relay the
 * area ([RelayEvent.Area]) before the first events and before the next events after each change.
 * [RelayEvent.Motion] (Direct touch, a mouse) and [RelayEvent.Relative] go as they are. One per
 * relay.
 */
class PointerBounds {
    /** The area last given to the relay; null before the first events. */
    private var sent: GameResolution.PictureRect? = null

    /**
     * [events] as they go to X: after [area] (X screen pixels; [GameResolution.pointerArea]) where it
     * is not the area last given to the relay, else as they are.
     */
    @Synchronized
    fun withArea(events: List<RelayEvent>, area: GameResolution.PictureRect): List<RelayEvent> {
        if (area == sent) return events
        sent = area
        return listOf(RelayEvent.Area(area)) + events
    }
}
