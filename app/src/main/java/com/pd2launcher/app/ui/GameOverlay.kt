package com.pd2launcher.app.ui

import android.os.SystemClock
import android.view.KeyEvent
import androidx.annotation.DrawableRes
import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.core.animateFloatAsState
import androidx.compose.animation.core.tween
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.slideInHorizontally
import androidx.compose.animation.slideOutHorizontally
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.LocalIndication
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.awaitEachGesture
import androidx.compose.foundation.gestures.awaitFirstDown
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.interaction.collectIsFocusedAsState
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.relocation.BringIntoViewRequester
import androidx.compose.foundation.relocation.bringIntoViewRequester
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxScope
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.IntrinsicSize
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.RowScope
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
import androidx.compose.foundation.selection.toggleable
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
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
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.disabled
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.semantics.testTag
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import com.pd2launcher.app.FpsReading
import com.pd2launcher.app.GameDisplay
import com.pd2launcher.app.LauncherStart
import com.pd2launcher.app.R
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

/**
 * The fps counter over the game: small and quiet in a corner; it takes no input. [fps] is read
 * here only, so its four changes a second recompose the counter and nothing around it (the menu).
 */
@Composable
internal fun FpsCounter(fps: () -> FpsReading?, modifier: Modifier) {
    Text(
        fpsText(fps()), style = Type.Hint, color = Palette.Text, maxLines = 1,
        modifier = modifier.padding(6.dp).alpha(0.6f).clip(RowShape).background(Palette.Background.copy(alpha = 0.5f))
            .padding(horizontal = 6.dp, vertical = 2.dp),
    )
}

/**
 * "Starting the official PD2 launcher…" under a spinner in the middle of the black X screen while
 * the launcher starts ([LauncherStart]), in the fps counter's style; from [LauncherStart.SLOW_MS]
 * after [since] (elapsed real time) a second line says that the launcher can take a minute to
 * start on slower phones. It takes no touch.
 */
@Composable
internal fun LauncherStartingNotice(since: Long, modifier: Modifier) {
    var slow by remember(since) { mutableStateOf(LauncherStart.slow(since, SystemClock.elapsedRealtime())) }
    LaunchedEffect(since) {
        delay(since + LauncherStart.SLOW_MS - SystemClock.elapsedRealtime())
        slow = true
    }
    Column(
        modifier.padding(16.dp).widthIn(max = 460.dp).clip(RowShape).background(Palette.Background.copy(alpha = 0.8f))
            .padding(horizontal = 16.dp, vertical = 12.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        CircularProgressIndicator(Modifier.size(28.dp), strokeWidth = 3.dp, color = Palette.Teal, trackColor = Palette.Rule)
        Text(
            stringResource(R.string.launcher_starting), style = Type.Body, color = Palette.Text, textAlign = TextAlign.Center,
            modifier = Modifier.padding(top = 10.dp),
        )
        if (slow) {
            Text(
                stringResource(R.string.launcher_starting_slow), style = Type.Hint, color = Palette.Muted, textAlign = TextAlign.Center,
                modifier = Modifier.padding(top = 4.dp),
            )
        }
    }
}

/**
 * Over the game beside the menu ([GameLayer.tapCloses]), under the on-screen controls and the
 * menu: a tap on what they leave of the screen closes the menu and never reaches the game.
 */
@Composable
internal fun TapToClose(onClose: () -> Unit) {
    val close by rememberUpdatedState(onClose)
    Box(Modifier.fillMaxSize().pointerInput(Unit) { detectTapGestures(onTap = { close() }) })
}

/** The in-game menu's two tabs. The strip's gamepad between them opens the Controls screen ([MenuStop]). */
internal enum class MenuTab { QUICK, SETTINGS }

/**
 * The strip's three stops, in its order: Quick, Controls (the full Controls screen) and
 * Settings. L1 and R1 step one stop at a time, with no wrap at the ends.
 */
internal enum class MenuStop {
    QUICK,
    CONTROLS,
    SETTINGS,
    ;

    companion object {
        /** Whether [keyCode] is a shoulder button that steps along the strip (L1 back, R1 on). */
        fun isStep(keyCode: Int): Boolean = keyCode == KeyEvent.KEYCODE_BUTTON_L1 || keyCode == KeyEvent.KEYCODE_BUTTON_R1

        /**
         * The stop the shoulder button [keyCode] goes to from [from]; null at an end or for another
         * key. [controls]: the strip has the Controls stop (not over the official launcher); with
         * none, a step passes it (also from it, should the launcher come while it shows).
         */
        fun step(from: MenuStop, keyCode: Int, controls: Boolean): MenuStop? {
            val by = when (keyCode) {
                KeyEvent.KEYCODE_BUTTON_L1 -> -1
                KeyEvent.KEYCODE_BUTTON_R1 -> 1
                else -> return null
            }
            var at = from.ordinal + by
            if (!controls && entries.getOrNull(at) == CONTROLS) at += by
            return entries.getOrNull(at)
        }

        /**
         * The stop the shoulder button [keyCode] steps from over [layer]: the menu's [tab], or the
         * Controls screen; null where L1 and R1 do not step: another key, another layer, or a
         * [popup] over it (the Frame rate input, the action picker, the Pointer speed input).
         */
        fun from(keyCode: Int, layer: GameLayer, tab: MenuTab, popup: Boolean): MenuStop? = when {
            !isStep(keyCode) || popup -> null
            layer == GameLayer.MENU -> of(tab)
            layer == GameLayer.CONTROLS -> CONTROLS
            else -> null
        }

        /** The stop of the menu's [tab]. */
        fun of(tab: MenuTab): MenuStop = if (tab == MenuTab.QUICK) QUICK else SETTINGS
    }
}

/** A quick tile: one state that a press changes at once, the menu open. */
internal enum class QuickTile { ON_SCREEN, FPS, DISPLAY, KEYBOARD }

/**
 * What the menu offers over what runs. Over the game: the four tiles, Frame rate and the
 * Controls screen. Over the official PD2 [launcher], a mouse and touch window with no on-screen
 * controls, no game fps and no controller to set up: the Display tile (the game's Display, which
 * shows the launcher's window too) and the Keyboard tile. The Settings tab is the same for both;
 * its Stop closes the launcher there.
 */
internal data class MenuItems(val tiles: List<QuickTile>, val frameRate: Boolean, val controls: Boolean) {
    companion object {
        fun of(launcher: Boolean): MenuItems =
            if (launcher) {
                MenuItems(listOf(QuickTile.DISPLAY, QuickTile.KEYBOARD), frameRate = false, controls = false)
            } else {
                MenuItems(QuickTile.entries, frameRate = true, controls = true)
            }
    }
}

/** The in-game menu's sizes, in dp: one panel of one size on every tab and every device, as wide as its words need. */
internal object MenuSize {
    /** Every target: a 48 dp square at least. */
    val ROW = 48.dp
    /** From the screen's safe edge (camera cutouts, system bars) to the menu. */
    val INSET = 8.dp
    /** Inside the panel, around the tab's body. */
    val PADDING = 8.dp
    /** The panel's least width; a word that needs more widens it, so none is cut. */
    val MIN_WIDTH = 264.dp
    /** The tab's title line. */
    val HEAD = 40.dp
    val HEAD_GAP = 4.dp
    /** Between the tiles. */
    val TILE_GAP = 6.dp
    /** A tile's least height: its icon and state over a label of two lines. */
    val TILE_HEIGHT = 74.dp
}

/** What the in-game menu shows: each tile's and switch's state now. */
internal data class MenuState(
    val launcher: Boolean,
    val onScreen: Boolean,
    val showFps: Boolean,
    val display: GameDisplay,
    /** Android's keyboard is up. */
    val keyboard: Boolean,
    /** The game's frame rate as the player reads it: a number of frames a second. */
    val frameRate: String,
    val slowerInBackground: Boolean,
    /** The Launcher's Sound in background; null while the Launcher's settings are not known (the switch waits). */
    val soundInBackground: Boolean?,
    val keepScreenOn: Boolean,
)

/** What the in-game menu's tiles, rows and switches do. */
internal class MenuActions(
    val onClose: () -> Unit,
    val onOnScreen: (Boolean) -> Unit,
    val onShowFps: (Boolean) -> Unit,
    val onDisplay: () -> Unit,
    val onKeyboard: (Boolean) -> Unit,
    val onFrameRate: () -> Unit,
    val onControls: () -> Unit,
    val onSlowerInBackground: (Boolean) -> Unit,
    val onSoundInBackground: (Boolean) -> Unit,
    val onKeepScreenOn: (Boolean) -> Unit,
    val onStop: () -> Unit,
)

/** The in-game panels' test tags: debug captures measure them (OverlayProbe). */
internal const val MENU_TAG = "game-menu"
internal const val CONTROLS_TAG = "controls-screen"
internal const val NUMBER_INPUT_TAG = "number-input"

/** What only takes its room (the hidden tab, the settings under the Stop question): debug captures leave it out. */
internal const val UNSEEN_TAG = "unseen"

/** Takes every touch inside it, so none falls through to the on-screen controls or [TapToClose] under it. */
internal fun Modifier.takesTouches(): Modifier = pointerInput(Unit) { awaitEachGesture { awaitFirstDown(requireUnconsumed = false) } }

/**
 * The in-game menu: one panel at the right edge, no scrim; the game stays live beside it and
 * never pauses, its on-screen controls too (with Slower game in the background on, the game is
 * in the background meanwhile, [GameLayer.suspends], and the on-screen controls show dimmed and
 * inert, [GameLayer.padInert]). A console tab strip on the screen's edge: ✕, then
 * Quick, the gamepad (the Controls screen, which keeps the strip in the same place) and Settings;
 * L1 and R1 step along it one at a time (the Game view, [MenuStop]; [focusTab] counts the steps
 * into a tab, which takes the focus). Quick: four tiles (On-screen controls, FPS counter, Display, Keyboard)
 * that change at once, then Frame rate, which opens its input. Settings: three of the app's
 * settings that apply at once, then a quiet Stop game, which asks in place first ([asking]; the
 * Game view hides the on-screen controls meanwhile). Both tabs share one size ([MenuSize]),
 * so nothing moves when the tab changes. While Android's keyboard is up the menu keeps above it
 * and scrolls if it must. Back, ✕ or a tap on the game closes it. It slides in and out at the
 * screen's edge; [fades]: it only fades, to and from the Controls screen. [onPanelHeight]: its panel's height
 * in px, for the Controls screen's strip ([ControlsScreen]).
 */
@Composable
internal fun BoxScope.GameMenu(
    visible: Boolean,
    tab: MenuTab,
    onTab: (MenuTab) -> Unit,
    asking: Boolean,
    onAsking: (Boolean) -> Unit,
    state: MenuState,
    actions: MenuActions,
    focusTab: Int,
    onPanelHeight: (Int) -> Unit,
    fades: Boolean = false,
) {
    AnimatedVisibility(
        visible, Modifier.align(Alignment.CenterEnd),
        enter = if (fades) fadeIn() else slideInHorizontally { it } + fadeIn(),
        exit = if (fades) fadeOut() else slideOutHorizontally { it } + fadeOut(),
    ) {
        val items = MenuItems.of(state.launcher)
        val quick = remember { FocusRequester() }
        val settings = remember { FocusRequester() }
        InitialFocus(if (tab == MenuTab.QUICK) quick else settings)
        // A shoulder button's step moves the focus into the tab it opens; a press on the strip keeps it there.
        LaunchedEffect(focusTab) {
            if (focusTab > 0) runCatching { (if (tab == MenuTab.QUICK) quick else settings).requestFocus() }
        }
        BoxWithConstraints(Modifier.windowInsetsPadding(WindowInsets.safeDrawing).fillMaxHeight(), contentAlignment = Alignment.CenterEnd) {
            Panel(
                Modifier.padding(MenuSize.INSET).onSizeChanged { onPanelHeight(it.height) }
                    .heightIn(max = (maxHeight - MenuSize.INSET * 2).coerceAtLeast(0.dp))
                    .widthIn(min = MenuSize.MIN_WIDTH).width(IntrinsicSize.Min)
                    .takesTouches().testTag(MENU_TAG),
                padding = 0.dp,
            ) {
                // Scrolls only where the room is short (Android's keyboard up).
                Row(Modifier.verticalScroll(rememberScrollState()).height(IntrinsicSize.Max)) {
                    Column(Modifier.weight(1f).fillMaxHeight().padding(start = MenuSize.PADDING, top = MenuSize.PADDING, bottom = MenuSize.PADDING)) {
                        Box(Modifier.heightIn(min = MenuSize.HEAD).padding(horizontal = 14.dp), contentAlignment = Alignment.CenterStart) {
                            Text(
                                stringResource(if (tab == MenuTab.QUICK) R.string.game_menu_quick else R.string.game_menu_settings),
                                style = Type.Button, color = Palette.Text, modifier = Modifier.semantics { heading() },
                            )
                        }
                        Spacer(Modifier.height(MenuSize.HEAD_GAP))
                        // Both tabs are laid out, the hidden one unseen and untouchable, so the panel has the larger one's size on both.
                        Box(Modifier.weight(1f).fillMaxWidth()) {
                            Pane(tab == MenuTab.QUICK) { shown -> QuickPane(items, state, actions, shown, quick) }
                            Pane(tab == MenuTab.SETTINGS) { shown ->
                                SettingsPane(state, actions, asking, onAsking, shown, settings)
                            }
                        }
                    }
                    Rule(Modifier.padding(start = 8.dp, end = 4.dp, top = MenuSize.PADDING, bottom = MenuSize.PADDING).fillMaxHeight(), vertical = true)
                    TabStrip(
                        MenuStop.of(tab), items.controls,
                        onStop = { stop ->
                            when (stop) {
                                MenuStop.QUICK -> onTab(MenuTab.QUICK)
                                MenuStop.SETTINGS -> onTab(MenuTab.SETTINGS)
                                MenuStop.CONTROLS -> actions.onControls()
                            }
                        },
                        onClose = actions.onClose,
                    )
                }
            }
        }
    }
}

/**
 * One tab's body: [shown] it is seen and takes input; hidden it still takes its room (the panel
 * keeps one size), unseen, without semantics, and its rows take no focus or touch.
 */
@Composable
private fun BoxScope.Pane(shown: Boolean, content: @Composable ColumnScope.(shown: Boolean) -> Unit) {
    // Both fill the tab's area, so the Settings tab's Stop sits at its foot.
    Column(Modifier.fillMaxSize().then(if (shown) Modifier else Modifier.unseen()), content = { content(shown) })
}

/** Laid out for its room, but unseen and without semantics (its rows take no focus or touch themselves). */
internal fun Modifier.unseen(): Modifier = alpha(0f).clearAndSetSemantics { testTag = UNSEEN_TAG }

/** Quick: the tiles that apply, two by two, then Frame rate. */
@Composable
private fun ColumnScope.QuickPane(items: MenuItems, state: MenuState, actions: MenuActions, shown: Boolean, first: FocusRequester) {
    items.tiles.chunked(2).forEachIndexed { index, row ->
        if (index > 0) Spacer(Modifier.height(MenuSize.TILE_GAP))
        Row(Modifier.fillMaxWidth().height(IntrinsicSize.Max), horizontalArrangement = Arrangement.spacedBy(MenuSize.TILE_GAP)) {
            row.forEachIndexed { column, tile ->
                QuickTileView(tile, state, actions, shown, if (index == 0 && column == 0) first else null)
            }
            if (row.size == 1) Spacer(Modifier.weight(1f))
        }
    }
    if (items.frameRate) {
        Spacer(Modifier.height(8.dp))
        LinkRow(stringResource(R.string.settings_frame_rate), state.frameRate, actions.onFrameRate, shown, oneLine = true)
    }
}

@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun RowScope.QuickTileView(tile: QuickTile, state: MenuState, actions: MenuActions, shown: Boolean, focus: FocusRequester?) {
    val on = stringResource(R.string.game_menu_on)
    val off = stringResource(R.string.game_menu_off)
    val fit = stringResource(R.string.game_menu_display_fit)
    val center = stringResource(R.string.game_menu_display_center)
    when (tile) {
        QuickTile.ON_SCREEN -> Tile(R.drawable.ic_tile_on_screen, stringResource(R.string.on_screen_controls), state.onScreen, on, off, shown, focus) {
            actions.onOnScreen(!state.onScreen)
        }
        QuickTile.FPS -> Tile(R.drawable.ic_tile_fps, stringResource(R.string.game_menu_fps_counter), state.showFps, on, off, shown, focus) {
            actions.onShowFps(!state.showFps)
        }
        // A choice of two, not on or off: never tinted; its word is the one in use.
        QuickTile.DISPLAY -> Tile(
            R.drawable.ic_tile_display, stringResource(R.string.game_menu_display), null,
            if (state.display == GameDisplay.FIT) fit else center, if (state.display == GameDisplay.FIT) center else fit, shown, focus,
        ) { actions.onDisplay() }
        QuickTile.KEYBOARD -> {
            // With Android's keyboard up the menu may not fit above it: it scrolls to this tile, which hides it.
            val inView = remember { BringIntoViewRequester() }
            LaunchedEffect(state.keyboard, shown) { if (state.keyboard && shown) runCatching { inView.bringIntoView() } }
            Tile(
                R.drawable.ic_tile_keyboard, stringResource(R.string.game_view_keyboard), state.keyboard, on, off, shown, focus,
                Modifier.bringIntoViewRequester(inView),
            ) { actions.onKeyboard(!state.keyboard) }
        }
    }
}

/**
 * A quick tile: its icon and state word on top, its label below. [on] null: a choice of two
 * ([word] the one in use, [other] the one a press picks); else on or off, tinted while on. The
 * word keeps the room of the longer of the two, so a press never changes the tile's size.
 */
@Composable
private fun RowScope.Tile(
    @DrawableRes icon: Int,
    label: String,
    on: Boolean?,
    onWord: String,
    offWord: String,
    shown: Boolean,
    focus: FocusRequester?,
    modifier: Modifier = Modifier,
    onPress: () -> Unit,
) {
    val interaction = remember { MutableInteractionSource() }
    val focused by interaction.collectIsFocusedAsState()
    val lit = on == true
    val word = if (on == false) offWord else onWord
    val other = if (on == false) onWord else offWord
    val press = when {
        !shown -> Modifier
        on == null -> Modifier.clickable(interaction, LocalIndication.current, role = Role.Button, onClick = onPress).semantics { stateDescription = word }
        else -> Modifier.toggleable(on, interaction, LocalIndication.current, role = Role.Switch) { onPress() }
    }
    Column(
        modifier.weight(1f).fillMaxHeight().heightIn(min = MenuSize.TILE_HEIGHT).focusOn(focus)
            .clip(RowShape)
            .background(if (lit) Palette.Teal.copy(alpha = 0.16f) else if (focused) Palette.Raised else Color.Transparent)
            .border(
                if (focused) 2.dp else 1.dp,
                when {
                    focused -> Palette.Teal
                    lit -> Palette.Teal.copy(alpha = 0.32f)
                    else -> Palette.Rule
                },
                RowShape,
            )
            .then(press)
            .padding(start = 12.dp, end = 10.dp, top = 10.dp, bottom = 10.dp),
        verticalArrangement = Arrangement.SpaceBetween,
    ) {
        Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
            Icon(painterResource(icon), null, Modifier.size(22.dp), tint = Palette.Text)
            Spacer(Modifier.weight(1f).widthIn(min = 6.dp))
            // The state is in the tile's own semantics (on or off, or its state description): the words are not read again.
            Box(Modifier.clearAndSetSemantics {}, contentAlignment = Alignment.CenterEnd) {
                val style = Type.Hint.copy(fontWeight = FontWeight.SemiBold)
                Text(other, style = style, color = Color.Transparent, maxLines = 1, softWrap = false, modifier = Modifier.clearAndSetSemantics {})
                Text(word, style = style, color = Palette.Text, maxLines = 1, softWrap = false, textAlign = TextAlign.End)
            }
        }
        Spacer(Modifier.height(8.dp))
        Text(label, style = Type.Hint.copy(fontWeight = FontWeight.Medium), color = Palette.Text)
    }
}

/** Settings: the app's settings the game follows at once, then Stop game apart at the foot, or the Stop question in its place. */
@Composable
private fun ColumnScope.SettingsPane(
    state: MenuState,
    actions: MenuActions,
    asking: Boolean,
    onAsking: (Boolean) -> Unit,
    shown: Boolean,
    first: FocusRequester,
) {
    // The question shows only in the shown tab: a change of tab forgets it (GameView).
    val question = asking && shown
    val rows = shown && !question
    Box(Modifier.weight(1f).fillMaxWidth()) {
        // The settings keep their room under the question, so the menu keeps its size.
        Column(Modifier.fillMaxSize().then(if (question) Modifier.unseen() else Modifier)) {
            MenuSwitch(stringResource(R.string.settings_background_frame_rate), state.slowerInBackground, rows, first, onChange = actions.onSlowerInBackground)
            MenuSwitch(
                stringResource(R.string.settings_sound_in_background), state.soundInBackground == true, rows, null,
                waiting = state.soundInBackground == null, onChange = actions.onSoundInBackground,
            )
            MenuSwitch(stringResource(R.string.settings_keep_screen_on), state.keepScreenOn, rows, null, onChange = actions.onKeepScreenOn)
            Spacer(Modifier.weight(1f).heightIn(min = 8.dp))
            Rule(Modifier.padding(vertical = 6.dp))
            StopRow(stringResource(Play.overlayStopLabel(state.launcher)), rows) { onAsking(true) }
        }
        if (question) {
            Column(Modifier.matchParentSize()) { StopAsk(state.launcher, actions.onStop) { onAsking(false) } }
        }
    }
}

/**
 * Stop asking in place, in the Settings tab: the question, Stop game, and Keep playing (focused) at
 * the foot where Stop game was, so a second press there keeps the game. Always whole in view:
 * the tab is taller than the question and its two buttons.
 */
@Composable
private fun ColumnScope.StopAsk(launcher: Boolean, onStop: () -> Unit, onKeep: () -> Unit) {
    val keep = remember { FocusRequester() }
    InitialFocus(keep)
    Spacer(Modifier.weight(1f))
    Text(stringResource(Play.overlayStopBody(launcher)), style = Type.Body, color = Palette.Text, modifier = Modifier.padding(horizontal = 6.dp))
    Spacer(Modifier.height(8.dp))
    LauncherButton(stringResource(Play.overlayStopLabel(launcher)), onStop, Modifier.fillMaxWidth(), Kind.DANGER, height = MenuSize.ROW)
    Spacer(Modifier.height(8.dp))
    LauncherButton(stringResource(Play.overlayKeepLabel(launcher)), onKeep, Modifier.fillMaxWidth(), focusRequester = keep, height = MenuSize.ROW)
}

/**
 * A switch row with the app's title for the setting only; the whole row switches. [waiting]: its
 * value is not known yet (the Launcher's settings), so it shows off, faint, and takes no press.
 */
@Composable
private fun MenuSwitch(label: String, on: Boolean, shown: Boolean, focus: FocusRequester?, waiting: Boolean = false, onChange: (Boolean) -> Unit) {
    val interaction = remember { MutableInteractionSource() }
    val focused by interaction.collectIsFocusedAsState()
    Row(
        Modifier.focusOn(focus).fillMaxWidth().heightIn(min = MenuSize.ROW).focusMark(focused)
            .then(
                when {
                    !shown -> Modifier
                    waiting -> Modifier.semantics { disabled() }
                    else -> Modifier.toggleable(on, interaction, LocalIndication.current, role = Role.Switch, onValueChange = onChange)
                },
            )
            .padding(horizontal = 14.dp, vertical = 4.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Text(label, style = Type.Body, color = if (waiting) Palette.Faint else Palette.Text, modifier = Modifier.weight(1f))
        Toggle(on)
    }
}

/** The quiet Stop row: red words, no pill; it asks first. */
@Composable
private fun StopRow(label: String, shown: Boolean, onClick: () -> Unit) {
    val interaction = remember { MutableInteractionSource() }
    val focused by interaction.collectIsFocusedAsState()
    Row(
        Modifier.fillMaxWidth().heightIn(min = MenuSize.ROW).focusMark(focused)
            .then(if (shown) Modifier.clickable(interaction, LocalIndication.current, role = Role.Button, onClick = onClick) else Modifier)
            .padding(horizontal = 14.dp, vertical = 4.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(label, style = Type.Body, color = Palette.Red)
    }
}

/**
 * A row that opens something: its label, its value in the text colour, a chevron. [oneLine]: the
 * label never wraps, so the row asks for its whole width (the menu widens for it).
 */
@Composable
internal fun LinkRow(label: String, value: String?, onClick: () -> Unit, enabled: Boolean = true, focus: FocusRequester? = null, oneLine: Boolean = false) {
    val interaction = remember { MutableInteractionSource() }
    val focused by interaction.collectIsFocusedAsState()
    Row(
        Modifier.focusOn(focus).fillMaxWidth().heightIn(min = MenuSize.ROW).focusMark(focused)
            .then(if (enabled) Modifier.clickable(interaction, LocalIndication.current, role = Role.Button, onClick = onClick) else Modifier)
            .padding(start = 14.dp, end = 9.dp, top = 4.dp, bottom = 4.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        // One line: the label asks for its whole width also where the menu measures its least width.
        Text(
            label, style = Type.Body, color = Palette.Text, softWrap = !oneLine,
            modifier = Modifier.weight(1f).then(if (oneLine) Modifier.width(IntrinsicSize.Max) else Modifier),
        )
        // A text that never wraps still reports its longest word as its least width: the whole value counts.
        if (value != null) Text(value, style = Type.Body, color = Palette.Text, maxLines = 1, softWrap = false, modifier = Modifier.width(IntrinsicSize.Max))
        Icon(painterResource(R.drawable.ic_chevron_right), null, Modifier.size(20.dp), tint = Palette.Muted)
    }
}

/**
 * The strip on the screen's edge, in the menu and beside the Controls screen alike: ✕ ([onClose]:
 * back to the game), then the L1 hint, Quick, the gamepad ([controls]: the Controls screen),
 * Settings and the R1 hint; [stop] is the shown one, tinted.
 */
@Composable
internal fun TabStrip(stop: MenuStop, controls: Boolean, onStop: (MenuStop) -> Unit, onClose: () -> Unit, modifier: Modifier = Modifier) {
    Column(modifier.width(MenuSize.ROW), horizontalAlignment = Alignment.CenterHorizontally) {
        IconTarget(R.drawable.ic_close, stringResource(R.string.game_menu_close), onClose, iconSize = 24.dp)
        Spacer(Modifier.height(10.dp))
        Column(horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.spacedBy(4.dp)) {
            HintCap(stringResource(R.string.game_menu_l1))
            StripTab(R.drawable.ic_menu_quick, stringResource(R.string.game_menu_quick), stop == MenuStop.QUICK) { onStop(MenuStop.QUICK) }
            if (controls) {
                StripTab(R.drawable.ic_menu_controls, stringResource(R.string.controls_title), stop == MenuStop.CONTROLS) { onStop(MenuStop.CONTROLS) }
            }
            StripTab(R.drawable.ic_menu_settings, stringResource(R.string.game_menu_settings), stop == MenuStop.SETTINGS) { onStop(MenuStop.SETTINGS) }
            HintCap(stringResource(R.string.game_menu_r1))
        }
    }
}

/** One of the strip's icons: a 48 dp square, tinted while its tab shows. */
@Composable
private fun StripTab(@DrawableRes icon: Int, label: String, selected: Boolean, onClick: () -> Unit) {
    val interaction = remember { MutableInteractionSource() }
    val focused by interaction.collectIsFocusedAsState()
    Box(
        Modifier.size(MenuSize.ROW).focusMark(focused, selected)
            .selectable(selected, interaction, LocalIndication.current, role = Role.Tab, onClick = onClick)
            .semantics { contentDescription = label },
        contentAlignment = Alignment.Center,
    ) {
        Icon(painterResource(icon), null, Modifier.size(24.dp), tint = if (selected) Palette.Teal else Palette.Muted)
    }
}

/** A controller button's name as a quiet hint, not a target: L1 and R1 beside the tabs. */
@Composable
private fun HintCap(text: String) {
    Box(
        Modifier.widthIn(min = 30.dp).height(20.dp).border(1.dp, Palette.Rule, RoundedCornerShape(6.dp)).padding(horizontal = 6.dp),
        contentAlignment = Alignment.Center,
    ) {
        Text(text, style = Type.Glyph.copy(fontSize = Type.Label.fontSize), color = Palette.Muted, maxLines = 1, softWrap = false)
    }
}

/** An icon button: a 48 dp circle (or [shape]) with the focus ring; [description] for TalkBack. */
@Composable
internal fun IconTarget(
    @DrawableRes icon: Int,
    description: String,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
    iconSize: Dp = 24.dp,
    shape: RoundedCornerShape = CircleShapeRounded,
    tint: Color = Palette.Text,
) {
    val interaction = remember { MutableInteractionSource() }
    val focused by interaction.collectIsFocusedAsState()
    Box(
        modifier.size(MenuSize.ROW).focusMark(focused, shape = shape)
            .clickable(interaction, LocalIndication.current, role = Role.Button, onClick = onClick)
            .semantics { contentDescription = description },
        contentAlignment = Alignment.Center,
    ) {
        Icon(painterResource(icon), null, Modifier.size(iconSize), tint = tint)
    }
}

/** A round shape for [focusMark], which takes rounded corner shapes. */
private val CircleShapeRounded = RoundedCornerShape(percent = 50)
