package com.pd2launcher.app.ui

import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.foundation.LocalIndication
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.interaction.collectIsFocusedAsState
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxScope
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.IntrinsicSize
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawing
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.layout.Layout
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.rememberTextMeasurer
import androidx.compose.ui.unit.Constraints
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import com.pd2launcher.app.R
import com.pd2launcher.app.input.Chord
import com.pd2launcher.app.input.Controls
import com.pd2launcher.app.input.PadButton
import kotlin.math.roundToInt

/** The in-game Controls screen's sizes, in dp. */
internal object ControlsScreenSize {
    /** The left pane's least width; a word that needs more widens it. */
    val NAV_WIDTH = 200.dp
    /** Between two columns of bindings. */
    val COLUMN_GAP = 12.dp
    /** A binding row around its words: the start, the cap's least width, two gaps, the chevron and the end. */
    val ROW_START = 6.dp
    val CAP_MIN = 44.dp
    val ROW_GAP = 10.dp
    val CHEVRON = 20.dp
    val ROW_END = 9.dp
    /** The room a cap needs around its name. */
    val CAP_PADDING = 16.dp
    /** How far above its foot the bindings fade out while more lie below. */
    val FADE = 40.dp
    /** The mode's words under the left pane's rows: their side room, the room above them and between the two. */
    val TEXT_SIDE = 14.dp
    val TEXT_TOP = 16.dp
    val TEXT_GAP = 8.dp
    /** The left and the right pane's rule with its room either side. */
    val RULE_START = 8.dp
    val RULE_END = 4.dp
}

/** The right pane's header beside the viewed mode's name: it is the mode in use, or a button makes it so. */
internal enum class ModeHeader {
    IN_USE,
    USE_PAD,
    USE_MOUSE,
    ;

    companion object {
        /** The header for the [viewedMouse] mode while the controller is in mouse mode ([activeMouse]) or not. */
        fun of(viewedMouse: Boolean, activeMouse: Boolean): ModeHeader = when {
            viewedMouse == activeMouse -> IN_USE
            viewedMouse -> USE_MOUSE
            else -> USE_PAD
        }
    }
}

/**
 * Whether the mode's words and the chord's hint go under the left pane's rows: where [rows], the
 * [gap] above the words and the words ([texts]) together fit the pane's height ([room]), all in
 * px; else they go at the top of the right pane, under its header (a phone on its side). One
 * place to change it.
 */
internal fun modeTextsFitLeft(rows: Int, texts: Int, gap: Int, room: Int): Boolean = rows + gap + texts <= room

/**
 * The in-game Controls screen, over the live game, about the physical controller: the whole
 * screen inside the menu's margin, with the menu's strip ([TabStrip]) on its right edge where the
 * menu shows it ([menuHeight]: the menu's panel's height in px; else at the top), the gamepad
 * selected. The strip's ✕ goes back to the game ([onClose]), its Quick and Settings to those tabs
 * ([onStop]; L1 and R1 too, the Game view), Back to the menu. Left:
 * the two modes to view and change, the one the controller is in ([activeMouse]) with an "In use"
 * badge; Pointer speed (its input, [PointerSpeedInput]); Edit on-screen layout
 * ([onEditOnScreen]); then what the viewed mode does and how the chord switches it, where the
 * pane has the height ([modeTextsFitLeft]). Right: the viewed mode's name and "In use", or a
 * button that switches the controller to it at once ([onUseMode], as the chord does); then every
 * button by where it sits ([Cluster]; the d-pad's four directions one by one), in as many columns
 * (one to three) as fit with no word cut, scrolling where they need more height. Each row is the
 * button's cap and what it does, changed ones in the text colour; a press opens the binding
 * picker ([BindingPicker]) and the change applies in the game at once ([save]). It opens on the
 * mode in use. [canUseMode]: the game takes the controller now (else the Use button waits).
 * [onPopup]: the action picker or the Pointer speed input shows (the shoulder buttons do not step).
 */
@Composable
internal fun BoxScope.ControlsScreen(
    visible: Boolean,
    controls: Controls,
    activeMouse: Boolean,
    canUseMode: Boolean,
    save: (Controls) -> Unit,
    onUseMode: (mouse: Boolean) -> Unit,
    menuHeight: Int?,
    onStop: (MenuStop) -> Unit,
    onClose: () -> Unit,
    onPopup: (Boolean) -> Unit,
    onEditOnScreen: () -> Unit,
) {
    AnimatedVisibility(visible, Modifier.matchParentSize(), enter = fadeIn(), exit = fadeOut()) {
        var mouse by remember { mutableStateOf(activeMouse) }
        var picker by remember { mutableStateOf<PadButton?>(null) }
        var speedInput by remember { mutableStateOf(false) }
        val popup by rememberUpdatedState(onPopup)
        LaunchedEffect(picker, speedInput) { popup(picker != null || speedInput) }
        DisposableEffect(Unit) { onDispose { popup(false) } }
        val first = remember { FocusRequester() }
        InitialFocus(first)
        // The picker closed: the focus goes back to the row that opened it.
        val rows = remember { PadButton.entries.associateWith { FocusRequester() } }
        var picked by remember { mutableStateOf<PadButton?>(null) }
        LaunchedEffect(picker) {
            if (picker == null) picked?.let { runCatching { rows.getValue(it).requestFocus() } }
        }
        // The Use button goes as its mode comes into use: the focus goes to the first binding.
        var switched by remember { mutableIntStateOf(0) }
        LaunchedEffect(switched) {
            if (switched > 0) runCatching { rows.getValue(PadButton.A).requestFocus() }
        }
        Box(Modifier.fillMaxSize()) {
            // The game shows dimmed around the screen, and takes no touch from here.
            Box(Modifier.fillMaxSize().background(Color.Black.copy(alpha = 0.55f)).takesTouches())
            // As the menu's panel: no padding on the strip's side, so the strip sits where the menu has it.
            Panel(
                Modifier.fillMaxSize().windowInsetsPadding(WindowInsets.safeDrawing).padding(MenuSize.INSET).takesTouches().testTag(CONTROLS_TAG),
                padding = 0.dp,
            ) {
                Row(Modifier.fillMaxSize()) {
                    Column(Modifier.weight(1f).fillMaxHeight().padding(start = 12.dp, top = 4.dp, bottom = 12.dp)) {
                        Box(Modifier.fillMaxWidth().heightIn(min = MenuSize.ROW), contentAlignment = Alignment.CenterStart) {
                            ScreenTitle(stringResource(R.string.controls_title), Modifier.padding(start = 4.dp))
                        }
                        ControlsBody(
                            controls, mouse, activeMouse, canUseMode, rows,
                            onView = { mouse = it },
                            onUseMode = {
                                onUseMode(it)
                                switched++
                            },
                            first = first,
                            onPointerSpeed = { speedInput = true },
                            onEditOnScreen = onEditOnScreen,
                        ) {
                            picked = it
                            picker = it
                        }
                    }
                    Rule(Modifier.padding(start = 8.dp, end = 4.dp, top = MenuSize.PADDING, bottom = MenuSize.PADDING).fillMaxHeight(), vertical = true)
                    StripAt(menuHeight) { TabStrip(MenuStop.CONTROLS, controls = true, onStop = onStop, onClose = onClose) }
                }
            }
            picker?.let { button -> BindingPicker(controls, button, mouse, side = true, save) { picker = null } }
        }
        if (speedInput) {
            PointerSpeedInput(controls.pointerSpeed, { save(controls.copy(pointerSpeed = it)) }) { speedInput = false }
        }
    }
}

/**
 * The two panes: the left one as wide as its words need, at least [ControlsScreenSize.NAV_WIDTH];
 * the mode's words under its rows where they fit its height ([modeTextsFitLeft], measured as it
 * is laid out), else under the right pane's header.
 */
@Composable
private fun ColumnScope.ControlsBody(
    controls: Controls,
    mouse: Boolean,
    activeMouse: Boolean,
    canUseMode: Boolean,
    rows: Map<PadButton, FocusRequester>,
    onView: (Boolean) -> Unit,
    onUseMode: (Boolean) -> Unit,
    first: FocusRequester,
    onPointerSpeed: () -> Unit,
    onEditOnScreen: () -> Unit,
    onPick: (PadButton) -> Unit,
) {
    val modeText = stringResource(if (mouse) R.string.controls_mouse_mode_body else R.string.controls_pad_mode_body)
    // The other mode's words count in the fit too, so they stay in one place whichever mode shows.
    val otherText = stringResource(if (mouse) R.string.controls_pad_mode_body else R.string.controls_mouse_mode_body)
    val chordText = stringResource(
        R.string.controls_panel_chord,
        stringResource(if (controls.chord == Chord.SELECT_START) R.string.controls_chord_select_start else R.string.controls_chord_thumbs),
    )
    // Unknown until the left pane is first laid out: the right pane waits with the words.
    var textsLeft by remember { mutableStateOf<Boolean?>(null) }
    Row(Modifier.weight(1f).fillMaxWidth().padding(top = 4.dp)) {
        LeftPane(modeText, otherText, chordText, textsLeft != false, { if (textsLeft != it) textsLeft = it }) {
            ModesPane(controls, mouse, activeMouse, onView, first, onPointerSpeed, onEditOnScreen)
        }
        Rule(Modifier.padding(start = ControlsScreenSize.RULE_START, end = ControlsScreenSize.RULE_END).fillMaxHeight(), vertical = true)
        Box(Modifier.weight(1f).fillMaxHeight()) {
            BindingsPane(controls, mouse, activeMouse, canUseMode, rows, if (textsLeft == false) modeText to chordText else null, onUseMode, onPick)
        }
    }
}

/**
 * The left pane: its [rows], as wide as their words need and at least
 * [ControlsScreenSize.NAV_WIDTH], then the mode's words under them where the pane's height holds
 * both ([modeTextsFitLeft]; the taller of the two modes' words counts, [other]); [onFit] hears
 * whether it does. [shown]: the words are the left pane's (else they only take part in the
 * measure). It scrolls where the rows alone are too tall.
 */
@Composable
private fun LeftPane(mode: String, other: String, chord: String, shown: Boolean, onFit: (Boolean) -> Unit, rows: @Composable () -> Unit) {
    BoxWithConstraints(Modifier.fillMaxHeight()) {
        val room = constraints.maxHeight
        val density = LocalDensity.current
        val least = with(density) { ControlsScreenSize.NAV_WIDTH.roundToPx() }
        val gap = with(density) { ControlsScreenSize.TEXT_TOP.roundToPx() }
        val fit by rememberUpdatedState(onFit)
        Layout(
            content = {
                Box { rows() }
                ModeTexts(mode, chord, Modifier.padding(horizontal = ControlsScreenSize.TEXT_SIDE).then(if (shown) Modifier else Modifier.unseen()))
                // Only measured, never placed.
                ModeTexts(other, chord, Modifier.padding(horizontal = ControlsScreenSize.TEXT_SIDE).unseen())
            },
            modifier = Modifier.verticalScroll(rememberScrollState()),
        ) { measurables, constraints ->
            val (rowsMeasurable, textsMeasurable, otherMeasurable) = measurables
            val width = maxOf(least, rowsMeasurable.minIntrinsicWidth(Constraints.Infinity)).coerceAtMost(constraints.maxWidth)
            val rowsPlaced = rowsMeasurable.measure(Constraints.fixedWidth(width))
            val textsPlaced = textsMeasurable.measure(Constraints.fixedWidth(width))
            val otherHeight = otherMeasurable.measure(Constraints.fixedWidth(width)).height
            val fits = modeTextsFitLeft(rowsPlaced.height, maxOf(textsPlaced.height, otherHeight), gap, room)
            fit(fits)
            layout(width, rowsPlaced.height + if (fits) gap + textsPlaced.height else 0) {
                rowsPlaced.place(0, 0)
                if (fits) textsPlaced.place(0, rowsPlaced.height + gap)
            }
        }
    }
}

/**
 * The menu's strip beside the Controls screen, where the menu shows it: the menu's panel (its
 * height [menuHeight], px) sits centred in the room this screen's panel fills, and the strip at
 * its top, so the strip is half the difference down; at the top while that is not known. It keeps
 * inside the pane's height.
 */
@Composable
private fun StripAt(menuHeight: Int?, strip: @Composable () -> Unit) {
    Layout(strip, Modifier.fillMaxHeight()) { measurables, constraints ->
        val placed = measurables.first().measure(constraints.copy(minHeight = 0))
        val height = if (constraints.hasBoundedHeight) constraints.maxHeight else placed.height
        // Rounded as Compose centres the menu's panel.
        val y = menuHeight?.let { ((height - it) / 2f).roundToInt() } ?: 0
        layout(placed.width, height) { placed.place(0, y.coerceIn(0, (height - placed.height).coerceAtLeast(0))) }
    }
}

private val PillShape = RoundedCornerShape(percent = 50)

/** The left pane's rows: the two modes, Pointer speed and Edit on-screen layout. */
@Composable
private fun ModesPane(
    controls: Controls,
    mouse: Boolean,
    activeMouse: Boolean,
    onMode: (Boolean) -> Unit,
    first: FocusRequester,
    onPointerSpeed: () -> Unit,
    onEditOnScreen: () -> Unit,
) {
    Column {
        PaneLabel(stringResource(R.string.controls_section_buttons), top = 8.dp)
        ModeItem(stringResource(R.string.controls_pad_mode), !mouse, !activeMouse, if (!mouse) first else null) { onMode(false) }
        ModeItem(stringResource(R.string.controls_mouse_mode), mouse, activeMouse, if (mouse) first else null) { onMode(true) }
        Rule(Modifier.padding(vertical = 6.dp))
        LinkRow(stringResource(R.string.controls_pointer_speed_title), stringResource(R.string.controls_factor, speedText(controls.pointerSpeed)), onPointerSpeed)
        LinkRow(stringResource(R.string.controls_edit_layout), null, onEditOnScreen)
    }
}

/** What the viewed mode does, then how the chord switches modes: quiet, small. */
@Composable
private fun ModeTexts(mode: String, chord: String, modifier: Modifier = Modifier) {
    Column(modifier, verticalArrangement = Arrangement.spacedBy(ControlsScreenSize.TEXT_GAP)) {
        Text(mode, style = Type.Hint, color = Palette.Muted)
        Text(chord, style = Type.Hint, color = Palette.Muted)
    }
}

/** A teal overline over a group, closer to its rows than [GroupLabel]: the screen is dense. */
@Composable
private fun PaneLabel(text: String, top: Dp) {
    Text(
        text.uppercase(), style = Type.Label, color = Palette.Teal,
        modifier = Modifier.padding(start = 4.dp, end = 4.dp, top = top, bottom = 4.dp).semantics { heading() },
    )
}

/** "In use": the mode the controller is in, a filled teal badge. */
@Composable
private fun InUseBadge() {
    Box(Modifier.clip(PillShape).background(Palette.Teal).padding(horizontal = 10.dp, vertical = 3.dp), contentAlignment = Alignment.Center) {
        Text(
            stringResource(R.string.controls_in_use), style = Type.Hint.copy(fontWeight = FontWeight.SemiBold), color = Palette.OnTeal,
            maxLines = 1, softWrap = false, modifier = Modifier.width(IntrinsicSize.Max),
        )
    }
}

/** A mode to view and change: the viewed one tinted, in teal; the one in use ([inUse]) with its badge. */
@Composable
private fun ModeItem(label: String, selected: Boolean, inUse: Boolean, focus: FocusRequester?, onClick: () -> Unit) {
    val interaction = remember { MutableInteractionSource() }
    val focused by interaction.collectIsFocusedAsState()
    Row(
        Modifier.focusOn(focus).fillMaxWidth().heightIn(min = MenuSize.ROW).focusMark(focused, selected)
            .selectable(selected, interaction, LocalIndication.current, role = Role.Tab, onClick = onClick)
            .padding(start = 14.dp, end = 10.dp, top = 4.dp, bottom = 4.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        // One line beside the badge: the pane widens for the whole label at a large font.
        Text(
            label, style = Type.Body.copy(fontWeight = if (selected) FontWeight.SemiBold else FontWeight.Medium),
            color = if (selected) Palette.Teal else Palette.Text, softWrap = false,
            modifier = Modifier.weight(1f).width(IntrinsicSize.Max),
        )
        if (inUse) InUseBadge()
    }
}

/**
 * The right pane: the viewed mode's name with "In use" or the button that switches to it, the
 * mode's words where the left pane has no room for them ([texts]), then the bindings in columns;
 * all under the header scrolls.
 */
@Composable
private fun BindingsPane(
    controls: Controls,
    mouse: Boolean,
    activeMouse: Boolean,
    canUseMode: Boolean,
    rows: Map<PadButton, FocusRequester>,
    texts: Pair<String, String>?,
    onUseMode: (Boolean) -> Unit,
    onPick: (PadButton) -> Unit,
) {
    Column(Modifier.fillMaxSize()) {
        Row(
            Modifier.fillMaxWidth().heightIn(min = MenuSize.ROW).padding(start = 4.dp),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Text(
                stringResource(if (mouse) R.string.controls_mouse_mode else R.string.controls_pad_mode),
                style = Type.Button, color = Palette.Text, modifier = Modifier.weight(1f).semantics { heading() },
            )
            when (ModeHeader.of(mouse, activeMouse)) {
                // Said as it comes, after a Use press.
                ModeHeader.IN_USE -> Box(Modifier.semantics { liveRegion = LiveRegionMode.Polite }) { InUseBadge() }
                ModeHeader.USE_MOUSE -> if (canUseMode) {
                    LauncherButton(stringResource(R.string.controls_use_mouse), { onUseMode(true) }, kind = Kind.PRIMARY, height = MenuSize.ROW)
                }
                ModeHeader.USE_PAD -> if (canUseMode) {
                    LauncherButton(stringResource(R.string.controls_use_pad), { onUseMode(false) }, kind = Kind.PRIMARY, height = MenuSize.ROW)
                }
            }
        }
        val scroll = rememberScrollState()
        Box(Modifier.weight(1f).fillMaxWidth()) {
            BoxWithConstraints(Modifier.fillMaxSize().verticalScroll(scroll).padding(bottom = 12.dp)) {
                val lines = PadButton.entries.associateWith { actionLine(controls.action(it, mouse)) }
                val column = bindingColumnWidth(PadButton.entries.map(lines::getValue), PadButton.entries.map(::buttonCap))
                val columns = bindingColumns(maxWidth, column, ControlsScreenSize.COLUMN_GAP)
                Column(Modifier.fillMaxWidth()) {
                    if (texts != null) {
                        ModeTexts(texts.first, texts.second, Modifier.padding(start = 4.dp, end = 4.dp, top = 6.dp, bottom = 4.dp))
                    } else {
                        Spacer(Modifier.height(4.dp))
                    }
                    Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(ControlsScreenSize.COLUMN_GAP)) {
                        clusterColumns(columns).forEach { clusters ->
                            Column(Modifier.weight(1f)) {
                                clusters.forEach { cluster ->
                                    PaneLabel(stringResource(cluster.label), top = 10.dp)
                                    cluster.buttons.forEach { button ->
                                        BindingRow(button, lines.getValue(button), controls.isDefault(button, mouse), rows.getValue(button)) { onPick(button) }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            // More below: the rows fade out at the foot rather than end cut through.
            if (scroll.canScrollForward) {
                Box(
                    Modifier.align(Alignment.BottomCenter).fillMaxWidth().height(ControlsScreenSize.FADE)
                        .background(Brush.verticalGradient(listOf(Palette.Surface.copy(alpha = 0f), Palette.Surface))),
                )
            }
        }
    }
}

/**
 * The least width of a column of bindings: a row's own room (its start, the widest of the [caps],
 * the gaps, the chevron, its end) and the longest word of the [lines], so no word is ever cut.
 */
@Composable
private fun bindingColumnWidth(lines: List<String>, caps: List<String>): Dp {
    val measurer = rememberTextMeasurer()
    val density = LocalDensity.current
    return remember(lines, caps, measurer, density) {
        val words = lines.flatMap { it.split(' ') }.filter { it.isNotEmpty() }.distinct()
        with(density) {
            val word = words.maxOfOrNull { measurer.measure(it, Type.Hint, maxLines = 1, softWrap = false).size.width } ?: 0
            val cap = caps.maxOf { measurer.measure(it, Type.Glyph, maxLines = 1, softWrap = false).size.width }
            val capWidth = maxOf(ControlsScreenSize.CAP_MIN, cap.toDp() + ControlsScreenSize.CAP_PADDING)
            ControlsScreenSize.ROW_START + capWidth + ControlsScreenSize.ROW_GAP * 2 + word.toDp() + ControlsScreenSize.CHEVRON + ControlsScreenSize.ROW_END
        }
    }
}

/** One binding: the button's cap, what it does (a default muted, a change in the text colour), a chevron; a press opens the picker. */
@Composable
private fun BindingRow(button: PadButton, line: String, isDefault: Boolean, focus: FocusRequester, onClick: () -> Unit) {
    val interaction = remember { MutableInteractionSource() }
    val focused by interaction.collectIsFocusedAsState()
    Row(
        Modifier.focusOn(focus).fillMaxWidth().heightIn(min = MenuSize.ROW).focusMark(focused)
            .clickable(interaction, LocalIndication.current, role = Role.Button, onClick = onClick)
            .padding(start = ControlsScreenSize.ROW_START, end = ControlsScreenSize.ROW_END, top = 4.dp, bottom = 4.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(ControlsScreenSize.ROW_GAP),
    ) {
        KeyCap(buttonCap(button), Modifier.widthIn(min = ControlsScreenSize.CAP_MIN))
        Text(line, style = Type.Hint, color = if (isDefault) Palette.Muted else Palette.Text, modifier = Modifier.weight(1f))
        Icon(painterResource(R.drawable.ic_chevron_right), null, Modifier.size(ControlsScreenSize.CHEVRON), tint = Palette.Muted)
    }
}
