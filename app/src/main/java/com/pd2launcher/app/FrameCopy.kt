package com.pd2launcher.app

import android.content.Context
import android.hardware.display.DisplayManager
import android.view.Display
import android.view.Surface
import java.io.File
import java.io.RandomAccessFile
import java.nio.ByteBuffer
import java.nio.ByteOrder

/**
 * The X screen's framebuffer as Xvfb lays it out ([Framebuffer]): an XWD header (25 32-bit
 * fields in Xvfb's byte order written as big-endian, then the window name), the colormap
 * ([ncolors] entries of 12 bytes), then the pixels.
 */
data class XwdHeader(val width: Int, val height: Int, val bitsPerPixel: Int, val bytesPerLine: Int, val pixelOffset: Long) {
    /**
     * A 32-bit screen of sizes the copy can take: each side 1 to [MAX_SIDE], lines at least as long
     * as the width's pixels (counted in 64 bits) and whole pixels long. The header is in memory a
     * guest can write; frame_copy.c checks the same.
     */
    val plausible: Boolean
        get() = bitsPerPixel == 32 && width in 1..MAX_SIDE && height in 1..MAX_SIDE &&
            bytesPerLine > 0 && bytesPerLine % 4 == 0 && bytesPerLine.toLong() >= width * 4L

    companion object {
        const val SIZE = 100
        /** The largest X screen side taken (frame_copy.c's MAX_SIDE). */
        const val MAX_SIDE = 16384
        private const val FILE_VERSION = 7
        private const val COLOR_SIZE = 12

        /** The header in [bytes] (at least [SIZE]), or null when it is not an XWD version 7 header. */
        fun parse(bytes: ByteArray): XwdHeader? {
            if (bytes.size < SIZE) return null
            val order = listOf(ByteOrder.BIG_ENDIAN, ByteOrder.LITTLE_ENDIAN)
                .firstOrNull { ByteBuffer.wrap(bytes).order(it).getInt(4) == FILE_VERSION } ?: return null
            val b = ByteBuffer.wrap(bytes).order(order)
            fun field(index: Int) = b.getInt(index * 4).toLong() and 0xFFFFFFFFL
            val headerSize = field(0)
            val ncolors = field(19)
            return XwdHeader(
                width = field(4).toInt(),
                height = field(5).toInt(),
                bitsPerPixel = field(11).toInt(),
                bytesPerLine = field(12).toInt(),
                pixelOffset = headerSize + ncolors * COLOR_SIZE,
            )
        }
    }
}

/**
 * Copies the X screen into a Surface on each display frame it changed in (libframecopy.so):
 * into a child layer of the Surface set by transactions, not through the Surface's buffer
 * queue, which some devices pace to 30 frames a second. While it runs it paces the
 * game's presents to the frames it takes (Mesa patch 0003, [PresentCounter]). The copy
 * thread logs, under the logcat tag `pd2-gameview` every 5 s, the game's presents per second
 * (from the present counter), its own checks (one a display frame), the frames whose content
 * changed, the frames it posted, the presents no check saw, the hashes and the copy time.
 */
class FrameCopy private constructor(private var handle: Long) {
    /**
     * The copy's counts so far, for [GameFrameRate]; null once stopped. Call on the thread that
     * calls [stop].
     */
    fun counts(): FrameCounts? {
        if (handle == 0L) return null
        val presents = nativePresents(handle)
        return FrameCounts(
            presents = if (presents >= 0) presents else null,
            shownPresents = nativeShownPresents(handle),
            changed = nativeChangedFrames(handle),
        )
    }

    /**
     * The buffers that went to the compositor since the start: the copy's own, and the game's
     * presents while its own layer shows (one buffer each); null once stopped. Without new ones
     * Android lowers the display's rate ([RateWatch]). Call on the thread that calls [stop].
     */
    fun posted(): Long? = if (handle != 0L) nativePostedFrames(handle) else null

    /** Its layer asks the display for [frameRate] from now on ([FrameRateVote]). Call on the thread that calls [stop]. */
    fun setFrameRate(frameRate: Float) {
        if (handle != 0L) nativeSetFrameRate(handle, frameRate)
    }

    /** The surface has a new size ([width] x [height]): the copy's layer shows the X screen scaled to it, with no new copy. */
    fun resize(width: Int, height: Int) {
        if (handle != 0L) nativeResize(handle, width, height)
    }

    fun stop() {
        if (handle != 0L) nativeStop(handle)
        handle = 0L
    }

    companion object {
        init {
            System.loadLibrary("framecopy")
        }

        /**
         * Starts copying [framebuffer] (32-bit pixels only, laid out as [header] says) into
         * [surface]; null if it cannot. On a display turned a quarter ([displayRotation],
         * `Surface.ROTATION_*`) the frames are stored turned back, so the panel takes them without
         * a rotation: on display hardware that cannot rotate, a rotation puts every frame through
         * GPU composition. [presentCounter] is the game's present counter
         * ([SessionPaths.presentCounter]); without it [counts] has no presents and the game is not
         * paced. [frameRate] is the rate asked of the display ([FrameRateVote]; 0: none).
         */
        fun start(
            surface: Surface, framebuffer: Framebuffer, header: XwdHeader, displayRotation: Int, presentCounter: File?,
            frameRate: Float,
        ): FrameCopy? {
            if (header.bitsPerPixel != 32) return null
            val handle = nativeStart(
                surface, framebuffer.descriptor(), header.pixelOffset, header.width, header.height, header.bytesPerLine,
                bufferTransform(displayRotation), presentCounter?.path, frameRate,
            )
            return if (handle == 0L) null else FrameCopy(handle)
        }

        /** The window buffer's transform (`ANATIVEWINDOW_TRANSFORM_*`) that undoes [displayRotation]. */
        fun bufferTransform(displayRotation: Int): Int = when (displayRotation) {
            Surface.ROTATION_90 -> TRANSFORM_ROTATE_270
            Surface.ROTATION_270 -> TRANSFORM_ROTATE_90
            else -> 0
        }

        private const val TRANSFORM_ROTATE_90 = 0x04
        private const val TRANSFORM_ROTATE_270 = 0x07

        @JvmStatic
        private external fun nativeStart(
            surface: Surface, fd: Int, offset: Long, width: Int, height: Int, stride: Int, transform: Int,
            presents: String?, frameRate: Float,
        ): Long

        @JvmStatic
        private external fun nativeChangedFrames(handle: Long): Long

        @JvmStatic
        private external fun nativePresents(handle: Long): Long

        @JvmStatic
        private external fun nativeShownPresents(handle: Long): Long

        @JvmStatic
        private external fun nativePostedFrames(handle: Long): Long

        @JvmStatic
        private external fun nativeSetFrameRate(handle: Long, frameRate: Float)

        @JvmStatic
        private external fun nativeResize(handle: Long, width: Int, height: Int)

        @JvmStatic
        private external fun nativeStop(handle: Long)
    }
}

/**
 * The frame rate the player picks (Settings, Game): the screen's full rate, or a reduced frame
 * rate where the screen is faster. The full rate is the rate the screen really shows when the
 * Game view asks it for its highest mode ([ScreenRate]); a phone's settings can hold it below the
 * highest mode (for example, Smooth Display off: 120 Hz modes, 60 frames a second shown). It
 * is the rate of the app's clock ([rate], [FrameClock]), which holds a game with vsync on to it
 * whatever rate the display runs at. The Game view asks the display for the same rate
 * (`ASurfaceTransaction_setFrameRate`, [vote]), and for the highest mode at the full rate: the
 * vote lets a 90 or 120 Hz phone leave 60 Hz, where the display's own heuristic would keep a layer
 * that posts 60 times a second, and lets a display run slower at a reduced rate. Only a hint to
 * the display: the game's pace never depends on it.
 */
object FrameRateVote {
    /** The lowest frame rate a player can type: Diablo II's own 25 steps a second. */
    const val MIN = 25

    /**
     * The highest refresh rate of [display]'s modes at its current resolution; 0 without a display.
     * The modes are what the panel can do, not what the phone's settings let it do ([ScreenRate]).
     */
    fun highest(display: Display?): Float {
        display ?: return 0f
        val mode = display.mode
        return display.supportedModes
            .filter { it.physicalWidth == mode.physicalWidth && it.physicalHeight == mode.physicalHeight }
            .maxOfOrNull { it.refreshRate } ?: display.refreshRate
    }

    /** The built-in display's highest mode ([highest]). */
    fun highest(context: Context): Float =
        highest(context.getSystemService(DisplayManager::class.java)?.getDisplay(Display.DEFAULT_DISPLAY))

    /**
     * The highest whole frame rate a player can type on a screen whose full rate is [full]: that
     * rate, rounded (a 60 Hz panel's modes read 60.000004 or 59.94); 0 without a display.
     */
    fun most(full: Float): Int = if (full > 0f) Math.round(full) else 0

    /**
     * The player's [pick] where the screen's full rate ([full]) is faster than it, else null: the
     * full rate (Full, kept as 0; a pick from another, faster screen; a pick under [MIN]).
     */
    fun cap(full: Float, pick: Int?): Int? = pick?.takeIf { it >= MIN && it < most(full) }

    /**
     * What the player typed ([text]) stores on a screen whose full rate is [full] and whose highest
     * mode is [highest]: a whole number from [MIN] to the screen's most; the highest mode's rate
     * as Full (0). On a screen the phone holds below its highest mode (for example, Smooth
     * Display off: 60 of 120) the most stays the number, so the pick stays 60 when the phone lets
     * the screen go faster later (the user's decision of 2026-10-23). Null for anything else.
     */
    fun typed(text: String, full: Float, highest: Float): Int? {
        val value = text.trim().toIntOrNull() ?: return null
        val most = most(full)
        return when {
            most < MIN || value < MIN || value > most -> null
            value == most(highest) -> AppSettings.FULL_FRAME_RATE
            else -> value
        }
    }

    /** The number the input shows for the stored [pick]: the pick where it applies, else the screen's most. */
    fun shown(pick: Int, full: Float): Int = cap(full, pick) ?: most(full)

    /** One step of the D-pad's up and down in the Frame rate input. */
    const val STEP = 5

    /**
     * [value] moved by [steps] of [STEP] on the step grid, inside [MIN] and the screen's most
     * ([full]): 60 up is 65, 144 down is 140, 141 up is 144 (the most).
     */
    fun stepped(value: Int, steps: Int, full: Float): Int {
        val most = most(full)
        if (most < MIN) return value
        val grid = if (steps > 0) Math.floorDiv(value, STEP) else -Math.floorDiv(-value, STEP)
        return ((grid + steps) * STEP).coerceIn(MIN, most)
    }

    /** The clock's rate on a screen whose full rate is [full]: the [pick] where it applies, else the full rate. */
    fun rate(full: Float, pick: Int?): Float = cap(full, pick)?.toFloat() ?: full

    /**
     * The rate the Game view's layers ask the display for: the [pick] where it applies on a screen
     * whose full rate is [full], else the highest mode ([highest]), so that the display runs as
     * fast as the phone lets it and [ScreenRate] sees how fast that is.
     */
    fun vote(highest: Float, full: Float, pick: Int?): Float = cap(full, pick)?.toFloat() ?: highest

    /** The time between two of the clock's ticks at [fps] ticks a second ([FrameClock]); 60 a second without a rate. */
    fun intervalNanos(fps: Float): Long = (1_000_000_000.0 / (if (fps > 0f) fps else 60f)).toLong()
}

/**
 * Frames per second from a growing frame count sampled now and then: the frames counted over
 * the last full window of at least [windowNanos], so a rate is never from a shorter stretch.
 * A count that goes back (a new copy) starts over.
 */
class FrameRate(private val windowNanos: Long = 1_000_000_000L) {
    private var startNanos = 0L
    private var startFrames = -1L

    /** The rate of the last full window, or null before the first one. */
    var fps: Float? = null
        private set

    fun sample(nowNanos: Long, frames: Long): Float? {
        if (startFrames < 0 || frames < startFrames) {
            startNanos = nowNanos
            startFrames = frames
            fps = null
        } else if (nowNanos - startNanos >= windowNanos) {
            fps = (frames - startFrames) * 1e9f / (nowNanos - startNanos)
            startNanos = nowNanos
            startFrames = frames
        }
        return fps
    }
}

/**
 * The game's present counter file (Mesa patch 0002, [SessionPaths.presentCounter]): Mesa adds one
 * to its first 32-bit word for each frame the game sends to the X screen, and only opens a file
 * that exists with at least 4 bytes, so the app makes it before the game starts. A page long, it
 * also carries the app's clock ([FrameClock], patch 0008: its ticks at byte 256, non-zero at byte
 * 320 while it ticks; a game with vsync on waits for a tick before each frame, at most 100 ms) and
 * the copy's takes of the X screen's frames (patches 0003 and 0007: the last one taken at byte 64,
 * non-zero at byte 128 while a copy runs).
 */
object PresentCounter {
    const val SIZE = 4096L

    /** Makes [file] one page of zeros, or keeps it as it is when it is that long already (a running game's count goes on). */
    fun ensure(file: File) {
        RandomAccessFile(file, "rw").use { if (it.length() < SIZE) it.setLength(SIZE) }
    }

    /** The count in [file] as it changes, through a mapping of its first word; null when it cannot be mapped. */
    fun reader(file: File): Reader? = try {
        RandomAccessFile(file, "r").use { Reader(it.channel.map(java.nio.channels.FileChannel.MapMode.READ_ONLY, 0, 4).order(ByteOrder.LITTLE_ENDIAN)) }
    } catch (_: java.io.IOException) {
        null
    }

    /** The game's presents so far ([presents]: the first word, unsigned, so it wraps at 2^32). */
    class Reader(private val word: ByteBuffer) {
        fun presents(): Long = word.getInt(0).toLong() and 0xFFFFFFFFL
    }
}

/**
 * The frame copy's counts since it started ([FrameCopy.counts]): the game's [presents] (null
 * without the present counter; a 32-bit count, so it wraps), the presents a display frame showed
 * ([shownPresents]: at most one a display frame), and the different pictures the copy found
 * ([changed]).
 */
data class FrameCounts(val presents: Long?, val shownPresents: Long, val changed: Long)

/**
 * The fps counter's reading: [fps] the game's frames per second, and [onScreen] the ones the
 * screen showed, only when that is noticeably fewer (the display's refresh rate is below the
 * game's), else null.
 */
data class FpsReading(val fps: Float, val onScreen: Float?)

/**
 * The fps counter from the copy's counts, sampled now and then: the game's presents per second;
 * where nothing comes through the present counter (no counter, or a picture drawn outside Mesa's
 * presentation) the different pictures per second instead, which are then the game's frames.
 */
class GameFrameRate(windowNanos: Long = 1_000_000_000L) {
    private val presents = FrameRate(windowNanos)
    private val shown = FrameRate(windowNanos)
    private val changed = FrameRate(windowNanos)

    fun sample(nowNanos: Long, counts: FrameCounts?): FpsReading? {
        if (counts == null) return null
        val game = counts.presents?.let { presents.sample(nowNanos, it) }
        val screen = shown.sample(nowNanos, counts.shownPresents)
        val pictures = changed.sample(nowNanos, counts.changed)
        return when {
            game != null && game > 0f -> FpsReading(game, screen?.takeIf { game - it >= ON_SCREEN_GAP })
            pictures != null -> FpsReading(pictures, null)
            else -> null
        }
    }

    companion object {
        /** The screen shows "noticeably fewer" from this many frames a second below the game's. */
        const val ON_SCREEN_GAP = 3f
    }
}
