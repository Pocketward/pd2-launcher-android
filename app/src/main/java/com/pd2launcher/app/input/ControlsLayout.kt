package com.pd2launcher.app.input

import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import kotlin.math.abs
import kotlin.math.max
import kotlin.math.min
import kotlin.math.roundToInt

/**
 * The controller's own on-screen pieces: the left stick, the d-pad and A/B/X/Y (one piece
 * each), the shoulders and triggers, Back (Select) and Start, and L3/R3 (hidden by default).
 * [aspect] is a piece's width over its height.
 */
enum class PieceKind(val id: String, val aspect: Float = 1f, val bit: Int = 0) {
    STICK("stick"),
    DPAD("dpad"),
    FACE("face"),
    LT("lt", WIDE),
    LB("lb", WIDE, GamepadLayout.LEFT_SHOULDER),
    RB("rb", WIDE, GamepadLayout.RIGHT_SHOULDER),
    RT("rt", WIDE),
    SELECT("select", WIDE, GamepadLayout.BACK),
    START("start", WIDE, GamepadLayout.START),
    L3("l3", bit = GamepadLayout.LEFT_THUMB),
    R3("r3", bit = GamepadLayout.RIGHT_THUMB),
    ;

    companion object {
        private val byId = entries.associateBy { it.id }

        fun of(id: String): PieceKind? = byId[id]
    }
}

/** The shoulders', triggers' and Back/Start's width over height. */
private const val WIDE = 1.5f

/** A rectangle on the screen, in pixels. */
data class Area(val left: Float, val top: Float, val right: Float, val bottom: Float) {
    val width get() = right - left
    val height get() = bottom - top
    val centerX get() = (left + right) / 2
    val centerY get() = (top + bottom) / 2

    fun contains(x: Float, y: Float) = x in left..right && y in top..bottom

    /** This area grown about its centre to at least [min] each way. */
    fun atLeast(min: Float): Area {
        val w = max(width, min) / 2
        val h = max(height, min) / 2
        return Area(centerX - w, centerY - h, centerX + w, centerY + h)
    }

    fun intersects(other: Area) = left < other.right && other.left < right && top < other.bottom && other.top < bottom

    /** How much of this area [other] covers. */
    fun overlap(other: Area): Float =
        max(0f, min(right, other.right) - max(left, other.left)) * max(0f, min(bottom, other.bottom) - max(top, other.top))
}

/**
 * Where the editor's [w] x [h] chip row goes beside the selected piece's [hit] area on a [width] x
 * [height] screen, [gap] clear of it and never over it: above it, below it, right or left of it
 * (above and below centred on it, then flush with its right or left side; beside it centred,
 * then flush with its top or bottom), kept on screen. The first that covers none of [others]
 * (the other pieces) wins; when each covers some, the one covering least. Its top left corner.
 */
fun chipRowAt(hit: Area, w: Float, h: Float, width: Float, height: Float, gap: Float, others: List<Area>): Pair<Float, Float> {
    fun x(at: Float) = at.coerceIn(0f, (width - w).coerceAtLeast(0f))
    fun y(at: Float) = at.coerceIn(0f, (height - h).coerceAtLeast(0f))
    val across = listOf(hit.centerX - w / 2, hit.right - w, hit.left)
    val along = listOf(hit.centerY - h / 2, hit.top, hit.bottom - h)
    val candidates = buildList {
        val above = hit.top - gap - h
        if (above >= 0f) across.forEach { add(x(it) to above) }
        val below = hit.bottom + gap
        if (below + h <= height) across.forEach { add(x(it) to below) }
        val right = hit.right + gap
        if (right + w <= width) along.forEach { add(right to y(it)) }
        val left = hit.left - gap - w
        if (left >= 0f) along.forEach { add(left to y(it)) }
    }
    if (candidates.isEmpty()) return (hit.left - gap - w).coerceAtLeast(0f) to y(hit.centerY - h / 2)
    return candidates.minBy { (left, top) ->
        val row = Area(left, top, left + w, top + h)
        others.sumOf { row.overlap(it).toDouble() }
    }
}

/**
 * One on-screen piece where the player put it, in coordinates normalised to the screen, so a
 * layout scales with it: [x], [y] its centre as fractions of the width and height, [size] its
 * height as a fraction of the shorter side (its width is that times the kind's aspect).
 * [opacity] is a factor of the controls' own. A custom button ([id] starting with
 * [CUSTOM_PREFIX]) carries its [action] ([PadAction.encode]: a key or a mouse button) and an
 * optional short [label].
 */
@Serializable
data class Piece(
    val id: String,
    val x: Float,
    val y: Float,
    val size: Float,
    val hidden: Boolean = false,
    val opacity: Float = 1f,
    val action: String? = null,
    val label: String? = null,
) {
    val kind: PieceKind? get() = PieceKind.of(id)
    val custom: Boolean get() = id.startsWith(CUSTOM_PREFIX)

    /** A custom button's key or mouse button; null for the controller's pieces. */
    val binding: PadAction? get() = action?.let(PadAction::decode)

    private val aspect get() = kind?.aspect ?: 1f

    /** Where the piece is drawn on a [width] x [height] screen. */
    fun area(width: Float, height: Float): Area {
        val h = size * min(width, height)
        val w = h * aspect
        val cx = x * width
        val cy = y * height
        return Area(cx - w / 2, cy - h / 2, cx + w / 2, cy + h / 2)
    }

    /** Where a finger presses it: its area, at least [minHit] each way however small it is drawn. */
    fun hitArea(width: Float, height: Float, minHit: Float): Area = area(width, height).atLeast(minHit)

    /**
     * The piece with its centre at [cx], [cy] (pixels on a [width] x [height] screen): on a
     * [grid], its sides drawn to an [edge] margin from the screen's sides when they come within a
     * grid step of it, its centre to the screen's middle likewise, and wholly on the screen.
     */
    fun movedTo(cx: Float, cy: Float, width: Float, height: Float, grid: Float, edge: Float): Piece {
        val now = area(width, height)
        val hw = now.width / 2
        val hh = now.height / 2
        fun snap(c: Float, half: Float, length: Float): Float {
            var at = if (grid > 0) (c / grid).roundToInt() * grid else c
            when {
                abs(c - half - edge) < grid -> at = edge + half
                abs(c + half - (length - edge)) < grid -> at = length - edge - half
                abs(c - length / 2) < grid -> at = length / 2
            }
            return at.coerceIn(min(half, length / 2), max(length - half, length / 2))
        }
        return copy(x = snap(cx, hw, width) / width, y = snap(cy, hh, height) / height)
    }

    /**
     * The piece [factor] times its size about its centre, at least [minSide] pixels high, no
     * larger than fits the screen, and still wholly on it.
     */
    fun resized(factor: Float, width: Float, height: Float, minSide: Float): Piece {
        val unit = min(width, height)
        val largest = min(MAX_SIZE, width / unit / aspect)
        val next = copy(size = (size * factor).coerceIn(min(minSide / unit, largest), largest))
        val area = next.area(width, height)
        val cx = area.centerX.coerceIn(area.width / 2, width - area.width / 2)
        val cy = area.centerY.coerceIn(area.height / 2, height - area.height / 2)
        return next.copy(x = cx / width, y = cy / height)
    }

    companion object {
        const val CUSTOM_PREFIX = "custom-"

        /** The largest piece: this much of the shorter side high. */
        const val MAX_SIZE = 0.9f

        val OPACITIES = 0.2f..1f
        const val OPACITY_STEP = 0.1f
    }
}

/**
 * The on-screen controls as the player laid them out for one screen size and orientation
 * ([key]), in drawing order: a later piece lies over an earlier one and takes a finger first.
 */
data class ControlsLayout(val pieces: List<Piece>) {
    fun piece(id: String): Piece? = pieces.firstOrNull { it.id == id }

    fun replace(piece: Piece) = ControlsLayout(pieces.map { if (it.id == piece.id) piece else it })

    fun without(id: String) = ControlsLayout(pieces.filterNot { it.id == id })

    /** A new custom button bound to [action] at the screen's centre, last (on top); and its id. */
    fun withCustom(action: PadAction, label: String?, size: Float = CUSTOM_SIZE): Pair<ControlsLayout, String> {
        val next = (pieces.mapNotNull { it.id.removePrefix(Piece.CUSTOM_PREFIX).takeIf { _ -> it.custom }?.toIntOrNull() }.maxOrNull() ?: 0) + 1
        val id = Piece.CUSTOM_PREFIX + next
        val piece = Piece(id, 0.5f, 0.5f, size, action = action.encode(), label = label?.trim()?.takeIf { it.isNotEmpty() })
        return ControlsLayout(pieces + piece) to id
    }

    /**
     * The piece a finger at [x], [y] presses on a [width] x [height] screen: the topmost whose hit
     * area ([Piece.hitArea]) holds it; hidden pieces only with [includeHidden] (the editor).
     */
    fun hit(width: Float, height: Float, minHit: Float, x: Float, y: Float, includeHidden: Boolean = false): Piece? =
        pieces.lastOrNull { (includeHidden || !it.hidden) && it.hitArea(width, height, minHit).contains(x, y) }

    /**
     * The visible piece a controller's d-pad moves to from [from] in direction [dx], [dy] (each
     * -1, 0 or 1) on a [width] x [height] screen: the nearest whose centre lies that way,
     * straying sideways counting double; from nothing, the first piece. Null when none lies
     * that way.
     */
    fun next(from: String?, dx: Int, dy: Int, width: Float, height: Float): String? {
        val shown = pieces.filter { !it.hidden }
        val start = from?.let(::piece)?.area(width, height) ?: return shown.firstOrNull()?.id
        return shown.filter { it.id != from }.mapNotNull { piece ->
            val area = piece.area(width, height)
            val ax = area.centerX - start.centerX
            val ay = area.centerY - start.centerY
            val along = ax * dx + ay * dy
            if (along <= 0f) null else piece.id to along + 2 * abs(ax * dy - ay * dx)
        }.minByOrNull { it.second }?.first
    }

    fun encode(): String = json.encodeToString(Stored.serializer(), Stored(VERSION, pieces))

    @Serializable
    private data class Stored(val version: Int = VERSION, val pieces: List<Piece> = emptyList())

    companion object {
        const val VERSION = 1

        /** A new custom button's height, a fraction of the shorter side. */
        const val CUSTOM_SIZE = 0.15f

        private val json = Json { ignoreUnknownKeys = true; encodeDefaults = false }

        /** The stored layouts' key for a screen of [width] x [height] dp (landscape when wider). */
        fun key(width: Int, height: Int): String = "${width}x$height"

        /**
         * A stored layout over [default]: nothing stored or unreadable is the default; the
         * controller's pieces it lacks come from the default (in the default's place), pieces it
         * does not know and custom buttons without a binding are dropped.
         */
        fun decode(text: String?, default: ControlsLayout): ControlsLayout {
            if (text.isNullOrBlank()) return default
            val stored = try {
                json.decodeFromString(Stored.serializer(), text)
            } catch (_: IllegalArgumentException) {
                return default
            }
            val kept = stored.pieces.filter { piece ->
                piece.size > 0f && piece.x.isFinite() && piece.y.isFinite() &&
                    if (piece.custom) piece.binding.let { it is PadAction.Key || it is PadAction.Mouse } else piece.kind != null
            }.distinctBy { it.id }.map { it.copy(opacity = it.opacity.coerceIn(Piece.OPACITIES)) }
            if (kept.isEmpty()) return default
            val missing = default.pieces.filter { d -> kept.none { it.id == d.id } }
            return ControlsLayout(missing + kept)
        }

        /** Room the menu button takes in the top right corner (its 48 dp and margins). */
        private const val GLYPH = 60f

        /** Room the frame counter takes either side of the top centre. */
        private const val COUNTER = 40f

        /** The smallest default key, so it stays a 48 dp target with its drawing nearly that. */
        private const val MIN_BUTTON = 44f

        /** PD2's world picture's width over its height (1068 x 600; D2GL bars the rest of the screen). */
        const val WORLD_ASPECT = 16f / 9f

        /** PD2's bottom HUD (the globes, the belt, the skill buttons): this much of the world picture's height. */
        const val HUD_SHARE = 0.18f

        /** Where PD2 draws its world on a [width] x [height] screen: 16:9, as large as fits, centred. */
        fun worldPicture(width: Float, height: Float): Area {
            val w = min(width, height * WORLD_ASPECT)
            val h = w / WORLD_ASPECT
            return Area((width - w) / 2, (height - h) / 2, (width + w) / 2, (height + h) / 2)
        }

        /** PD2's bottom HUD band on a [width] x [height] screen, which the default keeps clear. */
        fun hudBand(width: Float, height: Float): Area {
            val picture = worldPicture(width, height)
            return Area(picture.left, picture.bottom - picture.height * HUD_SHARE, picture.right, picture.bottom)
        }

        /**
         * The default layout for a [width] x [height] dp screen, emulator style over the game and
         * clear of PD2's bottom HUD ([hudBand]): the stick low in the left corner just above the
         * HUD with the d-pad above it, A/B/X/Y low in the right corner likewise with RB/RT above
         * (clear of the menu button), LT/LB above the d-pad or, where the height is short, beside
         * it; Back and Start in the bar below the picture where there is one, else in the side
         * bars' bottom corners, else at the centre above the HUD; L3 and R3 hidden beside the
         * groups. Sizes follow the shorter side, within bounds that keep them thumb-sized on
         * phones and handhelds alike; the stick and the d-pad shrink together only when the
         * height above the HUD cannot hold them.
         */
        fun default(width: Float, height: Float): ControlsLayout {
            val unit = min(width, height)
            val edge = 16f
            val gap = 8f
            val button = (0.13f * unit).coerceIn(MIN_BUTTON, 56f)
            val wide = button * WIDE
            val picture = worldPicture(width, height)
            val hud = hudBand(width, height)
            // The groups' bottoms, and the height above them from the top margin.
            val low = hud.top - gap
            val room = low - edge
            var stick = (0.40f * unit).coerceIn(112f, 170f)
            var dpad = (0.34f * unit).coerceIn(100f, 150f)
            val stacked = button + gap + dpad + gap + stick <= room
            if (!stacked && max(dpad, button) + gap + stick > room) {
                val factor = ((room - gap) / (dpad + stick)).coerceAtLeast(0.1f)
                stick *= factor
                dpad *= factor
            }
            val face = min((0.42f * unit).coerceIn(120f, 180f), room - button - gap).coerceAtLeast(button)

            fun piece(kind: PieceKind, cx: Float, cy: Float, side: Float, hidden: Boolean = false) =
                Piece(kind.id, cx / width, cy / height, side / unit, hidden = hidden)

            val stickX = edge + stick / 2
            val stickY = low - stick / 2
            val dpadY = stickY - stick / 2 - gap - dpad / 2
            val dpadTop = dpadY - dpad / 2
            val shoulders = if (stacked) {
                val row = dpadTop - gap - button / 2
                listOf(edge + wide / 2 to row, edge + wide + gap + wide / 2 to row)
            } else {
                // Beside the d-pad at its top: in a row, or one above the other where a row would
                // reach the frame counter.
                val start = stickX + dpad / 2 + gap
                if (start + wide + gap + wide <= width / 2 - COUNTER) {
                    listOf(start + wide / 2 to dpadTop + button / 2, start + wide + gap + wide / 2 to dpadTop + button / 2)
                } else {
                    listOf(start + wide / 2 to dpadTop + button / 2, start + wide / 2 to dpadTop + button + gap + button / 2)
                }
            }
            val faceX = width - edge - face / 2
            val faceY = low - face / 2
            val rightRow = max(edge + button / 2, faceY - face / 2 - gap - button / 2)
            // The right row keeps left of the menu button when it reaches the top.
            val rightEnd = if (rightRow - button / 2 < GLYPH) width - GLYPH - 4f else width - edge

            // Back and Start: their size and centres.
            val below = height - picture.bottom
            val bar = picture.left
            val keys: Triple<Float, Pair<Float, Float>, Pair<Float, Float>> = when {
                // The bar below the picture (a 4:3 screen).
                below >= MIN_BUTTON + gap -> {
                    val b = min(button, below - gap)
                    val y = picture.bottom + below / 2
                    Triple(b, width / 2 - gap - b * WIDE / 2 to y, width / 2 + gap + b * WIDE / 2 to y)
                }
                // The side bars' bottom corners (a 20:9 phone), below the groups.
                bar >= MIN_BUTTON * WIDE + gap && hud.height >= MIN_BUTTON + 2 * gap -> {
                    val b = min(min(button, (bar - gap) / WIDE), hud.height - 2 * gap)
                    val y = height - gap - b / 2
                    Triple(b, bar / 2 to y, width - bar / 2 to y)
                }
                else -> Triple(button, width / 2 - gap - wide / 2 to low - button / 2, width / 2 + gap + wide / 2 to low - button / 2)
            }
            val (key, back, start) = keys
            return ControlsLayout(
                listOf(
                    piece(PieceKind.STICK, stickX, stickY, stick),
                    piece(PieceKind.DPAD, stickX, dpadY, dpad),
                    piece(PieceKind.FACE, faceX, faceY, face),
                    piece(PieceKind.LT, shoulders[0].first, shoulders[0].second, button),
                    piece(PieceKind.LB, shoulders[1].first, shoulders[1].second, button),
                    piece(PieceKind.RB, rightEnd - wide - gap - wide / 2, rightRow, button),
                    piece(PieceKind.RT, rightEnd - wide / 2, rightRow, button),
                    piece(PieceKind.SELECT, back.first, back.second, key),
                    piece(PieceKind.START, start.first, start.second, key),
                    piece(PieceKind.L3, stickX + stick / 2 + gap + button / 2, stickY + stick / 2 - button / 2, button, hidden = true),
                    piece(PieceKind.R3, faceX - face / 2 - gap - button / 2, faceY + face / 2 - button / 2, button, hidden = true),
                ),
            )
        }
    }
}
