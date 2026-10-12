package com.pd2launcher.app

import android.content.Context
import android.graphics.Point
import android.hardware.display.DisplayManager
import android.view.Display

/**
 * The game's resolution (Settings → Game): the X screen's size,
 * which the Game view scales up to the display. Every choice keeps the display's own landscape
 * shape, so the picture fills it and [com.pd2launcher.app.input.GamePicture] reads it as on the
 * full size. The player's pick is a number of lines ([AppSettings.gameResolution]); none is
 * Automatic ([automaticLines]).
 */
object GameResolution {
    /** The fewest lines offered: PD2's 800 x 600 menus stay readable scaled down this far. */
    const val MIN_LINES = 480

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
     * Automatic's lines on a [height]-line display: [MIN_LINES] on the older and entry-level
     * Adrenos ([lowTierAdreno]), where the full size is slow; [MALI_LINES] on a Mali; else (a
     * newer Adreno, or no GPU known) the full height.
     */
    fun automaticLines(height: Int, gpu: HostGpu?): Int = when {
        gpu is MaliGpu -> minOf(MALI_LINES, height)
        gpu is KgslGpu && lowTierAdreno(gpu.chipId) -> minOf(MIN_LINES, height)
        else -> height
    }

    /** The X screen a Play uses: the [picked] lines (null: Automatic) at the display's shape. */
    fun forPlay(width: Int, height: Int, picked: Int?, gpu: HostGpu?): Pair<Int, Int> =
        at(width, height, picked ?: automaticLines(height, gpu))

    /** The X screen the official PD2 launcher opens on: the [picked] lines (null: the full size) at the display's shape. */
    fun forLauncher(width: Int, height: Int, picked: Int?): Pair<Int, Int> = at(width, height, picked ?: height)
}
