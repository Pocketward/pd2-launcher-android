package com.pd2launcher.app.input

import kotlin.math.abs
import kotlin.math.max
import kotlin.math.min

/**
 * What PD2 shows on the X screen, told from the shape of its picture: D2GL draws the menus
 * (login, character select, the main menu) 4:3 (800 x 600) and the game world 16:9 (1068 x
 * 600), each as large as fits the X screen, and fills the rest with one flat colour. On a 20:9
 * screen, and on the game's own 1068 x 600 ([com.pd2launcher.app.GameResolution.OWN_SIZE]), the
 * world reaches into side strips a menu leaves flat; on a 4:3 screen a menu fills the top and
 * bottom strips the world leaves flat.
 */
enum class GamePicture {
    MENU,
    WORLD,

    /** Nothing tells (a flat screen, a loading screen, no frame yet). */
    UNKNOWN,
    ;

    companion object {
        const val MENU_ASPECT = 4f / 3f

        /** Pixels this close (each channel) count as one flat colour. */
        private const val FLAT = 10

        /** The strips are read this far inside their sides, past any blending at the picture's edge. */
        private const val INSET = 4

        /** Strips thinner than this (after the inset) are rounding, not bars. */
        private const val THIN = 8

        /** Samples across and down each strip: rows down to PD2's bottom HUD, never dark. */
        private const val COLUMNS = 8
        private const val ROWS = 12

        /** The picture of a [width] x [height] X screen whose pixel at x, y is [pixel] (32 bits, any byte order). */
        fun of(width: Int, height: Int, pixel: (Int, Int) -> Int): GamePicture {
            if (width <= 0 || height <= 0) return UNKNOWN
            val w = width.toFloat()
            val h = height.toFloat()
            val menu = fit(MENU_ASPECT, w, h)
            val world = ControlsLayout.worldPicture(w, h)
            // Where only the world draws, and where only a menu draws.
            val worldOnly = strips(world, menu)
            val menuOnly = strips(menu, world)
            fun drawn(area: Area) = !flat(area, pixel)
            return when {
                worldOnly.any(::drawn) -> WORLD
                menuOnly.any(::drawn) -> MENU
                // The strips are bars: whichever picture leaves them so is the one drawn.
                !drawn(menu.intersection(world)) -> UNKNOWN
                worldOnly.isNotEmpty() -> MENU
                menuOnly.isNotEmpty() -> WORLD
                else -> UNKNOWN
            }
        }

        private fun fit(aspect: Float, width: Float, height: Float): Area {
            val w = min(width, height * aspect)
            val h = w / aspect
            return Area((width - w) / 2, (height - h) / 2, (width + w) / 2, (height + h) / 2)
        }

        private fun Area.intersection(other: Area) =
            Area(max(left, other.left), max(top, other.top), min(right, other.right), min(bottom, other.bottom))

        /** The parts of [a] outside [b] (both centred on the screen), inset and without slivers. */
        private fun strips(a: Area, b: Area): List<Area> = buildList {
            if (a.top < b.top) add(Area(a.left, a.top, a.right, b.top))
            if (b.bottom < a.bottom) add(Area(a.left, b.bottom, a.right, a.bottom))
            if (a.left < b.left) add(Area(a.left, max(a.top, b.top), b.left, min(a.bottom, b.bottom)))
            if (b.right < a.right) add(Area(b.right, max(a.top, b.top), a.right, min(a.bottom, b.bottom)))
        }.map { Area(it.left + INSET, it.top + INSET, it.right - INSET, it.bottom - INSET) }
            .filter { it.width >= THIN && it.height >= THIN }

        /** Every sample of [area] the same colour. */
        private fun flat(area: Area, pixel: (Int, Int) -> Int): Boolean {
            var first: Int? = null
            for (row in 0 until ROWS) for (column in 0 until COLUMNS) {
                val x = (area.left + area.width * (column + 0.5f) / COLUMNS).toInt()
                val y = (area.top + area.height * (row + 0.5f) / ROWS).toInt()
                val color = pixel(x, y)
                val base = first ?: color.also { first = it }
                for (shift in intArrayOf(0, 8, 16, 24)) {
                    if (abs((color shr shift and 0xFF) - (base shr shift and 0xFF)) > FLAT) return false
                }
            }
            return true
        }
    }
}

/**
 * [GamePicture] samples into a steady answer: the first known picture at once, a change only
 * after [needed] samples in a row agree ([leaveMenu] for a change away from [GamePicture.MENU]),
 * [GamePicture.UNKNOWN] never (it keeps what was). It can start from a picture steadied before
 * ([initial]), which then changes as any other.
 */
class PictureDebounce(initial: GamePicture = GamePicture.UNKNOWN, private val needed: Int = 3, private val leaveMenu: Int = needed) {
    var current: GamePicture = initial
        private set
    private var candidate: GamePicture = GamePicture.UNKNOWN
    private var count = 0

    fun sample(picture: GamePicture): GamePicture {
        if (picture == GamePicture.UNKNOWN || picture == current) {
            count = 0
        } else if (current == GamePicture.UNKNOWN) {
            current = picture
        } else {
            count = if (picture == candidate) count + 1 else 1
            candidate = picture
            if (count >= (if (current == GamePicture.MENU) leaveMenu else needed)) {
                current = picture
                count = 0
            }
        }
        return current
    }
}
