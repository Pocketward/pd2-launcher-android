package com.pd2launcher.app.ui

import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.core.RepeatMode
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.animateFloatAsState
import androidx.compose.animation.core.infiniteRepeatable
import androidx.compose.animation.core.rememberInfiniteTransition
import androidx.compose.animation.core.tween
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.slideInHorizontally
import androidx.compose.animation.slideOutHorizontally
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.LocalIndication
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.LocalBringIntoViewSpec
import androidx.compose.foundation.gestures.awaitEachGesture
import androidx.compose.foundation.gestures.awaitFirstDown
import androidx.compose.foundation.gestures.detectTapGestures
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
import androidx.compose.foundation.selection.toggleable
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.draw.clip
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import com.pd2launcher.app.FpsReading
import com.pd2launcher.app.R
import com.pd2launcher.app.input.Chord
import com.pd2launcher.app.input.Controls
import kotlinx.coroutines.delay
import kotlin.math.roundToInt

/** The slim menu glyph over the game: clear at first, then a quiet 30 %. */
@Composable
internal fun MenuGlyph(modifier: Modifier, onClick: () -> Unit) {
    val label = stringResource(R.string.game_view_menu)
    var faded by remember { mutableStateOf(false) }
    LaunchedEffect(Unit) {
        delay(2500)
        faded = true
    }
    val a by animateFloatAsState(if (faded) 0.3f else 0.95f, tween(900), label = "glyph")
    Box(
        modifier.padding(6.dp).size(48.dp).alpha(a).clip(CircleShape)
            .clickable(role = Role.Button, onClick = onClick)
            .semantics { contentDescription = label },
        contentAlignment = Alignment.Center,
    ) {
        Canvas(Modifier.size(22.dp)) {
            listOf(0.22f, 0.5f, 0.78f).forEach { y ->
                drawLine(Palette.Text, Offset(size.width * 0.12f, size.height * y), Offset(size.width * 0.88f, size.height * y), 2f * density, StrokeCap.Square)
            }
        }
    }
}

/** A live dot and "Game running": the menu never pretends the game has stopped. */
@Composable
private fun Live(launcher: Boolean = false) {
    val t = rememberInfiniteTransition(label = "live")
    val a by t.animateFloat(0.4f, 1f, infiniteRepeatable(tween(900), RepeatMode.Reverse), label = "a")
    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(6.dp)) {
        Box(Modifier.size(8.dp).clip(CircleShape).background(Palette.Teal.copy(alpha = a)))
        Text(stringResource(Play.overlayStatus(launcher)), style = Type.Hint, color = Palette.Muted, maxLines = 1, overflow = TextOverflow.Ellipsis)
    }
}

/**
 * "42 fps": the game's own frame rate; "73 fps · 60 on screen" when the screen shows fewer of its
 * frames (its refresh rate is lower); "– fps" before the first second is measured.
 */
@Composable
private fun fpsText(fps: FpsReading?): String = when {
    fps == null -> stringResource(R.string.game_fps_unknown)
    fps.onScreen != null -> stringResource(R.string.game_fps_on_screen, fps.fps.roundToInt(), fps.onScreen.roundToInt())
    else -> stringResource(R.string.game_fps, fps.fps.roundToInt())
}

/** The fps counter over the game: small and quiet in a corner; it takes no input. */
@Composable
internal fun FpsCounter(fps: FpsReading?, modifier: Modifier) {
    Text(
        fpsText(fps), style = Type.Hint, color = Palette.Text, maxLines = 1,
        modifier = modifier.padding(6.dp).alpha(0.6f).clip(RowShape).background(Palette.Background.copy(alpha = 0.5f))
            .padding(horizontal = 6.dp, vertical = 2.dp),
    )
}

/**
 * Over the game beside the rail ([GameLayer.tapCloses]), under the on-screen controls and the
 * rail: a tap on what they leave of the screen closes the rail and never reaches the game.
 */
@Composable
internal fun TapToClose(onClose: () -> Unit) {
    val close by rememberUpdatedState(onClose)
    Box(Modifier.fillMaxSize().pointerInput(Unit) { detectTapGestures(onTap = { close() }) })
}

/** What the in-game rail offers: quick toggles, then the pages it opens. Stop follows, apart. */
internal enum class RailItem { KEYBOARD, TOUCH_PAD, SHOW_FPS, CONTROLS, EDIT_CONTROLS, SETTINGS }

/**
 * The rail's two groups, in order. Over the game: Keyboard, Touch pad and Show fps, then
 * Controls, Edit controls and Settings. Over the official PD2 [launcher], a mouse/touch window
 * with no controller to set up and no game fps to show: Keyboard, then Settings. [settings]: the
 * app offers its settings here.
 */
internal data class RailRows(val toggles: List<RailItem>, val links: List<RailItem>) {
    companion object {
        fun of(launcher: Boolean, settings: Boolean): RailRows {
            val tail = if (settings) listOf(RailItem.SETTINGS) else emptyList()
            return if (launcher) {
                RailRows(listOf(RailItem.KEYBOARD), tail)
            } else {
                RailRows(listOf(RailItem.KEYBOARD, RailItem.TOUCH_PAD, RailItem.SHOW_FPS), listOf(RailItem.CONTROLS, RailItem.EDIT_CONTROLS) + tail)
            }
        }
    }
}

/**
 * The rail's shape in the room it has: one column where that fits the height, else the toggles
 * and the pages side by side (a phone on its side, about 360 dp tall), scrolling only when even
 * that does not fit. Its width follows its labels, at most [maxWidth]: a share of the width (a
 * larger one for two columns), never under [MIN_WIDTH].
 */
internal data class RailSize(val columns: Int, val maxWidth: Dp, val scrolls: Boolean) {
    companion object {
        /** Every row and button: a 48 dp target. */
        val ROW = 48.dp
        /** From the screen's edge to the rail. */
        val INSET = 8.dp
        /** Inside the rail, around its rows. */
        val PADDING = 8.dp
        /** Between the header and the first group. */
        val HEAD_GAP = 4.dp
        /** Between the two groups: a hairline with 6 dp either side. */
        val GROUP_GAP = 13.dp
        /** The least air above Stop; the body keeps it and Stop below its end, inside the scroll. */
        val STOP_GAP = 12.dp
        val MIN_WIDTH = 168.dp
        const val ONE_COLUMN_SHARE = 0.34f
        const val TWO_COLUMN_SHARE = 0.44f

        /** The height the rail needs for [rows] in [columns]: header, body and Stop. */
        fun height(rows: RailRows, columns: Int): Dp {
            val both = rows.toggles.isNotEmpty() && rows.links.isNotEmpty()
            val body = if (columns == 1) {
                ROW * (rows.toggles.size + rows.links.size) + if (both) GROUP_GAP else 0.dp
            } else {
                ROW * maxOf(rows.toggles.size, rows.links.size)
            }
            return INSET * 2 + PADDING * 2 + ROW + HEAD_GAP + body + STOP_GAP + ROW
        }

        /** The rail for [rows] in [width] x [height] dp, the safe area of the Game view. */
        fun of(width: Dp, height: Dp, rows: RailRows): RailSize {
            val columns = if (height(rows, 1) <= height) 1 else 2
            val share = if (columns == 1) ONE_COLUMN_SHARE else TWO_COLUMN_SHARE
            return RailSize(columns, maxOf(MIN_WIDTH, width * share), height(rows, columns) > height)
        }
    }
}

/** Takes every touch inside it, so none falls through to the on-screen controls or [TapToClose] under it. */
private fun Modifier.takesTouches(): Modifier = pointerInput(Unit) { awaitEachGesture { awaitFirstDown(requireUnconsumed = false) } }

/**
 * The in-game menu: a rail on the right edge, no scrim; the game stays live beside it and never
 * pauses, its on-screen controls too. The quick toggles (Keyboard, Touch pad, Show fps) act at
 * once and show over the game while the rail stays; the pages (Controls, Edit controls,
 * Settings) follow; Stop sits apart at the foot in red and asks in place first ([asking]; the
 * Game view hides the on-screen controls meanwhile). One column where the height allows, two
 * side by side on a phone on its side ([RailSize]); as wide as its labels. Back, ✕ or a tap on
 * the game closes it.
 */
@OptIn(ExperimentalFoundationApi::class)
@Composable
internal fun BoxScope.GameRail(
    visible: Boolean,
    asking: Boolean,
    onAsking: (Boolean) -> Unit,
    launcher: Boolean,
    keyboard: Boolean,
    touchPad: Boolean,
    showFps: Boolean,
    onClose: () -> Unit,
    onKeyboard: (Boolean) -> Unit,
    onTouchPad: (Boolean) -> Unit,
    onShowFps: (Boolean) -> Unit,
    onControls: () -> Unit,
    onEditControls: () -> Unit,
    onSettings: (() -> Unit)?,
    onStop: () -> Unit,
) {
    AnimatedVisibility(
        visible, Modifier.align(Alignment.CenterEnd),
        enter = slideInHorizontally { it } + fadeIn(), exit = slideOutHorizontally { it } + fadeOut(),
    ) {
        // The rail's width before Stop asked: the question keeps it rather than widening it.
        var settled by remember { mutableStateOf(0.dp) }
        val first = remember { FocusRequester() }
        InitialFocus(first)
        val density = LocalDensity.current
        val rows = RailRows.of(launcher, onSettings != null)
        BoxWithConstraints(Modifier.windowInsetsPadding(WindowInsets.safeDrawing).fillMaxHeight()) {
            val size = RailSize.of(maxWidth, maxHeight, rows)
            // Opaque: the on-screen controls it overlaps stay under it, unseen.
            Panel(
                Modifier.padding(RailSize.INSET).fillMaxHeight()
                    .widthIn(min = RailSize.MIN_WIDTH, max = size.maxWidth)
                    .then(if (asking && settled > 0.dp) Modifier.width(settled) else Modifier.width(IntrinsicSize.Max))
                    .onSizeChanged { if (!asking) settled = with(density) { it.width.toDp() } }
                    .takesTouches(),
                padding = RailSize.PADDING,
            ) {
                Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
                    Box(Modifier.weight(1f).padding(start = 14.dp, end = 8.dp)) { Live(launcher) }
                    LauncherButton(stringResource(R.string.game_menu_close), onClose, Modifier.width(RailSize.ROW), Kind.QUIET, height = RailSize.ROW, padding = 0.dp)
                }
                Box(Modifier.weight(1f).fillMaxWidth()) {
                    if (asking) {
                        StopAsk(launcher, onStop, onKeep = { onAsking(false) })
                    } else {
                        val item: @Composable (RailItem) -> Unit = { item ->
                            when (item) {
                                RailItem.KEYBOARD -> RailToggle(stringResource(R.string.game_view_keyboard), keyboard, onKeyboard, first)
                                RailItem.TOUCH_PAD -> RailToggle(stringResource(R.string.game_menu_touch_pad), touchPad, onTouchPad)
                                RailItem.SHOW_FPS -> RailToggle(stringResource(R.string.game_menu_show_fps), showFps, onShowFps)
                                RailItem.CONTROLS -> MenuRow(stringResource(R.string.game_menu_controls), onControls)
                                RailItem.EDIT_CONTROLS -> MenuRow(stringResource(R.string.game_menu_edit_controls), onEditControls)
                                RailItem.SETTINGS -> onSettings?.let { MenuRow(stringResource(R.string.game_menu_settings), it) }
                            }
                        }
                        val scroll = rememberScrollState()
                        val foot = RailSize.STOP_GAP + RailSize.ROW
                        val spec = remember(density) { ClearOfPills(with(density) { foot.toPx() }) }
                        CompositionLocalProvider(LocalBringIntoViewSpec provides spec) {
                            Column(Modifier.fillMaxSize().verticalScroll(scroll).padding(top = RailSize.HEAD_GAP, bottom = foot)) {
                                RailBody(rows, size.columns, item)
                            }
                        }
                        // Only while the body scrolls under Stop (an unmeasured scroll reports Int.MAX_VALUE).
                        val scrolling = scroll.maxValue in 1 until Int.MAX_VALUE
                        if (scrolling) {
                            val band = foot + FADE
                            Box(
                                Modifier.align(Alignment.BottomCenter).fillMaxWidth().height(band).background(
                                    Brush.verticalGradient(
                                        0f to Palette.Surface.copy(alpha = 0f),
                                        FADE / band to Palette.Surface.copy(alpha = 0.9f),
                                        1f to Palette.Surface,
                                    ),
                                ),
                            )
                        }
                        LauncherButton(
                            stringResource(Play.overlayStopLabel(launcher)), { onAsking(true) },
                            Modifier.align(Alignment.BottomCenter).fillMaxWidth(), Kind.DANGER, height = RailSize.ROW, floating = scrolling,
                        )
                    }
                }
            }
        }
    }
}

/** How far above Stop the scrolled rows fade out. */
private val FADE = 20.dp

/** The rail's groups: in one column with a hairline between, or side by side with a hairline down the middle. */
@Composable
private fun ColumnScope.RailBody(rows: RailRows, columns: Int, item: @Composable (RailItem) -> Unit) {
    if (columns == 2) {
        Row(Modifier.fillMaxWidth().height(IntrinsicSize.Min)) {
            Column(Modifier.width(IntrinsicSize.Max)) { rows.toggles.forEach { item(it) } }
            if (rows.links.isNotEmpty()) {
                Rule(Modifier.padding(horizontal = 6.dp).fillMaxHeight(), vertical = true)
                Column(Modifier.weight(1f)) { rows.links.forEach { item(it) } }
            }
        }
    } else {
        rows.toggles.forEach { item(it) }
        if (rows.toggles.isNotEmpty() && rows.links.isNotEmpty()) Rule(Modifier.padding(vertical = 6.dp))
        rows.links.forEach { item(it) }
    }
}

/**
 * Stop asking in place at the rail's foot: the question, Stop, and Keep playing (focused) where
 * Stop game was, so a second tap there keeps the game.
 */
@Composable
private fun BoxScope.StopAsk(launcher: Boolean, onStop: () -> Unit, onKeep: () -> Unit) {
    val keep = remember { FocusRequester() }
    InitialFocus(keep)
    Column(Modifier.align(Alignment.BottomCenter).fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(8.dp)) {
        Text(stringResource(Play.overlayStopBody(launcher)), style = Type.Body, color = Palette.Text, modifier = Modifier.padding(horizontal = 6.dp, vertical = 4.dp))
        LauncherButton(stringResource(R.string.game_menu_stop), onStop, Modifier.fillMaxWidth(), Kind.DANGER, height = RailSize.ROW)
        LauncherButton(stringResource(Play.overlayKeepLabel(launcher)), onKeep, Modifier.fillMaxWidth(), focusRequester = keep, height = RailSize.ROW)
    }
}

/** A quick toggle: its label on one line and the launcher toggle, which shows the state. One press. */
@Composable
private fun RailToggle(label: String, on: Boolean, onChange: (Boolean) -> Unit, focus: FocusRequester? = null) {
    val interaction = remember { MutableInteractionSource() }
    val focused by interaction.collectIsFocusedAsState()
    Row(
        Modifier.focusOn(focus).fillMaxWidth().heightIn(min = RailSize.ROW).focusMark(focused)
            .toggleable(on, interaction, LocalIndication.current, role = Role.Switch, onValueChange = onChange)
            .padding(horizontal = 14.dp, vertical = 4.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Text(label, style = Type.Body, color = Palette.Text, maxLines = 1, overflow = TextOverflow.Ellipsis, modifier = Modifier.weight(1f))
        Toggle(on)
    }
}

@Composable
private fun MenuRow(label: String, onClick: () -> Unit, modifier: Modifier = Modifier) {
    FocusRow(onClick, modifier, minHeight = RailSize.ROW) {
        Text(label, style = Type.Body, color = Palette.Text, maxLines = 1, overflow = TextOverflow.Ellipsis, modifier = Modifier.weight(1f))
        Text("›", style = Type.Title, color = Palette.Teal)
    }
}

/**
 * The in-game Controls panel's shape: [width] (three quarters of the screen's at most, never
 * over [MAX_WIDTH]) and its binding groups in [columns]: two where each gets [COLUMN].
 */
internal data class PanelSize(val width: Dp, val columns: Int) {
    companion object {
        val MAX_WIDTH = 480.dp
        /** A column of bindings: a cap and "Wheel down (next skill)" on one line. */
        val COLUMN = 200.dp
        /** The gap between the columns and the panel's padding either side. */
        private val CHROME = 16.dp + 12.dp * 2
        const val SHARE = 0.75f

        /** The panel in [width] dp, the safe area's width. */
        fun of(width: Dp): PanelSize {
            val w = minOf(MAX_WIDTH, width * SHARE)
            return PanelSize(w, if (w >= COLUMN * 2 + CHROME) 2 else 1)
        }
    }
}

/**
 * The in-game Controls panel, over the live game, about the physical controller: what each
 * button does now in pad and in mouse mode ([panelBindings]: grouped by where the buttons sit,
 * pad mode listing only the changed ones), the chord, and the pointer speed players change
 * mid-game. Buttons are changed in Settings; the on-screen controls in their editor
 * ([onEditOnScreen]).
 */
@Composable
internal fun BoxScope.ControlsPanel(
    visible: Boolean,
    controls: Controls,
    save: (Controls) -> Unit,
    onBack: () -> Unit,
    onChangeButtons: (() -> Unit)?,
    onEditOnScreen: () -> Unit,
) {
    AnimatedVisibility(visible, Modifier.align(Alignment.CenterEnd), enter = fadeIn() + slideInHorizontally { it / 3 }, exit = fadeOut()) {
        var mouse by remember { mutableStateOf(false) }
        val first = remember { FocusRequester() }
        InitialFocus(first)
        BoxWithConstraints(Modifier.windowInsetsPadding(WindowInsets.safeDrawing).fillMaxHeight()) {
            val size = PanelSize.of(maxWidth - RailSize.INSET * 2)
            Panel(Modifier.padding(RailSize.INSET).width(size.width).fillMaxHeight(), padding = 12.dp) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    LauncherButton(stringResource(R.string.back_to_menu), onBack, kind = Kind.QUIET, height = 48.dp, focusRequester = first, padding = 14.dp)
                    ScreenTitle(stringResource(R.string.controls_title), Modifier.weight(1f).padding(start = 8.dp))
                    Live()
                }
                val scroll = rememberScrollState()
                Box(Modifier.weight(1f).fillMaxWidth()) {
                    Column(Modifier.fillMaxSize().verticalScroll(scroll).padding(bottom = 12.dp)) {
                        Segmented(
                            listOf(stringResource(R.string.controls_pad_mode), stringResource(R.string.controls_mouse_mode)),
                            if (mouse) 1 else 0, { mouse = it == 1 }, Modifier.fillMaxWidth().padding(vertical = 6.dp),
                        )
                        val groups = panelBindings(controls, mouse)
                        val lead = when {
                            mouse -> R.string.controls_mouse_mode_body
                            groups.isEmpty() -> R.string.controls_panel_pad_all
                            else -> R.string.controls_panel_pad_changed
                        }
                        Text(stringResource(lead), style = Type.Hint, color = Palette.Muted, modifier = Modifier.padding(horizontal = 4.dp, vertical = 4.dp))
                        if (size.columns == 2 && groups.size > 1) {
                            Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(16.dp)) {
                                for (column in 0..1) {
                                    Column(Modifier.weight(1f)) { groups.filter { it.cluster?.column == column }.forEach { BindingBlock(it) } }
                                }
                            }
                        } else {
                            groups.forEach { BindingBlock(it) }
                        }
                        Text(
                            stringResource(
                                R.string.controls_panel_chord,
                                stringResource(if (controls.chord == Chord.SELECT_START) R.string.controls_chord_select_start else R.string.controls_chord_thumbs),
                            ),
                            style = Type.Hint, color = Palette.Muted, modifier = Modifier.padding(horizontal = 4.dp, vertical = 10.dp),
                        )
                        Rule(Modifier.padding(vertical = 4.dp))
                        PointerSpeed(controls, save)
                        onChangeButtons?.let { TextLink(stringResource(R.string.controls_change_in_settings), it) }
                        TextLink(stringResource(R.string.controls_edit_on_screen), onEditOnScreen)
                    }
                    // More below: the rows fade out at the panel's foot rather than end cut through.
                    if (scroll.canScrollForward) {
                        Box(
                            Modifier.align(Alignment.BottomCenter).fillMaxWidth().height(PANEL_FADE)
                                .background(Brush.verticalGradient(listOf(Palette.Surface.copy(alpha = 0f), Palette.Surface))),
                        )
                    }
                }
            }
        }
    }
}

/** How far above its foot the Controls panel's rows fade out while more lies below. */
private val PANEL_FADE = 40.dp

/** One group of bindings: its name (none for pad mode's changed buttons), then its lines. */
@Composable
private fun BindingBlock(group: BindingGroup) {
    Column {
        group.cluster?.let { GroupLabel(stringResource(it.label), Modifier.padding(horizontal = 4.dp)) }
        group.rows.forEach { BindingLine(it) }
    }
}

/**
 * One line: the caps of the buttons that share it (the d-pad on its arrow keys as one cap) and
 * what they do; a default muted, as in Settings, so a changed one stands out.
 */
@Composable
private fun BindingLine(row: BindingRow) {
    Row(
        Modifier.fillMaxWidth().heightIn(min = 36.dp).padding(vertical = 3.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        Row(horizontalArrangement = Arrangement.spacedBy(4.dp)) {
            if (row.action == null) {
                KeyCap(row.buttons.joinToString(" ") { buttonCap(it) })
            } else {
                row.buttons.forEach { KeyCap(buttonCap(it), Modifier.widthIn(min = 44.dp)) }
            }
        }
        Text(
            row.action?.let { actionLine(it) } ?: stringResource(R.string.controls_dpad_arrows),
            style = Type.Hint, color = if (row.isDefault) Palette.Muted else Palette.Text,
            maxLines = 2, overflow = TextOverflow.Ellipsis, modifier = Modifier.weight(1f),
        )
    }
}
