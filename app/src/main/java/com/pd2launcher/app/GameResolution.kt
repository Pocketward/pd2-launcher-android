package com.pd2launcher.app

import android.content.Context
import android.graphics.Point
import android.hardware.display.DisplayManager
import android.view.Display
import androidx.annotation.StringRes
import kotlin.math.roundToInt

/**
 * The game's resolution and how the Game view shows it (Settings → Game).
 *
 * Game resolution is the X screen's size for the app's own Play ([forPlay]): the game's own size
 * ([ownSize]: PD2's 1068 x 600 picture, which D2GL draws 1:1), the default, or a Custom size the
 * player typed ([typedCustom], [AppSettings.customSize]). Display ([GameDisplay],
 * [AppSettings.gameDisplay]) is how the Game view shows that X screen ([viewGeometry]), followed
 * live. The official PD2 launcher's sizes ([launcherChoices], [forLauncher]) keep the display's
 * own landscape shape.
 */
object GameResolution {
    /** The fewest lines the official launcher's choices offer: PD2's 800 x 600 menus stay readable scaled down this far. */
    const val MIN_LINES = 480

    /**
     * Every X screen the app starts has a multiple of these lines and an even width ([xScreen]):
     * Turnip pads a buffer's last rows to 4, so for other lines it wants more memory than the app's
     * buffer has and the direct present is refused (the frames then go through the X screen's copy).
     */
    const val LINES_STEP = 4

    /** The game's own size: PD2's picture in the game, which D2GL draws 1:1 on an X screen of this size. */
    val OWN_SIZE = 1068 to 600

    /** PD2's menus (login, character select, the main menu): 4:3, which D2GL fits evenly into the X screen. */
    val MENU_SIZE = 800 to 600

    /** The smallest Custom size; the largest is the display's own ([customMax]). */
    val CUSTOM_MIN = 640 to 480

    /** The official launcher's sizes below the display's full size, in lines. */
    private val LINES = listOf(1080, 900, 720, 600, MIN_LINES)

    /** The default display's native size in landscape, width to height. */
    @Suppress("DEPRECATION")
    fun display(context: Context): Pair<Int, Int> {
        val display = context.getSystemService(DisplayManager::class.java).getDisplay(Display.DEFAULT_DISPLAY)
        val size = Point()
        display.getRealSize(size)
        return maxOf(size.x, size.y) to minOf(size.x, size.y)
    }

    /**
     * The one rounding of every X screen the app starts (the game's, the official launcher's, an
     * install's, a debug size): [size] with an even width and a multiple of [LINES_STEP] lines,
     * rounded down; [log] gets one line where that changes it (1068 x 930 starts as 1068 x 928).
     */
    fun xScreen(size: Pair<Int, Int>, log: ((String) -> Unit)? = null): Pair<Int, Int> {
        val rounded = size.first / 2 * 2 to size.second / LINES_STEP * LINES_STEP
        if (rounded != size) {
            log?.invoke(
                "X screen ${size.first}x${size.second} rounded down to ${rounded.first}x${rounded.second} " +
                    "(an even width, lines a multiple of $LINES_STEP)",
            )
        }
        return rounded
    }

    /**
     * The official launcher's choices for a [width] x [height] landscape display: its full size
     * first, then each of [LINES] below it and no fewer than [MIN_LINES], at the display's shape
     * ([at]), each rounded as every X screen ([xScreen]).
     */
    fun launcherChoices(width: Int, height: Int): List<Pair<Int, Int>> =
        (listOf(width to height) + LINES.filter { it < height && it >= MIN_LINES }.map { at(width, height, it) })
            .map { xScreen(it) }.distinct()

    /**
     * The [width] x [height] display's shape at [lines] (an even width, rounded down); the full
     * size at as many lines as the display has or more. On a 1604 x 720 display 480 lines are
     * 1068 x 480.
     */
    fun at(width: Int, height: Int, lines: Int): Pair<Int, Int> {
        if (lines >= height) return width to height
        return width * lines / height / 2 * 2 to lines
    }

    /**
     * The X screen the official PD2 launcher opens on: the [picked] lines (null: the full size) at
     * the display's shape, rounded as every X screen ([xScreen], which tells [log]).
     */
    fun forLauncher(width: Int, height: Int, picked: Int?, log: ((String) -> Unit)? = null): Pair<Int, Int> =
        xScreen(at(width, height, picked ?: height), log)

    /** Whether a [width] x [height] display holds the game's own size, [OWN_SIZE]. */
    fun holdsOwnSize(width: Int, height: Int): Boolean = width >= OWN_SIZE.first && height >= OWN_SIZE.second

    /**
     * The game's own size on a [width] x [height] landscape display: [OWN_SIZE] where the display
     * holds it, else the display's full size, rounded as every X screen ([xScreen], which tells
     * [log]): 800 x 476 on 800 x 478, the same as [customMax] there.
     */
    fun ownSize(width: Int, height: Int, log: ((String) -> Unit)? = null): Pair<Int, Int> =
        if (holdsOwnSize(width, height)) OWN_SIZE else xScreen(width to height, log)

    /** The largest Custom size on a [width] x [height] display: its own size, rounded as every X screen ([xScreen]). */
    fun customMax(width: Int, height: Int): Pair<Int, Int> = xScreen(width to height)

    /**
     * Whether [size] is a Custom size that a [width] x [height] display allows: from [CUSTOM_MIN]
     * to [customMax], the width even and the lines a multiple of [LINES_STEP].
     */
    fun customAllowed(size: Pair<Int, Int>, width: Int, height: Int): Boolean {
        val (w, h) = size
        val (maxWidth, maxHeight) = customMax(width, height)
        return w in CUSTOM_MIN.first..maxWidth && h in CUSTOM_MIN.second..maxHeight && w % 2 == 0 && h % LINES_STEP == 0
    }

    /** Whether a [width] x [height] display offers Custom: it allows [CUSTOM_MIN]. */
    fun customOffered(width: Int, height: Int): Boolean = customAllowed(CUSTOM_MIN, width, height)

    /**
     * What the player typed for Custom ([widthText], [heightText]) on a [width] x [height]
     * display: the size where both are whole numbers and [customAllowed] allows it, else null.
     */
    fun typedCustom(widthText: String, heightText: String, width: Int, height: Int): Pair<Int, Int>? {
        val w = widthText.trim().toIntOrNull() ?: return null
        val h = heightText.trim().toIntOrNull() ?: return null
        return (w to h).takeIf { customAllowed(it, width, height) }
    }

    /**
     * The Custom size to keep for a typed [size] on a [width] x [height] display: none where it is
     * the game's own size ([ownSize]), so the pick is the default again.
     */
    fun customPick(size: Pair<Int, Int>, width: Int, height: Int): Pair<Int, Int>? = size.takeIf { it != ownSize(width, height) }

    /** The kept [custom] size where a [width] x [height] display allows it ([customAllowed]); else null, the game's own size. */
    fun allowedCustom(width: Int, height: Int, custom: Pair<Int, Int>?): Pair<Int, Int>? = custom?.takeIf { customAllowed(it, width, height) }

    /**
     * The X screen the app's own Play uses on a [width] x [height] display: the kept [custom] size
     * ([allowedCustom], already an X screen's size), else [ownSize], which tells [log] where it rounds.
     */
    fun forPlay(width: Int, height: Int, custom: Pair<Int, Int>?, log: ((String) -> Unit)? = null): Pair<Int, Int> =
        allowedCustom(width, height, custom) ?: ownSize(width, height, log)

    /**
     * The game resolution on a [width] x [height] display as Settings names it: "Game's own size
     * (1068 × 600)", "800 × 480 (full)" on a display that does not hold the game's own size, or
     * "Custom (1280 × 720)" for a kept [custom] size the display allows.
     */
    fun label(width: Int, height: Int, custom: Pair<Int, Int>?): Line {
        val (w, h) = forPlay(width, height, custom)
        return when {
            allowedCustom(width, height, custom) != null -> Line(R.string.settings_resolution_custom, w, h)
            holdsOwnSize(width, height) -> Line(R.string.settings_resolution_game_size, w, h)
            else -> Line(R.string.settings_resolution_full, w, h)
        }
    }

    /**
     * What the Game view enlarges ([viewGeometry]): the whole X screen for the official [launcher]'s
     * window (null), else PD2's [MENU_SIZE] while it shows a [menu], else its game picture, [OWN_SIZE]
     * (also while nothing is known yet, and with D2GL's Stretched Viewport, whose menu has no bars).
     */
    fun shownPicture(launcher: Boolean, menu: Boolean): Pair<Int, Int>? = when {
        launcher -> null
        menu -> MENU_SIZE
        else -> OWN_SIZE
    }

    /**
     * Where D2GL draws a [picture] (PD2's [MENU_SIZE] or [OWN_SIZE]) on the [screen] (the X
     * screen): evenly as large as fits, centred; its width and height in X screen pixels.
     */
    fun pictureArea(picture: Pair<Int, Int>, screen: Pair<Int, Int>): Pair<Double, Double> {
        val scale = minOf(screen.first.toDouble() / picture.first, screen.second.toDouble() / picture.second)
        return picture.first * scale to picture.second * scale
    }

    /** A rectangle in pixels: [left], [top], [width] x [height]. */
    data class PictureRect(val left: Int, val top: Int, val width: Int, val height: Int) {
        override fun toString() = "${width}x$height at $left,$top"
    }

    /**
     * The Game view's place in its area: the view's size, [view] (centred in the area, its sides
     * past the area's edges where larger), and the rectangle of the area in which the game's
     * picture shows, [picture].
     */
    data class ViewGeometry(val view: Pair<Int, Int>, val picture: PictureRect)

    /**
     * The Game view's geometry for the [screen] (the X screen) in a [width] x [height] pixel area,
     * as [display] says, while PD2 shows a [picture] ([MENU_SIZE] or [OWN_SIZE] in the game; null:
     * the whole X screen, as for the official launcher's window).
     *
     * [GameDisplay.FIT] enlarges or shrinks what the game shows (the picture's [pictureArea] on the
     * X screen) evenly until it touches the area's edges; the X screen's other parts (D2GL's bars)
     * reach past the edges where they do not fit. [GameDisplay.CENTER] shows the X screen's own
     * pixels in the middle where the area holds them, else as [GameDisplay.FIT].
     */
    fun viewGeometry(display: GameDisplay, picture: Pair<Int, Int>?, screen: Pair<Int, Int>, width: Int, height: Int): ViewGeometry {
        val (screenWidth, screenHeight) = screen
        val (areaWidth, areaHeight) = picture?.let { pictureArea(it, screen) } ?: (screenWidth.toDouble() to screenHeight.toDouble())
        val scale = if (display == GameDisplay.CENTER && screenWidth <= width && screenHeight <= height) {
            1.0
        } else {
            minOf(width / areaWidth, height / areaHeight)
        }
        val shownWidth = (areaWidth * scale).roundToInt()
        val shownHeight = (areaHeight * scale).roundToInt()
        return ViewGeometry(
            (screenWidth * scale).roundToInt() to (screenHeight * scale).roundToInt(),
            PictureRect((width - shownWidth) / 2, (height - shownHeight) / 2, shownWidth, shownHeight),
        )
    }
}

/** How the Game view shows the X screen (Settings → Game → Display, the in-game menu; [AppSettings.gameDisplay]): [title] and [body] as Settings says them. */
enum class GameDisplay(@param:StringRes val title: Int, @param:StringRes val body: Int) {
    /** What the game shows, as large as the view allows, its shape kept ([GameResolution.viewGeometry]). */
    FIT(R.string.settings_display_fit, R.string.settings_display_fit_body),

    /** One X screen pixel to one display pixel, in the middle, black around it; [FIT] where the view cannot hold it. */
    CENTER(R.string.settings_display_center, R.string.settings_display_center_body),
    ;

    /** The other way, for the in-game menu's Display row. */
    val other: GameDisplay get() = if (this == FIT) CENTER else FIT
}
