package com.pd2launcher.app.ui

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.gestures.awaitEachGesture
import androidx.compose.foundation.gestures.awaitFirstDown
import androidx.compose.foundation.gestures.waitForUpOrCancellation
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.draw.clip
import androidx.compose.ui.geometry.CornerRadius
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.Shadow
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.disabled
import androidx.compose.ui.semantics.onClick
import androidx.compose.ui.semantics.role
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.IntOffset
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.min
import androidx.compose.ui.unit.sp
import com.pd2launcher.app.R
import com.pd2launcher.app.input.Area
import com.pd2launcher.app.input.ControlsLayout
import com.pd2launcher.app.input.CustomButtons
import com.pd2launcher.app.input.GamepadLayout
import com.pd2launcher.app.input.PadButton
import com.pd2launcher.app.input.PadGroups
import com.pd2launcher.app.input.Piece
import com.pd2launcher.app.input.PieceKind
import com.pd2launcher.app.input.VirtualPad
import kotlin.math.hypot
import kotlin.math.roundToInt
import kotlin.math.sqrt

/**
 * The on-screen controls in play, each piece where the player's [layout] puts it
 * ([com.pd2launcher.app.input.LayoutStore]), drawn at [opacity] times its own. The controller's
 * pieces write the same gamepad file as a real pad ([VirtualPad], merged by
 * [com.pd2launcher.app.input.PadInput.onVirtual]); custom buttons send their key or click
 * through the relay ([CustomButtons]). A finger presses a piece anywhere in its hit area
 * ([Piece.hitArea]: at least [MIN_TARGET] however small it is drawn); elsewhere it reaches the
 * game. They stay beside the in-game menu too ([GameLayer.padShown]), under it where it overlaps.
 * [inert]: the game is in the background for the menu ([GameLayer.padInert]): the pieces show
 * dimmed (the whole drawing at [INERT_DIM]) and a touch on one does nothing (it is taken, so it
 * neither reaches the game nor closes the menu), until the menu closes.
 */
@Composable
internal fun OnScreenControls(layout: ControlsLayout, opacity: Float, pad: VirtualPad, buttons: CustomButtons, inert: Boolean = false) {
    // Hidden (the Controls screen or the editor opened, or turned off): nothing stays held in the game.
    DisposableEffect(buttons, pad) {
        onDispose {
            buttons.releaseAll()
            pad.releaseAll()
        }
    }
    BoxWithConstraints(Modifier.fillMaxSize()) {
        val width = constraints.maxWidth.toFloat()
        val height = constraints.maxHeight.toFloat()
        val minHit = with(LocalDensity.current) { MIN_TARGET.toPx() }
        layout.pieces.forEach { piece ->
            if (!piece.hidden) {
                // The same piece in play and inert (only its input and its dim change): the menu opens and closes with no piece made again.
                key(piece.id) {
                    val drawn = piece.area(width, height)
                    val hit = piece.hitArea(width, height, minHit)
                    OnScreenPiece(piece, drawn, hit, pieceAlpha(opacity, piece), inert, pad, buttons)
                }
            }
        }
    }
}

/** The opacity of the whole drawing (outline, label and shadow alike) while the pieces are inert ([OnScreenControls]). */
internal const val INERT_DIM = 0.5f

/**
 * One piece: its hit area takes the finger, its drawing lights what is pressed. [inert]: drawn as
 * in play at [alpha], the whole drawing dimmed to [INERT_DIM]; it takes a touch ([takesTouches])
 * and does nothing. A change to or from inert starts its input again: the gesture in progress is
 * cut short, so what it held goes up ([thumb], [press]).
 */
@Composable
private fun OnScreenPiece(piece: Piece, drawn: Area, hit: Area, alpha: Float, inert: Boolean, pad: VirtualPad, buttons: CustomButtons) {
    var lit by remember { mutableIntStateOf(0) }
    var dot by remember { mutableStateOf<Offset?>(null) }
    val radius = drawn.width / 2
    val kind = piece.kind
    // The release at a change to or from inert needs a new start of the piece's pointer input: the
    // inert input's key (Unit, takesTouches) differs from the play input's (piece; piece and radius).
    val input = if (inert) Modifier.takesTouches() else when (kind) {
        PieceKind.STICK -> Modifier.thumb(piece, radius, follow = { x, y ->
            // The dot stays inside the ring.
            val rim = maxOf(1f, hypot(x, y))
            dot = Offset(x / rim, y / rim)
            pad.stick(x, y)
        }, lift = {
            dot = null
            pad.stick(0f, 0f)
        })
        PieceKind.DPAD -> Modifier.thumb(piece, radius, follow = { x, y -> lit = dpadTo(pad, lit, PadGroups.dpad(x, y)) }, lift = { lit = dpadTo(pad, lit, 0) })
        PieceKind.FACE -> Modifier.thumb(piece, radius, follow = { x, y -> lit = faceTo(pad, lit, PadGroups.face(x, y)) }, lift = { lit = faceTo(pad, lit, 0) })
        PieceKind.LT, PieceKind.RT -> Modifier.press(piece, {
            pad.trigger(kind == PieceKind.LT, true)
            lit = 1
        }, {
            pad.trigger(kind == PieceKind.LT, false)
            lit = 0
        })
        null -> piece.binding?.let { action ->
            Modifier.press(piece, {
                buttons.press(piece.id, action)
                lit = 1
            }, {
                buttons.release(piece.id)
                lit = 0
            })
        } ?: Modifier
        else -> Modifier.press(piece, {
            pad.button(kind.bit, true)
            lit = 1
        }, {
            pad.button(kind.bit, false)
            lit = 0
        })
    }
    val description = pieceDescription(piece)
    val density = LocalDensity.current
    Box(
        Modifier
            .offset { IntOffset(hit.left.roundToInt(), hit.top.roundToInt()) }
            .size(with(density) { hit.width.toDp() }, with(density) { hit.height.toDp() })
            // Not merging: Android then reports an inert piece as not enabled.
            .semantics {
                contentDescription = description
                if (kind != PieceKind.STICK && kind != PieceKind.DPAD && kind != PieceKind.FACE) role = Role.Button
                if (inert) disabled()
            }
            .then(input),
        contentAlignment = Alignment.Center,
    ) {
        PieceGraphic(
            piece, with(density) { drawn.width.toDp() }, with(density) { drawn.height.toDp() }, lit, dot,
            modifier = if (inert) Modifier.alpha(INERT_DIM) else Modifier, alpha = alpha,
        )
    }
}

private val DPAD_BITS = listOf(GamepadLayout.DPAD_UP, GamepadLayout.DPAD_DOWN, GamepadLayout.DPAD_LEFT, GamepadLayout.DPAD_RIGHT)

/** The d-pad from [from] to [to] pressed (bits), each changed direction once. */
private fun dpadTo(pad: VirtualPad, from: Int, to: Int): Int {
    val changed = from xor to
    DPAD_BITS.filter { changed and it != 0 }.forEach { pad.button(it, to and it != 0) }
    return to
}

/** A/B/X/Y from button [from] to button [to] (0: none), so the finger may roll from one to the next. */
private fun faceTo(pad: VirtualPad, from: Int, to: Int): Int {
    if (from == to) return to
    if (from != 0) pad.button(from, false)
    if (to != 0) pad.button(to, true)
    return to
}

/**
 * One finger's way over a round piece of [radius] pixels: [follow] with its offset from the
 * centre in radii (down positive) on touch and every move, [lift] when it goes, also when the
 * gesture is cut short (the piece's layout changed, or the piece went to or from inert, and its
 * input started again). A finger still down after that moves nothing: only a new touch starts a gesture.
 */
private fun Modifier.thumb(piece: Piece, radius: Float, follow: (Float, Float) -> Unit, lift: () -> Unit): Modifier = pointerInput(piece, radius) {
    fun at(position: Offset) = follow((position.x - size.width / 2f) / radius, (position.y - size.height / 2f) / radius)
    awaitEachGesture {
        val down = awaitFirstDown()
        down.consume()
        try {
            at(down.position)
            while (true) {
                val change = awaitPointerEvent().changes.firstOrNull { it.id == down.id } ?: break
                if (!change.pressed) break
                change.consume()
                at(change.position)
            }
        } finally {
            lift()
        }
    }
}

/** A button's press while a finger is on it (no focus, no ripple); let go also when the gesture is cut short (as at a change to or from inert). */
private fun Modifier.press(piece: Piece, onPress: () -> Unit, onRelease: () -> Unit): Modifier =
    semantics { onClick { onPress(); onRelease(); true } }.pointerInput(piece) {
        awaitEachGesture {
            awaitFirstDown().consume()
            onPress()
            try {
                waitForUpOrCancellation()?.consume()
            } finally {
                onRelease()
            }
        }
    }

/**
 * What a piece shows: the controller's name for it, as the app names the buttons everywhere
 * ([PadButton.label]: L1, L2, R1, R2, L3, R3, Select, Start), or a custom button's label or key cap.
 */
@Composable
internal fun pieceLabel(piece: Piece): String = when (piece.kind) {
    PieceKind.STICK -> stringResource(R.string.piece_stick)
    PieceKind.DPAD -> stringResource(R.string.piece_dpad)
    PieceKind.FACE -> stringResource(R.string.piece_face)
    null -> piece.label ?: piece.binding?.let(::defaultLabel).orEmpty()
    else -> piece.kind?.let(::pieceButton)?.label.orEmpty()
}

/** The controller button an on-screen piece stands for; null for the stick, the d-pad and A/B/X/Y (several buttons each). */
internal fun pieceButton(kind: PieceKind): PadButton? = when (kind) {
    PieceKind.LT -> PadButton.L2
    PieceKind.LB -> PadButton.L1
    PieceKind.RB -> PadButton.R1
    PieceKind.RT -> PadButton.R2
    PieceKind.SELECT -> PadButton.SELECT
    PieceKind.START -> PadButton.START
    PieceKind.L3 -> PadButton.L3
    PieceKind.R3 -> PadButton.R3
    PieceKind.STICK, PieceKind.DPAD, PieceKind.FACE -> null
}

@Composable
private fun pieceDescription(piece: Piece): String = when (piece.kind) {
    PieceKind.STICK -> stringResource(R.string.on_screen_stick_description)
    PieceKind.DPAD -> stringResource(R.string.on_screen_dpad_description)
    PieceKind.FACE -> stringResource(R.string.on_screen_face_description)
    null -> stringResource(R.string.on_screen_button_description, piece.binding?.let { actionLine(it) } ?: pieceLabel(piece))
    else -> pieceLabel(piece)
}

/** The smallest touch target over the game, whatever the drawn size. */
internal val MIN_TARGET = 48.dp

/**
 * A piece's two inks at [alpha] (the layout's opacity times the piece's, [pieceAlpha]), from
 * [inkAlphas]: outline and arrows/labels alike follow it from faint to solid.
 */
private class Ink(alpha: Float) {
    private val alphas = inkAlphas(alpha)
    val line = Palette.Text.copy(alpha = alphas.line)
    val lineLit = Palette.Text.copy(alpha = alphas.lineLit)
    val glyph = Palette.Text.copy(alpha = alphas.glyph)
    val glyphLit = Palette.Text
    val knob = Palette.Text.copy(alpha = alphas.knob)
}

/** The opacity a [piece] is drawn at under the controls' [layoutOpacity], in game and in the editor alike. */
internal fun pieceAlpha(layoutOpacity: Float, piece: Piece): Float = (layoutOpacity * piece.opacity).coerceIn(0f, 1f)

/** The alphas of a piece's outline (at rest and pressed), arrows and labels, and stick knob. */
internal data class InkAlphas(val line: Float, val lineLit: Float, val glyph: Float, val knob: Float)

/**
 * A piece's alphas at [alpha] ([pieceAlpha]): the arrows and labels at its square root, the
 * outline at that times [alpha], so both fade with the piece's own opacity, from faint at its
 * least to fully solid at 100 %, and the glyphs always stand above the outline, readable over the
 * game (a 35 % layout's at about 60 % with a soft dark shadow, its outline at about 20 %).
 * Pressed, the outline brightens to the glyphs'.
 */
internal fun inkAlphas(alpha: Float): InkAlphas {
    val a = alpha.coerceIn(0f, 1f)
    val glyph = sqrt(a)
    return InkAlphas(line = glyph * a, lineLit = glyph, glyph = glyph, knob = 0.3f * a.coerceAtLeast(0.5f))
}

/** Lifts a label off the game picture behind it. */
private val GlyphShadow = Shadow(Color.Black.copy(alpha = 0.7f), Offset(0f, 1f), 4f)

/**
 * A piece's drawing at [width] x [height], thin outlines with small labels: the
 * stick's ring (and the finger's [dot]), the d-pad as a tight cross of four arms and A/B/X/Y as
 * a tight diamond (both nearly touching, so the group is mostly target), the rest as a rounded
 * key. [lit]: the pressed d-pad or face bits, or non-zero for a pressed key. [alpha]: its
 * opacity ([Ink]).
 */
@Composable
internal fun PieceGraphic(piece: Piece, width: Dp, height: Dp, lit: Int = 0, dot: Offset? = null, modifier: Modifier = Modifier, alpha: Float = 1f) {
    val ink = Ink(alpha)
    when (piece.kind) {
        PieceKind.STICK -> Stick(width, dot, ink, modifier)
        PieceKind.DPAD -> DPad(width, lit, ink, modifier)
        PieceKind.FACE -> FaceButtons(width, lit, ink, modifier)
        else -> Key(pieceLabel(piece), width, height, lit != 0, ink, modifier)
    }
}

@Composable
private fun Stick(diameter: Dp, dot: Offset?, ink: Ink, modifier: Modifier) {
    val knob = diameter * 0.16f
    Box(modifier.size(diameter).border(1.dp, ink.line, CircleShape), contentAlignment = Alignment.Center) {
        if (dot != null) {
            Box(
                Modifier
                    .offset {
                        val travel = (diameter - knob).toPx() / 2
                        IntOffset((dot.x * travel).roundToInt(), (dot.y * travel).roundToInt())
                    }
                    .size(knob)
                    .clip(CircleShape)
                    .background(ink.knob),
            )
        }
    }
}

/** The d-pad: four rounded arms in a cross of thirds, [PadGroups.GAP] apart; each brightens while pressed. */
@Composable
private fun DPad(side: Dp, bits: Int, ink: Ink, modifier: Modifier) {
    Canvas(modifier.size(side)) {
        val s = size.width
        val cell = s / 3f
        val gap = s * PadGroups.GAP
        val stroke = 1.dp.toPx()
        val corner = CornerRadius(cell * 0.2f)
        fun arm(bit: Int, column: Int, row: Int, tip: Offset, l: Offset, r: Offset) {
            val on = bits and bit != 0
            val color = if (on) ink.lineLit else ink.line
            val width = if (on) stroke * 2 else stroke
            val inset = gap / 2 + width / 2
            drawRoundRect(
                color, Offset(column * cell + inset, row * cell + inset), Size(cell - inset * 2, cell - inset * 2), corner,
                style = Stroke(width),
            )
            drawPath(Path().apply { moveTo(tip.x, tip.y); lineTo(l.x, l.y); lineTo(r.x, r.y); close() }, if (on) ink.glyphLit else ink.glyph)
        }
        val c = s / 2
        val t = cell * 0.16f
        val near = cell * 0.4f
        arm(GamepadLayout.DPAD_UP, 1, 0, Offset(c, near), Offset(c - t, near + t * 1.4f), Offset(c + t, near + t * 1.4f))
        arm(GamepadLayout.DPAD_DOWN, 1, 2, Offset(c, s - near), Offset(c - t, s - near - t * 1.4f), Offset(c + t, s - near - t * 1.4f))
        arm(GamepadLayout.DPAD_LEFT, 0, 1, Offset(near, c), Offset(near + t * 1.4f, c - t), Offset(near + t * 1.4f, c + t))
        arm(GamepadLayout.DPAD_RIGHT, 2, 1, Offset(s - near, c), Offset(s - near - t * 1.4f, c - t), Offset(s - near - t * 1.4f, c + t))
    }
}

/** A/B/X/Y as on an Xbox pad (Y top, X left, B right, A bottom), a tight diamond of large circles. */
@Composable
private fun FaceButtons(side: Dp, held: Int, ink: Ink, modifier: Modifier) {
    val button = side * PadGroups.FACE_DIAMETER
    val reach = side * PadGroups.FACE_REACH
    Box(modifier.size(side), contentAlignment = Alignment.Center) {
        listOf(
            Triple(GamepadLayout.Y, "Y", 0 to -1),
            Triple(GamepadLayout.X, "X", -1 to 0),
            Triple(GamepadLayout.B, "B", 1 to 0),
            Triple(GamepadLayout.A, "A", 0 to 1),
        ).forEach { (bit, label, at) ->
            val on = held == bit
            Box(
                Modifier.offset(reach * at.first, reach * at.second).size(button)
                    .border(if (on) 2.dp else 1.dp, if (on) ink.lineLit else ink.line, CircleShape),
                contentAlignment = Alignment.Center,
            ) {
                Text(
                    label, style = Type.Button.copy(fontSize = (button.value * 0.38f).coerceIn(12f, 24f).sp, shadow = GlyphShadow),
                    color = if (on) ink.glyphLit else ink.glyph, maxLines = 1,
                )
            }
        }
    }
}

/** A key: a thin rounded outline with its label, shrunk to fit the width. */
@Composable
private fun Key(label: String, width: Dp, height: Dp, lit: Boolean, ink: Ink, modifier: Modifier) {
    val short = min(width, height)
    // About 0.6 em a character: a long label shrinks rather than clips.
    val fit = (width.value - 6f) / (label.length.coerceAtLeast(1) * 0.62f)
    val fontSize = minOf((short.value * 0.36f).coerceIn(10f, 18f), fit.coerceAtLeast(8f))
    Box(
        modifier.size(width, height).border(if (lit) 2.dp else 1.dp, if (lit) ink.lineLit else ink.line, RoundedCornerShape(short / 3)),
        contentAlignment = Alignment.Center,
    ) {
        Text(
            label,
            style = Type.Button.copy(fontSize = fontSize.sp, shadow = GlyphShadow),
            color = if (lit) ink.glyphLit else ink.glyph,
            textAlign = TextAlign.Center,
            maxLines = 1,
            softWrap = false,
        )
    }
}
