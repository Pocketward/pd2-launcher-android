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
 * Game resolution is the X screen's size for the app's own Play and for the official PD2 launcher
 * ([forPlay]; a game the launcher starts runs on its X screen): the game's own size ([ownSize]:
 * PD2's 1068 x 600 picture, which D2GL draws 1:1), the default, or a Custom size the player typed
 * ([typedCustom], [AppSettings.customSize]). Display ([GameDisplay], [AppSettings.gameDisplay]) is
 * how the Game view shows that X screen ([viewGeometry]), followed live; the same for the game and
 * for the launcher's window ([LAUNCHER_WINDOW]).
 */
object GameResolution {
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

    /**
     * The official PD2 launcher's window: a fixed 800 x 600 (PD2Launcher.exe's main window at
     * Wine's 96 DPI), in the middle of the X screen as D2GL puts PD2's 4:3 menu: the X input relay
     * moves it to the middle when it shows and wherever it is somewhere else (a place the launcher
     * kept from another X screen, a drag). An X screen with fewer columns or lines cuts it off: below
     * 788 columns or 588 lines Windows makes the window smaller (the X screen's size and 12 pixels
     * more each way: 652 x 492 on 640 x 480), its parts overlap and its right and bottom edges are
     * off the X screen, and the relay leaves it where it is. The relay has the same size
     * (LAUNCHER_WIDTH, LAUNCHER_HEIGHT in userland/x-input-relay.c): change both together.
     */
    val LAUNCHER_WINDOW = 800 to 600

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
    fun typedCustom(widthText: String, heightText: String, width: Int, height: Int): Pair<Int, Int>? =
        typedSize(widthText, heightText)?.takeIf { customAllowed(it, width, height) }

    /** What the player typed for Custom ([widthText], [heightText]) where both are whole numbers, allowed or not; else null. */
    fun typedSize(widthText: String, heightText: String): Pair<Int, Int>? {
        val w = widthText.trim().toIntOrNull() ?: return null
        val h = heightText.trim().toIntOrNull() ?: return null
        return w to h
    }

    /**
     * Whether an X screen of [size] cuts off part of the official launcher's window: it has fewer
     * columns or lines than [LAUNCHER_WINDOW]. Settings then says so under the game resolution and
     * in its Custom input (the player decides; the size stays allowed).
     */
    fun cutsLauncher(size: Pair<Int, Int>): Boolean = size.first < LAUNCHER_WINDOW.first || size.second < LAUNCHER_WINDOW.second

    /**
     * The Custom size to keep for a typed [size] on a [width] x [height] display: none where it is
     * the game's own size ([ownSize]), so the pick is the default again.
     */
    fun customPick(size: Pair<Int, Int>, width: Int, height: Int): Pair<Int, Int>? = size.takeIf { it != ownSize(width, height) }

    /** The kept [custom] size where a [width] x [height] display allows it ([customAllowed]); else null, the game's own size. */
    fun allowedCustom(width: Int, height: Int, custom: Pair<Int, Int>?): Pair<Int, Int>? = custom?.takeIf { customAllowed(it, width, height) }

    /**
     * The X screen the app's own Play and the official PD2 launcher use on a [width] x [height]
     * display: the kept [custom] size ([allowedCustom], already an X screen's size), else
     * [ownSize], which tells [log] where it rounds.
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
     * What the Game view enlarges ([viewGeometry]) on the [screen] (the X screen), its width and
     * height in X screen pixels: the official [launcher]'s window, [LAUNCHER_WINDOW] at its own
     * pixels in the middle (the part the X screen holds); else D2GL's fit ([pictureArea]) of PD2's
     * [MENU_SIZE] while it shows a [menu], else of its game picture, [OWN_SIZE] (also while nothing
     * is known yet, and with D2GL's Stretched Viewport, whose menu has no bars).
     */
    fun shownArea(launcher: Boolean, menu: Boolean, screen: Pair<Int, Int>): Pair<Double, Double> = when {
        launcher -> minOf(LAUNCHER_WINDOW.first, screen.first).toDouble() to minOf(LAUNCHER_WINDOW.second, screen.second).toDouble()
        menu -> pictureArea(MENU_SIZE, screen)
        else -> pictureArea(OWN_SIZE, screen)
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
     * as [display] says, while the X screen shows [shown] in its middle ([shownArea]: PD2's menu or
     * game picture, or the official launcher's window; its width and height in X screen pixels).
     *
     * [GameDisplay.FIT] enlarges or shrinks [shown] evenly until it touches the area's edges; the X
     * screen's other parts (D2GL's bars, the black beside the launcher's window) reach past the
     * edges where they do not fit. [GameDisplay.CENTER] shows the X screen's own pixels in the
     * middle where the area holds them, else as [GameDisplay.FIT].
     */
    fun viewGeometry(display: GameDisplay, shown: Pair<Double, Double>, screen: Pair<Int, Int>, width: Int, height: Int): ViewGeometry {
        val (screenWidth, screenHeight) = screen
        val (areaWidth, areaHeight) = shown
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

/**
 * How the Game view shows the X screen, the same for the game and the official launcher's window
 * (Settings → Game → Display, the in-game menu; [AppSettings.gameDisplay]): [title] and [body] as
 * Settings says them.
 */
enum class GameDisplay(@param:StringRes val title: Int, @param:StringRes val body: Int) {
    /** What the game or the launcher shows, as large as the view allows, its shape kept ([GameResolution.viewGeometry]). */
    FIT(R.string.settings_display_fit, R.string.settings_display_fit_body),

    /** One X screen pixel to one display pixel, in the middle, black around it; [FIT] where the view cannot hold it. */
    CENTER(R.string.settings_display_center, R.string.settings_display_center_body),
    ;

    /** The other way, for the in-game menu's Display row. */
    val other: GameDisplay get() = if (this == FIT) CENTER else FIT
}
