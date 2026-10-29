package com.pd2launcher.app.input

import android.view.KeyEvent

/**
 * Keys into the X screen: a hardware keyboard's Android keycodes map to the X keysyms of the
 * same physical keys (modifiers are keys of their own, so Shift + A reaches X as Shift_L then a,
 * exactly as on a desktop keyboard); text from Android's keyboard (`commitText`) maps character
 * by character to keysyms, which x-input-relay types with Shift or a spare key as needed.
 */
object Keys {
    // X keysyms (X11/keysymdef.h).
    const val XK_BACKSPACE = 0xff08
    const val XK_TAB = 0xff09
    const val XK_RETURN = 0xff0d
    const val XK_PAUSE = 0xff13
    const val XK_SCROLL_LOCK = 0xff14
    const val XK_ESCAPE = 0xff1b
    const val XK_HOME = 0xff50
    const val XK_LEFT = 0xff51
    const val XK_UP = 0xff52
    const val XK_RIGHT = 0xff53
    const val XK_DOWN = 0xff54
    const val XK_PAGE_UP = 0xff55
    const val XK_PAGE_DOWN = 0xff56
    const val XK_END = 0xff57
    const val XK_PRINT = 0xff61
    const val XK_INSERT = 0xff63
    const val XK_MENU = 0xff67
    const val XK_NUM_LOCK = 0xff7f
    const val XK_KP_ENTER = 0xff8d
    const val XK_KP_MULTIPLY = 0xffaa
    const val XK_KP_ADD = 0xffab
    const val XK_KP_SUBTRACT = 0xffad
    const val XK_KP_DECIMAL = 0xffae
    const val XK_KP_DIVIDE = 0xffaf
    const val XK_KP_0 = 0xffb0
    const val XK_F1 = 0xffbe
    const val XK_SHIFT_L = 0xffe1
    const val XK_SHIFT_R = 0xffe2
    const val XK_CONTROL_L = 0xffe3
    const val XK_CONTROL_R = 0xffe4
    const val XK_CAPS_LOCK = 0xffe5
    const val XK_ALT_L = 0xffe9
    const val XK_ALT_R = 0xffea
    const val XK_SUPER_L = 0xffeb
    const val XK_SUPER_R = 0xffec
    const val XK_DELETE = 0xffff

    /** Unicode characters outside Latin-1 are keysyms 0x01000000 + code point. */
    private const val UNICODE_KEYSYM = 0x01000000

    private val table: Map<Int, Int> = buildMap {
        for (i in 0..25) put(KeyEvent.KEYCODE_A + i, 'a'.code + i)
        for (i in 0..9) put(KeyEvent.KEYCODE_0 + i, '0'.code + i)
        for (i in 0..9) put(KeyEvent.KEYCODE_NUMPAD_0 + i, XK_KP_0 + i)
        for (i in 0..11) put(KeyEvent.KEYCODE_F1 + i, XK_F1 + i)
        put(KeyEvent.KEYCODE_SPACE, ' '.code)
        put(KeyEvent.KEYCODE_GRAVE, '`'.code)
        put(KeyEvent.KEYCODE_MINUS, '-'.code)
        put(KeyEvent.KEYCODE_EQUALS, '='.code)
        put(KeyEvent.KEYCODE_LEFT_BRACKET, '['.code)
        put(KeyEvent.KEYCODE_RIGHT_BRACKET, ']'.code)
        put(KeyEvent.KEYCODE_BACKSLASH, '\\'.code)
        put(KeyEvent.KEYCODE_SEMICOLON, ';'.code)
        put(KeyEvent.KEYCODE_APOSTROPHE, '\''.code)
        put(KeyEvent.KEYCODE_COMMA, ','.code)
        put(KeyEvent.KEYCODE_PERIOD, '.'.code)
        put(KeyEvent.KEYCODE_SLASH, '/'.code)
        put(KeyEvent.KEYCODE_ENTER, XK_RETURN)
        put(KeyEvent.KEYCODE_ESCAPE, XK_ESCAPE)
        put(KeyEvent.KEYCODE_TAB, XK_TAB)
        put(KeyEvent.KEYCODE_DEL, XK_BACKSPACE)
        put(KeyEvent.KEYCODE_FORWARD_DEL, XK_DELETE)
        put(KeyEvent.KEYCODE_INSERT, XK_INSERT)
        put(KeyEvent.KEYCODE_MOVE_HOME, XK_HOME)
        put(KeyEvent.KEYCODE_MOVE_END, XK_END)
        put(KeyEvent.KEYCODE_PAGE_UP, XK_PAGE_UP)
        put(KeyEvent.KEYCODE_PAGE_DOWN, XK_PAGE_DOWN)
        put(KeyEvent.KEYCODE_DPAD_LEFT, XK_LEFT)
        put(KeyEvent.KEYCODE_DPAD_UP, XK_UP)
        put(KeyEvent.KEYCODE_DPAD_RIGHT, XK_RIGHT)
        put(KeyEvent.KEYCODE_DPAD_DOWN, XK_DOWN)
        put(KeyEvent.KEYCODE_SHIFT_LEFT, XK_SHIFT_L)
        put(KeyEvent.KEYCODE_SHIFT_RIGHT, XK_SHIFT_R)
        put(KeyEvent.KEYCODE_CTRL_LEFT, XK_CONTROL_L)
        put(KeyEvent.KEYCODE_CTRL_RIGHT, XK_CONTROL_R)
        put(KeyEvent.KEYCODE_ALT_LEFT, XK_ALT_L)
        put(KeyEvent.KEYCODE_ALT_RIGHT, XK_ALT_R)
        put(KeyEvent.KEYCODE_META_LEFT, XK_SUPER_L)
        put(KeyEvent.KEYCODE_META_RIGHT, XK_SUPER_R)
        put(KeyEvent.KEYCODE_CAPS_LOCK, XK_CAPS_LOCK)
        put(KeyEvent.KEYCODE_NUM_LOCK, XK_NUM_LOCK)
        put(KeyEvent.KEYCODE_SCROLL_LOCK, XK_SCROLL_LOCK)
        put(KeyEvent.KEYCODE_BREAK, XK_PAUSE)
        put(KeyEvent.KEYCODE_SYSRQ, XK_PRINT)
        put(KeyEvent.KEYCODE_MENU, XK_MENU)
        put(KeyEvent.KEYCODE_NUMPAD_ENTER, XK_KP_ENTER)
        put(KeyEvent.KEYCODE_NUMPAD_ADD, XK_KP_ADD)
        put(KeyEvent.KEYCODE_NUMPAD_SUBTRACT, XK_KP_SUBTRACT)
        put(KeyEvent.KEYCODE_NUMPAD_MULTIPLY, XK_KP_MULTIPLY)
        put(KeyEvent.KEYCODE_NUMPAD_DIVIDE, XK_KP_DIVIDE)
        put(KeyEvent.KEYCODE_NUMPAD_DOT, XK_KP_DECIMAL)
    }

    /** The X keysym of a hardware keyboard key, or null for a key X has no use for here. */
    fun keysym(keyCode: Int): Int? = table[keyCode]

    /**
     * A Shift key of Android's keyboard ([KeyEvent.FLAG_SOFT_KEYBOARD] in [flags]), which stays out
     * of X: the keyboard's text comes with its capitals ([typeText]), and its Shift up in X would let
     * go of a Shift the player holds (an on-screen button). A hardware keyboard's Shift goes to X.
     */
    fun isSoftKeyboardShift(keyCode: Int, flags: Int): Boolean =
        (keyCode == KeyEvent.KEYCODE_SHIFT_LEFT || keyCode == KeyEvent.KEYCODE_SHIFT_RIGHT) &&
            flags and KeyEvent.FLAG_SOFT_KEYBOARD != 0

    /** The keysym that types [codePoint]: Latin-1 as itself, Return for a line break. */
    fun keysymOf(codePoint: Int): Int? = when {
        codePoint == '\n'.code || codePoint == '\r'.code -> XK_RETURN
        codePoint == '\t'.code -> XK_TAB
        codePoint in 0x20..0x7e || codePoint in 0xa0..0xff -> codePoint
        codePoint > 0xff && codePoint <= 0x10ffff -> UNICODE_KEYSYM or codePoint
        else -> null
    }

    /** Key presses and releases that type [text] (from the IME's `commitText`). */
    fun typeText(text: CharSequence): List<RelayEvent> {
        val events = ArrayList<RelayEvent>()
        val string = text.toString()
        var i = 0
        while (i < string.length) {
            val codePoint = string.codePointAt(i)
            i += Character.charCount(codePoint)
            val sym = keysymOf(codePoint) ?: continue
            events += RelayEvent.Key(sym, true)
            events += RelayEvent.Key(sym, false)
        }
        return events
    }

    /**
     * BackSpace taps for [before] characters and Delete taps for [after] (the IME's
     * `deleteSurroundingText`), each between 0 and [MAX_DELETE]: the lengths come from the
     * keyboard app, and a negative one would throw, a huge one fill the memory.
     */
    fun deleteText(before: Int, after: Int): List<RelayEvent> =
        List(before.coerceIn(0, MAX_DELETE)) { tapOf(XK_BACKSPACE) }.flatten() +
            List(after.coerceIn(0, MAX_DELETE)) { tapOf(XK_DELETE) }.flatten()

    private fun tapOf(sym: Int) = listOf(RelayEvent.Key(sym, true), RelayEvent.Key(sym, false))

    /** The most characters one IME deletion takes out (the chat line is shorter). */
    const val MAX_DELETE = 64
}
