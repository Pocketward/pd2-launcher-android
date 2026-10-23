package com.pd2launcher.app.ui

import android.view.KeyEvent
import com.pd2launcher.app.input.Keys
import androidx.compose.ui.unit.sp
import androidx.compose.ui.semantics.Role
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.interaction.collectIsFocusedAsState
import androidx.compose.foundation.LocalIndication
import android.view.InputDevice
import androidx.activity.compose.BackHandler
import androidx.compose.animation.core.animateFloatAsState
import androidx.compose.animation.core.tween
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.focusable
import androidx.compose.foundation.gestures.awaitEachGesture
import androidx.compose.foundation.gestures.awaitFirstDown
import androidx.compose.foundation.gestures.calculatePan
import androidx.compose.foundation.gestures.calculateZoom
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxScope
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawing
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Slider
import androidx.compose.material3.SliderDefaults
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.draw.clip
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.geometry.CornerRadius
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.PathEffect
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.input.key.key
import androidx.compose.ui.input.key.nativeKeyCode
import androidx.compose.ui.input.key.onPreviewKeyEvent
import androidx.compose.ui.input.key.type
import androidx.compose.ui.input.key.KeyEventType
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalLayoutDirection
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.IntOffset
import androidx.compose.ui.unit.IntSize
import androidx.compose.ui.unit.dp
import com.pd2launcher.app.R
import com.pd2launcher.app.input.Area
import com.pd2launcher.app.input.Controls
import com.pd2launcher.app.input.ControlsLayout
import com.pd2launcher.app.input.ControlsStore
import com.pd2launcher.app.input.LayoutStore
import com.pd2launcher.app.input.PadAction
import com.pd2launcher.app.input.Piece
import com.pd2launcher.app.input.chipRowAt
import kotlinx.coroutines.delay
import kotlin.math.hypot
import kotlin.math.roundToInt

/** A custom button being made ([id] null) or changed: its binding so far and its label. */
private data class Draft(val id: String?, val action: PadAction?, val label: String)

/** The longest custom button label. */
private const val LABEL_MAX = 6

/**
 * The on-screen controls editor, over the live game or, when not playing ([sample]), a sample
 * screen. Nothing is open on entry: the pieces over a light dim and a faint snap grid, the menu
 * glyph, and a hint that fades.
 *
 * - A finger drags a piece at once (an 8 dp grid, drawn to the edges and centre lines, kept on
 *   screen); two pinch it; a tap selects it: a teal outline, a corner handle that resizes it,
 *   and a chip row just outside it (opacity, Hide or, for a custom button, Rebind, Label and
 *   Delete). A tap on nothing lets go of it.
 * - The glyph opens the one menu, which holds every on-screen option (Settings has none): when
 *   the controls show and their opacity ([Controls]), Add button, Show hidden pieces, Reset to
 *   default (asked at the menu's foot), Cancel and Done (the layout kept for this screen size,
 *   [LayoutStore]).
 * - A controller: the d-pad walks the pieces, A selects, B lets go or closes (Back), Start
 *   opens the menu.
 *
 * Cancel and Back with nothing open drop every change.
 */
@Composable
internal fun ControlsEditor(sample: Boolean, onClose: () -> Unit) {
    val context = LocalContext.current
    remember { ControlsStore.load(context) }
    val controls by ControlsStore.current.collectAsState()
    BoxWithConstraints(Modifier.fillMaxSize()) {
        val widthDp = maxWidth.value.roundToInt()
        val heightDp = maxHeight.value.roundToInt()
        val width = constraints.maxWidth.toFloat()
        val height = constraints.maxHeight.toFloat()
        val density = LocalDensity.current
        val minHit = with(density) { MIN_TARGET.toPx() }
        val grid = with(density) { 8.dp.toPx() }
        val edge = with(density) { 16.dp.toPx() }
        val minSide = with(density) { 28.dp.toPx() }
        val menuHeight = maxHeight - 12.dp
        // The menu glyph's corner: a finger landing there is the glyph's, never a piece's.
        val layoutDirection = LocalLayoutDirection.current
        val glyphLeft = width - WindowInsets.safeDrawing.getRight(density, layoutDirection) - with(density) { GLYPH_ZONE.toPx() }
        val glyphBottom = WindowInsets.safeDrawing.getTop(density) + with(density) { GLYPH_ZONE.toPx() }
        var layout by remember(widthDp, heightDp) { mutableStateOf(LayoutStore.layout(context, widthDp, heightDp)) }
        var opacity by remember { mutableFloatStateOf(controls.onScreenOpacity) }
        var shown by remember { mutableStateOf(controls.onScreen) }
        var selected by remember { mutableStateOf<String?>(null) }
        // The piece a controller's d-pad has reached, before A selects it.
        var cursor by remember { mutableStateOf<String?>(null) }
        var menu by remember { mutableStateOf(false) }
        var draft by remember { mutableStateOf<Draft?>(null) }
        var hint by remember { mutableStateOf(true) }
        LaunchedEffect(Unit) {
            delay(4000)
            hint = false
        }
        val hintAlpha by animateFloatAsState(if (hint) 1f else 0f, tween(900), label = "hint")
        val root = remember { FocusRequester() }
        LaunchedEffect(menu, draft == null) { if (!menu && draft == null) runCatching { root.requestFocus() } }
        BackHandler {
            when {
                draft != null -> draft = null
                menu -> menu = false
                selected != null -> selected = null
                else -> onClose()
            }
        }

        Box(
            Modifier.fillMaxSize().focusRequester(root).focusable().onPreviewKeyEvent { event ->
                if (menu || draft != null || event.type != KeyEventType.KeyDown) return@onPreviewKeyEvent false
                fun walk(dx: Int, dy: Int) {
                    layout.next(cursor ?: selected, dx, dy, width, height)?.let { cursor = it }
                }
                when (event.key.nativeKeyCode) {
                    KeyEvent.KEYCODE_DPAD_UP -> walk(0, -1)
                    KeyEvent.KEYCODE_DPAD_DOWN -> walk(0, 1)
                    KeyEvent.KEYCODE_DPAD_LEFT -> walk(-1, 0)
                    KeyEvent.KEYCODE_DPAD_RIGHT -> walk(1, 0)
                    KeyEvent.KEYCODE_DPAD_CENTER, KeyEvent.KEYCODE_ENTER -> selected = cursor ?: selected
                    KeyEvent.KEYCODE_BUTTON_START -> {
                        selected = null
                        menu = true
                    }
                    else -> return@onPreviewKeyEvent false
                }
                true
            },
        ) {
            if (sample) SampleScreen()
            // Edit mode at a glance: the game a little darker, and the snap grid faintly.
            Canvas(Modifier.fillMaxSize()) {
                drawRect(Color.Black.copy(alpha = 0.3f))
                val step = 16.dp.toPx()
                var y = step
                while (y < size.height) {
                    var x = step
                    while (x < size.width) {
                        drawCircle(Palette.Text.copy(alpha = 0.08f), 1.dp.toPx(), Offset(x, y))
                        x += step
                    }
                    y += step
                }
            }

            layout.pieces.filter { !it.hidden }.forEach { piece ->
                key(piece.id) {
                    val area = piece.area(width, height)
                    Box(
                        Modifier.offset { IntOffset(area.left.roundToInt(), area.top.roundToInt()) }
                            .size(with(density) { area.width.toDp() }, with(density) { area.height.toDp() }),
                        contentAlignment = Alignment.Center,
                    ) {
                        PieceGraphic(
                            piece, with(density) { area.width.toDp() }, with(density) { area.height.toDp() },
                            alpha = pieceAlpha(opacity, piece),
                        )
                    }
                }
            }
            // Each piece's hit area dashed (hidden ones are not drawn: the menu shows them again),
            // the controller's cursor dashed in teal, the selected piece solid in teal with its
            // corner handle.
            Canvas(Modifier.fillMaxSize()) {
                val dash = PathEffect.dashPathEffect(floatArrayOf(4.dp.toPx(), 4.dp.toPx()))
                val corner = CornerRadius(6.dp.toPx())
                layout.pieces.filter { !it.hidden }.forEach { piece ->
                    val hit = piece.hitArea(width, height, minHit)
                    val chosen = piece.id == selected && draft == null
                    val color = if (chosen || piece.id == cursor) Palette.Teal else Palette.Text.copy(alpha = 0.35f)
                    drawRoundRect(
                        color, Offset(hit.left, hit.top), Size(hit.width, hit.height), corner,
                        style = Stroke(if (chosen) 2.dp.toPx() else 1.dp.toPx(), pathEffect = if (chosen) null else dash),
                    )
                    if (chosen) {
                        drawCircle(Palette.Teal, HANDLE_DOT.toPx(), Offset(hit.right, hit.bottom))
                        drawCircle(Palette.Background, HANDLE_DOT.toPx() - 2.dp.toPx(), Offset(hit.right, hit.bottom))
                    }
                }
            }
            // Fingers: the selected piece's handle resizes it, a drag moves the piece under it at
            // once, two fingers pinch it, a tap selects it, a tap on nothing lets go.
            Box(
                Modifier.fillMaxSize().pointerInput(width, height, glyphLeft, glyphBottom) {
                    awaitEachGesture {
                        val down = awaitFirstDown()
                        if (down.position.x >= glyphLeft && down.position.y <= glyphBottom) return@awaitEachGesture
                        cursor = null
                        hint = false
                        val chosen = selected?.let { layout.piece(it) }?.takeIf { !it.hidden }
                        val handle = chosen?.hitArea(width, height, minHit)
                            ?.takeIf { hypot(down.position.x - it.right, down.position.y - it.bottom) <= minHit / 2 }
                        val start = if (handle != null) chosen else layout.hit(width, height, minHit, down.position.x, down.position.y)
                        if (start == null) {
                            selected = null
                            return@awaitEachGesture
                        }
                        down.consume()
                        val area = start.area(width, height)
                        val reach = hypot(down.position.x - area.centerX, down.position.y - area.centerY).coerceAtLeast(1f)
                        var cx = area.centerX
                        var cy = area.centerY
                        var scale = 1f
                        var travel = 0f
                        while (true) {
                            val event = awaitPointerEvent()
                            val finger = event.changes.firstOrNull { it.id == down.id }
                            if (event.changes.none { it.pressed }) break
                            if (handle != null) {
                                val at = finger?.position ?: continue
                                travel = maxOf(travel, hypot(at.x - down.position.x, at.y - down.position.y))
                                scale = hypot(at.x - area.centerX, at.y - area.centerY) / reach
                                layout = layout.replace(start.resized(scale, width, height, minSide))
                            } else {
                                scale *= event.calculateZoom()
                                val pan = event.calculatePan()
                                cx += pan.x
                                cy += pan.y
                                travel = maxOf(travel, hypot(cx - area.centerX, cy - area.centerY), if (scale != 1f) Float.MAX_VALUE else 0f)
                                if (travel > viewConfiguration.touchSlop) {
                                    layout = layout.replace(start.resized(scale, width, height, minSide).movedTo(cx, cy, width, height, grid, edge))
                                }
                            }
                            event.changes.forEach { it.consume() }
                        }
                        if (handle == null && travel <= viewConfiguration.touchSlop) selected = start.id
                    }
                },
            )

            // How to edit, in the middle the default leaves clear; it fades after a few seconds.
            if (hintAlpha > 0f) {
                Column(
                    Modifier.align(Alignment.Center).alpha(hintAlpha).clip(RowShape).background(Palette.Background.copy(alpha = 0.75f))
                        .padding(horizontal = 14.dp, vertical = 10.dp),
                    horizontalAlignment = Alignment.CenterHorizontally,
                ) {
                    Text(stringResource(R.string.editor_hint), style = Type.Hint, color = Palette.Text)
                    if (sample) Text(stringResource(R.string.editor_sample), style = Type.Hint, color = Palette.Muted, modifier = Modifier.padding(top = 4.dp))
                }
            }

            selected?.takeIf { draft == null }?.let { layout.piece(it) }?.takeIf { !it.hidden }?.let { piece ->
                ChipRow(
                    piece, piece.hitArea(width, height, minHit), width, height,
                    others = layout.pieces.filter { !it.hidden && it.id != piece.id }.map { it.hitArea(width, height, minHit) },
                    onPiece = { layout = layout.replace(it) },
                    onHide = {
                        layout = layout.replace(piece.copy(hidden = true))
                        selected = null
                    },
                    onDelete = {
                        layout = layout.without(piece.id)
                        selected = null
                    },
                    onRebind = { draft = Draft(piece.id, piece.binding, piece.label.orEmpty()) },
                )
            }

            if (!menu) {
                MenuGlyph(Modifier.align(Alignment.TopEnd).windowInsetsPadding(WindowInsets.safeDrawing)) {
                    selected = null
                    menu = true
                }
            } else {
                // Any tap outside the menu closes it.
                Box(Modifier.fillMaxSize().clickable(remember { MutableInteractionSource() }, null) { menu = false })
                EditorMenu(
                    layout, shown, opacity,
                    onShown = { shown = it },
                    onOpacity = { opacity = it },
                    onAdd = {
                        menu = false
                        draft = Draft(null, null, "")
                    },
                    onShow = { piece ->
                        layout = layout.replace(piece.copy(hidden = false))
                        selected = piece.id
                        menu = false
                    },
                    onReset = {
                        layout = ControlsLayout.default(widthDp.toFloat(), heightDp.toFloat())
                        opacity = Controls().onScreenOpacity
                        selected = null
                        menu = false
                    },
                    onCancel = onClose,
                    onDone = {
                        LayoutStore.save(context, widthDp, heightDp, layout)
                        ControlsStore.save(context, controls.copy(onScreen = shown, onScreenOpacity = opacity))
                        onClose()
                    },
                    onClose = { menu = false },
                    modifier = Modifier.align(Alignment.TopEnd).windowInsetsPadding(WindowInsets.safeDrawing).padding(6.dp)
                        .width(300.dp).heightIn(max = menuHeight),
                )
            }
            draft?.let { current ->
                CustomSheet(
                    current,
                    onChange = { draft = it },
                    onCancel = { draft = null },
                    onPlace = { action, label ->
                        val id = current.id
                        if (id == null) {
                            val (next, added) = layout.withCustom(action, label)
                            layout = next
                            selected = added
                        } else {
                            layout.piece(id)?.let { layout = layout.replace(it.copy(action = action.encode(), label = label.trim().ifEmpty { null })) }
                        }
                        draft = null
                    },
                )
            }
        }
    }
}

/** How far above the menu's foot its scrolled rows fade out. */
private val MENU_FADE = 24.dp

/** The menu glyph's corner (its 48 dp and 6 dp around it) that pieces never take a touch from. */
private val GLYPH_ZONE = 60.dp

/** The selected piece's resize handle, drawn this big at its hit area's bottom right corner. */
private val HANDLE_DOT = 9.dp

/** Room kept between a piece's hit area (and its handle) and its chip row. */
private val CHIP_GAP = 30.dp

/**
 * The selected piece's chips, just outside it and never over it or its handle: above it, else
 * below, else beside it, kept on screen, and over none of the [others] where it can be
 * ([chipRowAt]); it follows the piece as it moves. Opacity (a mini slider), then Hide for the
 * controller's pieces, or Rebind, Label and Delete for a custom button.
 */
@Composable
private fun ChipRow(
    piece: Piece,
    hit: Area,
    width: Float,
    height: Float,
    others: List<Area>,
    onPiece: (Piece) -> Unit,
    onHide: () -> Unit,
    onDelete: () -> Unit,
    onRebind: () -> Unit,
) {
    var measured by remember { mutableStateOf(IntSize.Zero) }
    val gap = with(LocalDensity.current) { CHIP_GAP.toPx() }
    val small = Type.Hint.copy(fontWeight = FontWeight.SemiBold)
    Row(
        Modifier
            .offset {
                val (left, top) = chipRowAt(hit, measured.width.toFloat(), measured.height.toFloat(), width, height, gap, others)
                IntOffset(left.roundToInt(), top.roundToInt())
            }
            .onSizeChanged { measured = it }
            .clip(RowShape).background(Palette.Surface.copy(alpha = 0.95f)).border(1.dp, Palette.Rule, RowShape)
            .padding(horizontal = 6.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(4.dp),
    ) {
        Text("◐", style = Type.Body, color = Palette.Muted, modifier = Modifier.padding(start = 6.dp))
        Text(
            stringResource(R.string.editor_opacity, (piece.opacity * 100).roundToInt()), style = Type.Hint, color = Palette.Text,
            maxLines = 1, modifier = Modifier.width(112.dp),
        )
        Slider(
            piece.opacity, { onPiece(piece.copy(opacity = (Math.round(it * 10) / 10f).coerceIn(Piece.OPACITIES))) },
            Modifier.width(110.dp), valueRange = Piece.OPACITIES, colors = sliderColors(),
        )
        if (piece.custom) {
            Chip("⌨", stringResource(R.string.editor_rebind), onRebind, small)
            Chip("Aa", stringResource(R.string.editor_label), onRebind, small)
            Chip("✕", stringResource(R.string.editor_delete), onDelete, small, danger = true)
        } else {
            Chip("◌", stringResource(R.string.editor_hide), onHide, small)
        }
    }
}

/** One chip: a small glyph and its word, a 48 dp target. */
@Composable
private fun Chip(glyph: String, label: String, onClick: () -> Unit, style: androidx.compose.ui.text.TextStyle, danger: Boolean = false) {
    Row(
        Modifier.heightIn(min = 48.dp).clip(RowShape).clickable(onClick = onClick).padding(horizontal = 10.dp)
            .semantics { contentDescription = label },
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(6.dp),
    ) {
        Text(glyph, style = style, color = if (danger) Palette.Red else Palette.Teal)
        Text(label, style = style, color = if (danger) Palette.Red else Palette.Text, maxLines = 1)
    }
}

@Composable
private fun sliderColors() = SliderDefaults.colors(
    thumbColor = Palette.Teal, activeTrackColor = Palette.Teal, inactiveTrackColor = Palette.Rule,
    activeTickColor = Color.Transparent, inactiveTickColor = Color.Transparent,
)

/** The on-screen controls' Show choices ([Controls.onScreen]): label and what it means. */
private val SHOW_CHOICES = listOf(
    null to (R.string.editor_show_auto to R.string.editor_show_auto_body),
    true to (R.string.editor_show_always to R.string.editor_show_always_body),
    false to (R.string.editor_show_never to R.string.editor_show_never_body),
)

/**
 * The editor's one menu ("On-screen layout"), from the glyph and in its corner like the game's menu: first the
 * controls as a whole (when they show, with what that means, and their opacity), then Add
 * button, the hidden pieces to show again, Reset to default, then Cancel and Done at the foot,
 * which never scrolls away: the reset question with Keep and Reset takes their place.
 */
@Composable
private fun EditorMenu(
    layout: ControlsLayout,
    shown: Boolean?,
    opacity: Float,
    onShown: (Boolean?) -> Unit,
    onOpacity: (Float) -> Unit,
    onAdd: () -> Unit,
    onShow: (Piece) -> Unit,
    onReset: () -> Unit,
    onCancel: () -> Unit,
    onDone: () -> Unit,
    onClose: () -> Unit,
    modifier: Modifier,
) {
    var confirm by remember { mutableStateOf(false) }
    // Keep leaves the question: the focus goes back to Reset to default, where it was.
    val resetRow = remember { FocusRequester() }
    var kept by remember { mutableStateOf(0) }
    LaunchedEffect(kept) { if (kept > 0) runCatching { resetRow.requestFocus() } }
    val first = remember { FocusRequester() }
    InitialFocus(first)
    val small = Type.Hint.copy(fontWeight = FontWeight.SemiBold)
    Panel(modifier, padding = 10.dp, alpha = 1f) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text(stringResource(R.string.editor_title), style = Type.Body.copy(fontWeight = FontWeight.SemiBold), color = Palette.Text, modifier = Modifier.weight(1f).padding(start = 6.dp))
            IconTarget(R.drawable.ic_close, stringResource(R.string.game_menu_close), onClose)
        }
        val menuScroll = rememberScrollState()
        Box(Modifier.weight(1f, fill = false)) {
            Column(Modifier.verticalScroll(menuScroll)) {
                Column(Modifier.padding(horizontal = 6.dp, vertical = 4.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
                    Text(stringResource(R.string.editor_show), style = Type.Hint, color = Palette.Muted, modifier = Modifier.padding(start = 2.dp))
                    Segmented(
                        SHOW_CHOICES.map { stringResource(it.second.first) }, SHOW_CHOICES.indexOfFirst { it.first == shown },
                        { onShown(SHOW_CHOICES[it].first) }, Modifier.fillMaxWidth(),
                    )
                    Text(
                        stringResource(SHOW_CHOICES.first { it.first == shown }.second.second), style = Type.Hint, color = Palette.Muted,
                        modifier = Modifier.padding(start = 2.dp),
                    )
                }
                Row(Modifier.padding(horizontal = 8.dp), verticalAlignment = Alignment.CenterVertically) {
                    Text(stringResource(R.string.editor_opacity, (opacity * 100).roundToInt()), style = Type.Body, color = Palette.Text, maxLines = 1, modifier = Modifier.width(112.dp))
                    Slider(
                        opacity, { onOpacity(Controls.stepped(it, Controls.OPACITY_STEP, 0, Controls.OPACITIES)) }, Modifier.weight(1f),
                        valueRange = Controls.OPACITIES, colors = sliderColors(),
                    )
                }
                Rule(Modifier.padding(vertical = 4.dp))
                FocusRow(onAdd, focusRequester = first, minHeight = 48.dp) {
                    Text(stringResource(R.string.editor_add), style = Type.Body, color = Palette.Text, modifier = Modifier.weight(1f))
                    Text("+", style = Type.Title, color = Palette.Teal)
                }
                val hidden = layout.pieces.filter { it.hidden }
                if (hidden.isNotEmpty()) {
                    Text(stringResource(R.string.editor_show_hidden), style = Type.Hint, color = Palette.Muted, modifier = Modifier.padding(horizontal = 14.dp))
                    Row(Modifier.horizontalScroll(rememberScrollState()).padding(horizontal = 8.dp, vertical = 4.dp), horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                        hidden.forEach { piece -> LauncherButton(pieceLabel(piece), { onShow(piece) }, height = 48.dp, padding = 14.dp, style = small) }
                    }
                }
                Rule(Modifier.padding(vertical = 4.dp))
                FocusRow({ confirm = true }, focusRequester = resetRow, minHeight = 48.dp) {
                    Text(stringResource(R.string.editor_reset_default), style = Type.Body, color = if (confirm) Palette.Faint else Palette.Text)
                }
            }
            // More below: the rows fade out above the foot rather than end cut through.
            if (menuScroll.canScrollForward) {
                Box(
                    Modifier.align(Alignment.BottomCenter).fillMaxWidth().height(MENU_FADE)
                        .background(Brush.verticalGradient(listOf(Palette.Surface.copy(alpha = 0f), Palette.Surface))),
                )
            }
        }
        Spacer(Modifier.heightIn(min = 6.dp))
        // The foot is never scrolled away: Cancel and Done, or the reset question with Keep (focused) and Reset in their place.
        if (confirm) {
            val keep = remember { FocusRequester() }
            InitialFocus(keep)
            Column(Modifier.padding(horizontal = 6.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
                Text(stringResource(R.string.editor_reset_confirm), style = Type.Hint, color = Palette.Text, modifier = Modifier.padding(horizontal = 2.dp))
                Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                    LauncherButton(stringResource(R.string.editor_keep), { confirm = false; kept++ }, Modifier.weight(1f), focusRequester = keep, height = 48.dp, padding = 8.dp, style = small)
                    LauncherButton(stringResource(R.string.editor_reset), onReset, Modifier.weight(1f), Kind.DANGER, height = 48.dp, padding = 8.dp, style = small)
                }
            }
        } else {
            Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                LauncherButton(stringResource(R.string.editor_cancel), onCancel, Modifier.weight(1f), Kind.QUIET, height = 48.dp, padding = 8.dp, style = small)
                LauncherButton(stringResource(R.string.editor_done), onDone, Modifier.weight(1f), Kind.PRIMARY, height = 48.dp, padding = 8.dp, style = small)
            }
        }
    }
}

/**
 * A custom button's key and label: a sheet from the right over the dimmed editor, its actions in
 * the header. One 16 dp gutter runs through it: the title, the label field, the caps and the
 * group names start on it, Save and the preview end on it. The label is optional (the key's
 * cap stands in, as the preview shows); under it, what the button will do (or a prompt) and
 * Any key, which opens the [AnyKeyPanel]. Then the quick picks ([QuickPicks]): the clicks and
 * Diablo II's keys. Place or Save once a key or click is chosen.
 */
@Composable
private fun BoxScope.CustomSheet(draft: Draft, onChange: (Draft) -> Unit, onCancel: () -> Unit, onPlace: (PadAction, String) -> Unit) {
    var keyboard by remember { mutableStateOf(false) }
    BackHandler(enabled = keyboard) { keyboard = false }
    Box(
        Modifier.matchParentSize().background(Color.Black.copy(alpha = 0.5f))
            .clickable(remember { MutableInteractionSource() }, null) { if (keyboard) keyboard = false else onCancel() },
    )
    if (keyboard) {
        AnyKeyPanel(
            draft.action,
            onPick = {
                onChange(draft.copy(action = it))
                keyboard = false
            },
            onBack = { keyboard = false },
            modifier = Modifier.align(Alignment.Center).windowInsetsPadding(WindowInsets.safeDrawing).padding(8.dp)
                .widthIn(max = 800.dp).fillMaxWidth().fillMaxHeight(),
        )
        return
    }
    val first = remember { FocusRequester() }
    Column(
        Modifier.align(Alignment.CenterEnd).windowInsetsPadding(WindowInsets.safeDrawing).padding(8.dp).width(SHEET_WIDTH).fillMaxHeight()
            .clip(RowShape).background(Palette.Surface).border(1.dp, Palette.Rule, RowShape),
    ) {
        Row(
            Modifier.fillMaxWidth().padding(start = SHEET_GUTTER, end = SHEET_GUTTER, top = 6.dp, bottom = 6.dp),
            verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(4.dp),
        ) {
            Text(
                stringResource(if (draft.id == null) R.string.editor_new_title else R.string.editor_edit_title),
                style = Type.Button, color = Palette.Text, maxLines = 1, overflow = TextOverflow.Ellipsis, modifier = Modifier.weight(1f),
            )
            LauncherButton(stringResource(R.string.editor_cancel), onCancel, kind = Kind.QUIET, height = 48.dp, padding = 14.dp)
            LauncherButton(
                stringResource(if (draft.id == null) R.string.editor_place else R.string.editor_save),
                { draft.action?.let { onPlace(it, draft.label) } }, kind = Kind.PRIMARY, enabled = draft.action != null, height = 48.dp, padding = 20.dp,
            )
        }
        // The label beside how the button will look, both 48 dp and on one centre line.
        val labelName = stringResource(R.string.editor_label)
        Row(Modifier.fillMaxWidth().padding(horizontal = SHEET_GUTTER), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(12.dp)) {
            Box(
                Modifier.weight(1f).height(48.dp).clip(Small).background(Palette.Background)
                    .border(1.dp, Palette.Rule, Small).padding(horizontal = 12.dp),
                contentAlignment = Alignment.CenterStart,
            ) {
                if (draft.label.isEmpty()) {
                    Text(stringResource(R.string.editor_label_hint), style = Type.Body, color = Palette.Muted, maxLines = 1, overflow = TextOverflow.Ellipsis)
                }
                BasicTextField(
                    draft.label,
                    { onChange(draft.copy(label = it.take(LABEL_MAX))) },
                    textStyle = Type.Body.copy(color = Palette.Text),
                    cursorBrush = SolidColor(Palette.Teal),
                    singleLine = true,
                    keyboardOptions = KeyboardOptions(capitalization = KeyboardCapitalization.Characters, imeAction = ImeAction.Done),
                    modifier = Modifier.fillMaxWidth().semantics { contentDescription = labelName },
                )
            }
            val action = draft.action
            if (action != null) {
                PieceGraphic(Piece("", 0f, 0f, 0f, label = draft.label.ifEmpty { null }, action = action.encode()), 48.dp, 48.dp)
            } else {
                // Where the preview will be: the key's outline, dashed and empty.
                Canvas(Modifier.size(48.dp)) {
                    val inset = 0.5.dp.toPx()
                    drawRoundRect(
                        Palette.Faint, Offset(inset, inset), Size(size.width - inset * 2, size.height - inset * 2), CornerRadius(16.dp.toPx()),
                        style = Stroke(1.dp.toPx(), pathEffect = PathEffect.dashPathEffect(floatArrayOf(4.dp.toPx(), 4.dp.toPx()))),
                    )
                }
            }
        }
        // What the button does, and the way to any other key.
        Row(Modifier.fillMaxWidth().padding(start = SHEET_GUTTER, end = 6.dp, top = 2.dp), verticalAlignment = Alignment.CenterVertically) {
            Text(
                draft.action?.let { actionLine(it) } ?: stringResource(R.string.editor_choose),
                style = Type.Hint, color = Palette.Muted, maxLines = 1, overflow = TextOverflow.Ellipsis, modifier = Modifier.weight(1f),
            )
            TextLink(stringResource(R.string.editor_any_key), { keyboard = true })
        }
        Rule()
        Box(Modifier.weight(1f)) {
            val scroll = rememberScrollState()
            Column(Modifier.fillMaxSize().verticalScroll(scroll).padding(horizontal = SHEET_GUTTER - PICK_INSET)) {
                QuickPicks(draft.action, first) { onChange(draft.copy(action = it)) }
                Spacer(Modifier.height(16.dp))
            }
            // The list runs on below the sheet's edge: it fades out there rather than stopping on a cut row.
            if (scroll.canScrollForward) {
                Box(
                    Modifier.align(Alignment.BottomCenter).fillMaxWidth().height(28.dp)
                        .background(Brush.verticalGradient(listOf(Palette.Surface.copy(alpha = 0f), Palette.Surface))),
                )
            }
        }
    }
}

/** The Add/Edit button sheet's width: three columns of cap and name. */
private val SHEET_WIDTH = 432.dp

/** The Add/Edit button sheet's one inner margin. */
private val SHEET_GUTTER = 16.dp

/** A pick's own inset: its highlight reaches this far past the gutter, its cap starts on it. */
private val PICK_INSET = 4.dp

/** A pick's height: a dense list, still about a thumb. */
private val PICK_HEIGHT = 44.dp

/**
 * The quick picks: the three clicks in a row, then Diablo II's keys three to a row, cap and
 * name on one line; keys that share a name (the belt slots, the skills, the arrows) as one row
 * of caps before that name.
 */
@Composable
private fun QuickPicks(current: PadAction?, first: FocusRequester, onPick: (PadAction) -> Unit) {
    val clicks = customClicks
    val keys = keyChoices
    val named = keys.map { it to actionName(it) }
    val counts = named.groupingBy { it.second }.eachCount()
    val singles = named.filter { counts[it.second] == 1 }.map { it.first }
    val families = named.filter { counts[it.second]!! > 1 }.groupBy({ it.second }, { it.first })
    val focusOn = current?.takeIf { it in clicks || it in keys } ?: clicks.first()
    @Composable
    fun grid(actions: List<PadAction>) {
        Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
            actions.chunked(3).forEach { row ->
                Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(4.dp)) {
                    row.forEach { action ->
                        PickCell(choiceCap(action).orEmpty(), actionName(action), action == current, if (action == focusOn) first else null, Modifier.weight(1f)) { onPick(action) }
                    }
                    repeat(3 - row.size) { Spacer(Modifier.weight(1f)) }
                }
            }
        }
    }
    GroupLabel(stringResource(R.string.custom_group_mouse), Modifier.padding(start = PICK_INSET))
    grid(clicks)
    GroupLabel(stringResource(R.string.custom_group_game_keys), Modifier.padding(start = PICK_INSET))
    grid(singles)
    families.forEach { (name, actions) ->
        Row(Modifier.padding(top = 4.dp, start = PICK_INSET - 3.dp).height(PICK_HEIGHT), verticalAlignment = Alignment.CenterVertically) {
            actions.forEach { action ->
                PickCap(keyCap(action).orEmpty(), actionLine(action), action == current, if (action == focusOn) first else null) { onPick(action) }
            }
            Text(name, style = Type.Hint, color = Palette.Text, maxLines = 1, overflow = TextOverflow.Ellipsis, modifier = Modifier.padding(start = 8.dp))
        }
    }
}

/** A quick pick: its cap, then what it does, one line; teal when chosen, ringed when focused. */
@Composable
private fun PickCell(cap: String, name: String, selected: Boolean, focus: FocusRequester?, modifier: Modifier, onClick: () -> Unit) {
    val interaction = remember { MutableInteractionSource() }
    val focused by interaction.collectIsFocusedAsState()
    Row(
        modifier.focusOn(focus)
            .height(PICK_HEIGHT).focusMark(focused, selected, Small)
            .selectable(selected, interaction, LocalIndication.current, role = Role.RadioButton, onClick = onClick)
            .padding(horizontal = PICK_INSET),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(6.dp),
    ) {
        Cap(cap, selected, Modifier.width(42.dp))
        // A long name ("Swap weapons") takes a second line rather than losing its end.
        Text(
            name, style = Type.Hint.copy(lineHeight = 15.sp), color = if (selected) Palette.Teal else Palette.Text,
            maxLines = 2, overflow = TextOverflow.Ellipsis,
        )
    }
}

/** A cap alone (a belt slot, a skill, an arrow): its row's name follows the caps. */
@Composable
private fun PickCap(cap: String, description: String, selected: Boolean, focus: FocusRequester?, onClick: () -> Unit) {
    val interaction = remember { MutableInteractionSource() }
    val focused by interaction.collectIsFocusedAsState()
    Box(
        Modifier.focusOn(focus)
            .size(44.dp, PICK_HEIGHT).clip(Small)
            .border(if (focused) 2.dp else 0.dp, if (focused) Palette.Teal else Color.Transparent, Small)
            .selectable(selected, interaction, LocalIndication.current, role = Role.RadioButton, onClick = onClick)
            .semantics { contentDescription = description },
        contentAlignment = Alignment.Center,
    ) {
        Cap(cap, selected, Modifier.width(38.dp))
    }
}

/** A key cap: raised, its letters bold; filled teal when chosen. */
@Composable
private fun Cap(text: String, selected: Boolean, modifier: Modifier) {
    Box(
        modifier.height(30.dp).clip(Small).background(if (selected) Palette.Teal else Palette.Raised)
            .border(1.dp, if (selected) Palette.Teal else Palette.Rule, Small),
        contentAlignment = Alignment.Center,
    ) { Text(text, style = Type.Glyph, color = if (selected) Palette.OnTeal else Palette.Text, maxLines = 1, softWrap = false) }
}

/** One key of the [AnyKeyPanel]: its keysym (0 for a gap) and its width in key units. */
private class BoardKey(val keysym: Int, val units: Float = 1f)

private fun keys(text: String) = text.map { BoardKey(it.code) }

/**
 * The keyboard the [AnyKeyPanel] draws, row by row as on a real one, every row 15 units wide (the
 * top one 13 keys): Esc and the function keys; the number row; the three letter rows, the up
 * arrow over the down one; then Ctrl, Alt, Space, the editing keys and the other arrows.
 */
private val BOARD: List<List<BoardKey>> = listOf(
    listOf(BoardKey(Keys.XK_ESCAPE)) + (0 until 12).map { BoardKey(Keys.XK_F1 + it) },
    keys("`1234567890-=") + BoardKey(Keys.XK_BACKSPACE, 2f),
    listOf(BoardKey(Keys.XK_TAB, 1.5f)) + keys("qwertyuiop[]") + BoardKey('\\'.code, 1.5f),
    listOf(BoardKey(0, 1.75f)) + keys("asdfghjkl;'") + BoardKey(Keys.XK_RETURN, 2.25f),
    listOf(BoardKey(Keys.XK_SHIFT_L, 2.25f)) + keys("zxcvbnm,./") + listOf(BoardKey(0, 0.75f), BoardKey(Keys.XK_UP), BoardKey(0, 1f)),
    listOf(BoardKey(Keys.XK_CONTROL_L, 1.5f), BoardKey(Keys.XK_ALT_L, 1.5f), BoardKey(' '.code, 3f)) +
        listOf(Keys.XK_INSERT, Keys.XK_DELETE, Keys.XK_HOME, Keys.XK_END, Keys.XK_PAGE_UP, Keys.XK_PAGE_DOWN, Keys.XK_LEFT, Keys.XK_DOWN, Keys.XK_RIGHT)
            .map { BoardKey(it) },
)

/**
 * Any key, over the whole editor: a keyboard laid out as a real one, a tap binds that key; a key
 * pressed on an attached keyboard binds it too. Back returns to the sheet unchanged.
 */
@Composable
private fun AnyKeyPanel(current: PadAction?, onPick: (PadAction) -> Unit, onBack: () -> Unit, modifier: Modifier) {
    val first = remember { FocusRequester() }
    val chosen = (current as? PadAction.Key)?.keysym
    val focusOn = chosen?.takeIf { sym -> BOARD.any { row -> row.any { it.keysym == sym } } } ?: Keys.XK_ESCAPE
    // The panel holds focus itself (a touch screen gives none to its keys) so an attached
    // keyboard's presses reach it; a controller's d-pad then moves onto the keys.
    val panel = remember { FocusRequester() }
    LaunchedEffect(Unit) {
        runCatching { panel.requestFocus() }
        runCatching { first.requestFocus() }
    }
    Column(
        modifier.clip(RowShape).background(Palette.Surface).border(1.dp, Palette.Rule, RowShape)
            .focusRequester(panel).focusable()
            .onPreviewKeyEvent { event ->
                val native = event.nativeKeyEvent
                // Back here, as the panel holds focus: Compose would spend it on that focus.
                if (native.keyCode == KeyEvent.KEYCODE_BACK) {
                    if (event.type == KeyEventType.KeyUp) onBack()
                    return@onPreviewKeyEvent true
                }
                if (event.type != KeyEventType.KeyDown || native.device?.keyboardType != InputDevice.KEYBOARD_TYPE_ALPHABETIC) return@onPreviewKeyEvent false
                val sym = pickedKeysym(native.keyCode) ?: return@onPreviewKeyEvent false
                onPick(PadAction.Key(sym))
                true
            },
    ) {
        Row(
            Modifier.fillMaxWidth().padding(start = SHEET_GUTTER, end = SHEET_GUTTER, top = 6.dp, bottom = 6.dp),
            verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Text(stringResource(R.string.editor_any_key_title), style = Type.Button, color = Palette.Text, maxLines = 1)
            Text(
                stringResource(R.string.editor_any_key_hint), style = Type.Hint, color = Palette.Muted, maxLines = 1,
                overflow = TextOverflow.Ellipsis, modifier = Modifier.weight(1f),
            )
            LauncherButton(stringResource(R.string.action_back), onBack, kind = Kind.QUIET, height = 48.dp, padding = 14.dp)
        }
        BoxWithConstraints(Modifier.weight(1f).fillMaxWidth().padding(start = SHEET_GUTTER, end = SHEET_GUTTER, bottom = 12.dp)) {
            val gap = 4.dp
            val rowHeight = ((maxHeight - gap * (BOARD.size - 1)) / BOARD.size).coerceIn(34.dp, 52.dp)
            Column(Modifier.align(Alignment.Center).fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(gap)) {
                BOARD.forEach { row ->
                    Row(Modifier.fillMaxWidth().height(rowHeight), horizontalArrangement = Arrangement.spacedBy(gap)) {
                        row.forEach { key ->
                            if (key.keysym == 0) {
                                Spacer(Modifier.weight(key.units))
                            } else {
                                val action = PadAction.Key(key.keysym)
                                BoardButton(
                                    keyCap(action).orEmpty(), actionLine(action), key.keysym == chosen,
                                    if (key.keysym == focusOn) first else null, Modifier.weight(key.units).fillMaxHeight(),
                                ) { onPick(action) }
                            }
                        }
                    }
                }
            }
        }
    }
}

/** A key on the [AnyKeyPanel]'s keyboard: raised, filled teal when it is the button's key. */
@Composable
private fun BoardButton(cap: String, description: String, selected: Boolean, focus: FocusRequester?, modifier: Modifier, onClick: () -> Unit) {
    val interaction = remember { MutableInteractionSource() }
    val focused by interaction.collectIsFocusedAsState()
    Box(
        modifier.focusOn(focus)
            .clip(Small).background(if (selected) Palette.Teal else Palette.Raised)
            .border(if (focused) 2.dp else 1.dp, if (focused) Palette.Text else if (selected) Palette.Teal else Palette.Rule, Small)
            .clickable(interaction, LocalIndication.current, role = Role.Button, onClick = onClick)
            .semantics { contentDescription = description },
        contentAlignment = Alignment.Center,
    ) {
        Text(cap, style = Type.Glyph.copy(fontSize = 13.sp), color = if (selected) Palette.OnTeal else Palette.Text, maxLines = 1, softWrap = false)
    }
}

/**
 * The sample screen behind the editor when the game is not running: the launcher's lattice and,
 * outlined, where Diablo II's own controls lie at the foot of its picture (the life and mana
 * globes and the bar between them), so pieces can be kept clear of them.
 */
@Composable
private fun SampleScreen() {
    Lattice(Modifier.fillMaxSize())
    Canvas(Modifier.fillMaxSize()) {
        // The game's picture: at most 16:9, centred.
        val pictureWidth = minOf(size.width, size.height * 16f / 9f)
        val left = (size.width - pictureWidth) / 2
        val globe = size.height * 0.11f
        val stroke = Stroke(1.dp.toPx())
        drawRect(Palette.Rule, Offset(left, 0f), Size(pictureWidth, size.height), style = stroke)
        drawCircle(Palette.Red.copy(alpha = 0.5f), globe, Offset(left + pictureWidth * 0.08f, size.height - globe * 1.1f), style = stroke)
        drawCircle(Palette.Teal.copy(alpha = 0.5f), globe, Offset(left + pictureWidth * 0.92f, size.height - globe * 1.1f), style = stroke)
        drawRoundRect(
            Palette.Faint, Offset(left + pictureWidth * 0.2f, size.height - globe * 0.9f), Size(pictureWidth * 0.6f, globe * 0.8f),
            CornerRadius(4.dp.toPx()), style = stroke,
        )
    }
}
