package com.pd2launcher.app

import android.content.Context
import android.graphics.Point
import android.hardware.display.DisplayManager
import android.view.Display

/**
 * The game's resolution (Settings → Game): the X screen's size,
 * which the Game view scales up to the display. Every choice keeps the display's own landscape
 * shape, so the picture fills it and [com.pd2launcher.app.input.GamePicture] reads it as on the
 * full size. The player's pick is a number of lines ([AppSettings.gameResolution]) or
 * [GAME_SIZE]; none is Automatic ([automaticLines]).
 */
object GameResolution {
    /** The fewest lines offered: PD2's 800 x 600 menus stay readable scaled down this far. */
    const val MIN_LINES = 480

    /** The pick that is not a number of lines: the game's own size ([gameSize]). */
    const val GAME_SIZE = 0

    /** PD2's own picture in the game, which D2GL scales to fit the X screen (its menus are 800 x 600). */
    private const val GAME_WIDTH = 1068
    private const val GAME_HEIGHT = 600

    /**
     * Automatic's lines on a Mali (PanVK, Arm's kbase): a moderate size. Every frame is copied on
     * the CPU (Mesa's software WSI into Xvfb, Xvfb's frame buffer into the Game view), which grows
     * with the X screen; 720 lines are under half the pixels of a 1080-line display's full size.
     */
    const val MALI_LINES = 720

    /** The sizes below the display's full size, in lines. */
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
     * The choices for a [width] x [height] landscape display: its full size first, then each of
     * [LINES] below it and no fewer than [MIN_LINES], at the display's shape ([at]).
     */
    fun choices(width: Int, height: Int): List<Pair<Int, Int>> =
        listOf(width to height) + LINES.filter { it < height && it >= MIN_LINES }.map { at(width, height, it) }

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
     * The game's own size on a [width] x [height] landscape display: the smallest X screen of the
     * display's shape that holds PD2's 1068 x 600 picture, so D2GL draws it 1:1, not scaled (bars
     * where the shapes differ); null where the display holds no more (its full size is then the
     * most). The lines are the display's shape at 1068 wide, even and rounded down as [at] rounds
     * widths, and at least 600; the width is the shape at those lines ([at]), at least 1068. A
     * display wider than the picture gets 600 lines (1604 x 720: 1336 x 600, its 600-line size),
     * a narrower one 1068 columns (1280 x 960: 1068 x 800). On 16:9 the shape at 1068 wide is
     * 600.75 lines: rounded down, 1068 x 600 is the picture exactly (602 lines would put a line of
     * bar above and below it), only 0.125 % wider than the display's shape (852 x 480, [at]'s
     * 480 lines there, is 0.16 % narrower).
     */
    fun gameSize(width: Int, height: Int): Pair<Int, Int>? {
        val lines = maxOf(GAME_HEIGHT, GAME_WIDTH * height / width / 2 * 2)
        val size = maxOf(GAME_WIDTH, at(width, height, lines).first) to lines
        return size.takeIf { it.first < width && it.second < height }
    }

    /** The game's own [size] ([gameSize]) as Settings names it: "Game's own size (1068 × 600)". */
    fun gameSizeLabel(size: Pair<Int, Int>): Line = Line(R.string.settings_resolution_game_size, size.first, size.second)

    /** One of the game resolution's choices below Automatic: the [size] it makes and the [pick] that keeps it. */
    data class Choice(val size: Pair<Int, Int>, val pick: Int)

    /**
     * The game resolution's choices below Automatic on a [width] x [height] display, largest
     * first: the display's ([choices]), each kept as its lines, and the game's own size
     * ([gameSize], kept as [GAME_SIZE]) among them by its lines, in place of the choice of the same
     * lines. On 16:9 and wider that is the 600-line choice, the same size or (16:9) 2 columns
     * narrower; on a narrower display (4:3: 1068 x 800) the 600-line choice stays.
     */
    fun gameChoices(width: Int, height: Int): List<Choice> {
        val choices = choices(width, height).map { Choice(it, it.second) }
        val own = gameSize(width, height) ?: return choices
        return choices.filter { it.pick > own.second } + Choice(own, GAME_SIZE) + choices.filter { it.pick < own.second }
    }

    /**
     * The [picked] resolution as the choices on a [width] x [height] display hold it
     * ([gameChoices]): the lines whose choice the game's own size took are that size (a 600-line
     * pick on 16:9: 1068 x 600 for 1066 x 600, the nearest), and the game's own size where the
     * display holds no more is its full size.
     */
    fun offered(width: Int, height: Int, picked: Int?): Int? {
        val own = gameSize(width, height)
        return when {
            picked == GAME_SIZE && own == null -> height
            picked != null && picked == own?.second -> GAME_SIZE
            else -> picked
        }
    }

    /**
     * Automatic's lines on a [height]-line display: [MIN_LINES] on the older and entry-level
     * Adrenos ([lowTierAdreno]), where the full size is slow; [MALI_LINES] on a Mali; else (a
     * newer Adreno, or no GPU known) the full height.
     */
    fun automaticLines(height: Int, gpu: HostGpu?): Int = when {
        gpu is MaliGpu -> minOf(MALI_LINES, height)
        gpu is KgslGpu && lowTierAdreno(gpu.chipId) -> minOf(MIN_LINES, height)
        else -> height
    }

    /**
     * The X screen a Play uses: the [picked] lines (null: Automatic) at the display's shape, or the
     * game's own size ([GAME_SIZE]), as the choices hold them ([offered]).
     */
    fun forPlay(width: Int, height: Int, picked: Int?, gpu: HostGpu?): Pair<Int, Int> = when (val pick = offered(width, height, picked)) {
        GAME_SIZE -> gameSize(width, height) ?: (width to height)
        else -> at(width, height, pick ?: automaticLines(height, gpu))
    }

    /** The X screen the official PD2 launcher opens on: the [picked] lines (null: the full size) at the display's shape. */
    fun forLauncher(width: Int, height: Int, picked: Int?): Pair<Int, Int> = at(width, height, picked ?: height)
}
