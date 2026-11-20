package com.pd2launcher.app.input

/**
 * A key shortcut that a Quick tile presses in the game, as a keyboard does: Ctrl, then the
 * letter, held for [HOLD_MS], then let go in the other order. [COPY]: BH copies the item under the
 * pointer, and its text goes to Android's clipboard ([GameClipboard]). [PASTE]: the open chat line
 * takes the clipboard's text, which is Android's.
 */
enum class GameShortcut(val keysyms: List<Int>) {
    COPY(listOf(Keys.XK_CONTROL_L, 'c'.code)),
    PASTE(listOf(Keys.XK_CONTROL_L, 'v'.code)),
    ;

    fun down(): List<RelayEvent> = keysyms.map { RelayEvent.Key(it, true) }

    companion object {
        /** About a quick press of a real key: the game reads its keys between frames, so a press of no time can fall between two. */
        const val HOLD_MS = 100L

        /** [keysyms] let go, the last pressed first. */
        fun up(keysyms: List<Int>): List<RelayEvent> = keysyms.reversed().map { RelayEvent.Key(it, false) }
    }
}
