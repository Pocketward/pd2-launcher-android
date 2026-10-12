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
    companion object {
        const val SIZE = 100
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
 * on Android 10+ into a child layer of the Surface set by transactions, not through the Surface's
 * buffer queue, which some devices pace to 30 frames a second. While it runs it paces the
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
        private external fun nativeStop(handle: Long)
    }
}

/**
 * The frame rate the Game view asks the display for: the panel's full rate, or [CAP] when the
 * player limits it (Settings, Game) on a faster screen. The vote
 * (`ASurfaceTransaction_setFrameRate`, Android 11+) lets a 90 or 120 Hz phone leave 60 Hz, where
 * the display's own heuristic would keep a layer that posts 60 times a second.
 */
object FrameRateVote {
    const val CAP = 60f

    /** The highest refresh rate [display] offers at its current resolution; 0 without a display. */
    fun highest(display: Display?): Float {
        display ?: return 0f
        val mode = display.mode
        return display.supportedModes
            .filter { it.physicalWidth == mode.physicalWidth && it.physicalHeight == mode.physicalHeight }
            .maxOfOrNull { it.refreshRate } ?: display.refreshRate
    }

    /** The built-in display's highest refresh rate ([highest]). */
    fun highest(context: Context): Float =
        highest(context.getSystemService(DisplayManager::class.java)?.getDisplay(Display.DEFAULT_DISPLAY))

    /** Whether the limit means anything on a screen whose highest rate is [highest]: one faster than [CAP]. */
    fun limitApplies(highest: Float): Boolean = highest > CAP + 1f

    /** The rate to ask for on a screen whose highest rate is [highest], [limited] to [CAP] or not. */
    fun vote(highest: Float, limited: Boolean): Float = if (limited && limitApplies(highest)) CAP else highest
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
 * also carries the Game view's pacing (patch 0003): the last present the copy has taken at byte
 * 64, and at byte 128 a word that is non-zero while the copy runs; Mesa holds each present (vsync
 * on or off) back until the one before is taken (at most 100 ms), so the game draws one frame per
 * display frame.
 */
object PresentCounter {
    const val SIZE = 4096L

    /** Makes [file] one page of zeros, or keeps it as it is when it is that long already (a running game's count goes on). */
    fun ensure(file: File) {
        RandomAccessFile(file, "rw").use { if (it.length() < SIZE) it.setLength(SIZE) }
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
