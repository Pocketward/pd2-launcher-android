package com.pd2launcher.app.ui

import androidx.annotation.StringRes
import androidx.compose.runtime.Composable
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.Dp
import com.pd2launcher.app.R
import com.pd2launcher.app.input.Keys
import com.pd2launcher.app.input.PadAction
import com.pd2launcher.app.input.PadButton
import com.pd2launcher.app.input.RelayEvent

/** What a binding may be, in the picker's two groups: general (the game pad in pad mode only, nothing, the mouse), then Diablo II's keys. */
internal fun generalChoices(mouse: Boolean): List<PadAction> = buildList {
    if (!mouse) add(PadAction.Pad)
    add(PadAction.None)
    listOf(RelayEvent.LEFT, RelayEvent.RIGHT, 2, RelayEvent.WHEEL_UP, RelayEvent.WHEEL_DOWN).forEach { add(PadAction.Mouse(it)) }
}

internal val keyChoices: List<PadAction> get() = KEY_CHOICES.map { PadAction.Key(it.keysym) }

private class KeyChoice(val keysym: Int, val cap: String, @param:StringRes val name: Int)

/** Diablo II's default keys (Esc, Enter, Tab, Alt, Shift, Ctrl, Space, panels, belt, skills, arrows). */
private val KEY_CHOICES: List<KeyChoice> = buildList {
    add(KeyChoice(Keys.XK_ESCAPE, "Esc", R.string.controls_key_escape))
    add(KeyChoice(Keys.XK_RETURN, "Enter", R.string.controls_key_enter))
    add(KeyChoice(Keys.XK_TAB, "Tab", R.string.controls_key_tab))
    add(KeyChoice(Keys.XK_ALT_L, "Alt", R.string.controls_key_alt))
    add(KeyChoice(Keys.XK_SHIFT_L, "Shift", R.string.controls_key_shift))
    add(KeyChoice(Keys.XK_CONTROL_L, "Ctrl", R.string.controls_key_ctrl))
    add(KeyChoice(' '.code, "Space", R.string.controls_key_space))
    add(KeyChoice('i'.code, "I", R.string.controls_key_inventory))
    add(KeyChoice('c'.code, "C", R.string.controls_key_character))
    add(KeyChoice('t'.code, "T", R.string.controls_key_skill_tree))
    add(KeyChoice('q'.code, "Q", R.string.controls_key_quests))
    add(KeyChoice('o'.code, "O", R.string.controls_key_hireling))
    add(KeyChoice('p'.code, "P", R.string.controls_key_party))
    add(KeyChoice('m'.code, "M", R.string.controls_key_messages))
    add(KeyChoice('h'.code, "H", R.string.controls_key_help))
    add(KeyChoice('r'.code, "R", R.string.controls_key_run))
    add(KeyChoice('w'.code, "W", R.string.controls_key_swap))
    add(KeyChoice('z'.code, "Z", R.string.controls_key_portraits))
    add(KeyChoice('`'.code, "~", R.string.controls_key_belt))
    for (i in 1..4) add(KeyChoice('0'.code + i, "$i", R.string.controls_key_belt_slot))
    for (i in 1..8) add(KeyChoice(Keys.XK_F1 + i - 1, "F$i", R.string.controls_key_skill))
    add(KeyChoice(Keys.XK_UP, "↑", R.string.controls_key_arrow))
    add(KeyChoice(Keys.XK_DOWN, "↓", R.string.controls_key_arrow))
    add(KeyChoice(Keys.XK_LEFT, "←", R.string.controls_key_arrow))
    add(KeyChoice(Keys.XK_RIGHT, "→", R.string.controls_key_arrow))
}

/**
 * Every other key a custom on-screen button may type, through Any key: the letters and digits
 * Diablo II leaves free, F9–F12, the editing keys and the punctuation keys.
 */
private val OTHER_KEYS: List<KeyChoice> = buildList {
    val named = KEY_CHOICES.map { it.keysym }.toSet()
    for (c in 'a'..'z') if (c.code !in named) add(KeyChoice(c.code, c.uppercase(), R.string.controls_key_letter))
    for (c in '0'..'9') if (c.code !in named) add(KeyChoice(c.code, "$c", R.string.controls_key_number))
    for (i in 9..12) add(KeyChoice(Keys.XK_F1 + i - 1, "F$i", R.string.controls_key_function))
    add(KeyChoice(Keys.XK_BACKSPACE, "⌫", R.string.controls_key_backspace))
    add(KeyChoice(Keys.XK_DELETE, "Del", R.string.controls_key_delete))
    add(KeyChoice(Keys.XK_INSERT, "Ins", R.string.controls_key_insert))
    add(KeyChoice(Keys.XK_HOME, "Home", R.string.controls_key_home))
    add(KeyChoice(Keys.XK_END, "End", R.string.controls_key_end))
    add(KeyChoice(Keys.XK_PAGE_UP, "PgUp", R.string.controls_key_page_up))
    add(KeyChoice(Keys.XK_PAGE_DOWN, "PgDn", R.string.controls_key_page_down))
    "-=[];',./\\".forEach { add(KeyChoice(it.code, "$it", R.string.controls_key_symbol)) }
}

private val ALL_KEYS = KEY_CHOICES + OTHER_KEYS

/** The clicks a custom on-screen button may make: left, right and middle. */
internal val customClicks: List<PadAction> get() = listOf(RelayEvent.LEFT, RelayEvent.RIGHT, 2).map { PadAction.Mouse(it) }

/** Whether [keysym] is a key the app names (a cap and what it does): any key a custom button may send. */
internal fun namedKey(keysym: Int): Boolean = ALL_KEYS.any { it.keysym == keysym }

/**
 * The key a hardware keyboard's [keyCode] binds a custom button to: a right-hand Shift, Ctrl or
 * Alt as its left-hand twin; null for a key the app has no name for (the keypad, media keys).
 */
internal fun pickedKeysym(keyCode: Int): Int? = when (val sym = Keys.keysym(keyCode)) {
    Keys.XK_SHIFT_R -> Keys.XK_SHIFT_L
    Keys.XK_CONTROL_R -> Keys.XK_CONTROL_L
    Keys.XK_ALT_R -> Keys.XK_ALT_L
    else -> sym?.takeIf(::namedKey)
}

/** The key cap a key binding shows; null for the pad, nothing and the mouse. */
internal fun keyCap(action: PadAction): String? =
    (action as? PadAction.Key)?.let { key -> ALL_KEYS.firstOrNull { it.keysym == key.keysym }?.cap ?: "0x%x".format(key.keysym) }

/**
 * The cap a choice list shows beside a binding's name: a key's cap, or L/R/M for a click (what a
 * custom button bound to it shows); null for the pad, nothing and the wheel.
 */
internal fun choiceCap(action: PadAction): String? = keyCap(action)
    ?: (action as? PadAction.Mouse)?.takeIf { it.button != RelayEvent.WHEEL_UP && it.button != RelayEvent.WHEEL_DOWN }?.let(::defaultLabel)

/** A custom button's label when the player gave none: its key cap, or L/R/M for a click. */
internal fun defaultLabel(action: PadAction): String = keyCap(action) ?: when ((action as? PadAction.Mouse)?.button) {
    RelayEvent.LEFT -> "L"
    RelayEvent.RIGHT -> "R"
    else -> "M"
}

/** A controller button's cap: its name, the d-pad as arrows. */
internal fun buttonCap(button: PadButton): String = when (button) {
    PadButton.UP -> "↑"
    PadButton.DOWN -> "↓"
    PadButton.LEFT -> "←"
    PadButton.RIGHT -> "→"
    else -> button.label
}

/** What a binding does, in words (a key's game meaning, without its cap). */
@Composable
internal fun actionName(action: PadAction): String = when (action) {
    PadAction.Pad -> stringResource(R.string.controls_action_pad)
    PadAction.None -> stringResource(R.string.controls_action_none)
    is PadAction.Mouse -> stringResource(
        when (action.button) {
            RelayEvent.LEFT -> R.string.controls_action_left_click
            RelayEvent.RIGHT -> R.string.controls_action_right_click
            RelayEvent.WHEEL_UP -> R.string.controls_action_wheel_up
            RelayEvent.WHEEL_DOWN -> R.string.controls_action_wheel_down
            else -> R.string.controls_action_middle_click
        },
    )
    is PadAction.Key -> ALL_KEYS.firstOrNull { it.keysym == action.keysym }?.let { stringResource(it.name) }
        ?: stringResource(R.string.controls_action_key)
}

/** The arrow keys' own names in a line: one word says which (the picker groups them as "Arrow"). */
private val ARROW_LINES = mapOf(
    Keys.XK_UP to R.string.controls_key_arrow_up,
    Keys.XK_DOWN to R.string.controls_key_arrow_down,
    Keys.XK_LEFT to R.string.controls_key_arrow_left,
    Keys.XK_RIGHT to R.string.controls_key_arrow_right,
)

/** A binding as one line: "Alt · Show items", "Left click", "Up arrow". */
@Composable
internal fun actionLine(action: PadAction): String {
    ARROW_LINES[(action as? PadAction.Key)?.keysym]?.let { return stringResource(it) }
    return keyCap(action)?.let { "$it · ${actionName(action)}" } ?: actionName(action)
}

/**
 * Where a controller button sits, as the in-game Controls screen groups the bindings: the face
 * buttons, the shoulders and triggers (the first column where there are two), then the d-pad (its
 * four directions, each a binding of its own), the stick buttons, and Start and Select.
 */
internal enum class Cluster(@param:StringRes val label: Int, val buttons: List<PadButton>, val column: Int) {
    FACE(R.string.controls_group_face, listOf(PadButton.A, PadButton.B, PadButton.X, PadButton.Y), 0),
    SHOULDERS(R.string.controls_group_shoulders, listOf(PadButton.L1, PadButton.R1, PadButton.L2, PadButton.R2), 0),
    DPAD(R.string.controls_group_dpad, listOf(PadButton.UP, PadButton.DOWN, PadButton.LEFT, PadButton.RIGHT), 1),
    STICKS(R.string.controls_group_sticks, listOf(PadButton.L3, PadButton.R3), 1),
    MENU(R.string.controls_group_menu, listOf(PadButton.START, PadButton.SELECT), 1),
}

/** The most columns of bindings the Controls screen uses. */
internal const val MOST_BINDING_COLUMNS = 3

/**
 * The binding groups in [columns] (1 to 3), in the same order: three hold the face buttons, the
 * shoulders and triggers, and the rest; two the face buttons with the shoulders, then the rest;
 * one all of them.
 */
internal fun clusterColumns(columns: Int): List<List<Cluster>> = when {
    columns >= MOST_BINDING_COLUMNS -> listOf(listOf(Cluster.FACE), listOf(Cluster.SHOULDERS), listOf(Cluster.DPAD, Cluster.STICKS, Cluster.MENU))
    columns == 2 -> Cluster.entries.groupBy { it.column }.values.toList()
    else -> listOf(Cluster.entries)
}

/**
 * How many columns of bindings fit [width]: the most (up to [MOST_BINDING_COLUMNS]) that each get
 * [column], the room a binding needs for its longest word, with [gap] between them; at least one.
 */
internal fun bindingColumns(width: Dp, column: Dp, gap: Dp): Int =
    (MOST_BINDING_COLUMNS downTo 2).firstOrNull { column * it + gap * (it - 1) <= width } ?: 1
