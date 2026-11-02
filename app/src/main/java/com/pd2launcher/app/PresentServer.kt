package com.pd2launcher.app

import android.view.Surface
import com.pd2launcher.app.input.GameWindow
import java.io.File
import java.nio.ByteOrder
import java.util.Locale
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

/**
 * The game's direct present (libframecopy.so's present_server.cpp,
 * the app's side of Mesa patch 0004): the game's Vulkan swapchain renders into AHardwareBuffers
 * the app allocates, and the Game view shows them on a layer of their own above the X screen's
 * copy, where the game's window is on the X screen, paced by the panel. It listens on
 * [SessionPaths.presentSocket] for the game environment's lifetime; the Game view [attach]es its
 * surface, and x-input-relay's word on the X screen's windows ([setGameWindow]) places the layer,
 * or sends the frames back to the X screen while anything else shows there. Where Mesa cannot
 * use it (a GPU that refuses the buffers, a compressed buffer) the game presents to the X screen
 * instead.
 */
class PresentServer private constructor(private val handle: Long) {
    private val lock = Any()
    @Volatile private var stopped = false

    /**
     * The Game view's [surface] carries the game's layer, asking the display for [frameRate];
     * [capped]: a Frame rate below the screen's ([FrameRateVote.cap]), which the screen shows a game
     * with vsync off at.
     */
    fun attach(surface: Surface, frameRate: Float, capped: Boolean) = synchronized(lock) {
        if (!stopped) nativeAttach(handle, surface, frameRate, capped)
    }

    /** The Game view's [surface] has a new size ([width] x [height]): the game's layer stays and shows at it. */
    fun resize(surface: Surface, width: Int, height: Int) = synchronized(lock) {
        if (!stopped) nativeResize(handle, surface, width, height)
    }

    /** The game's layer asks the display for [frameRate] from now on ([FrameRateVote]); [capped] as for [attach]. */
    fun setFrameRate(frameRate: Float, capped: Boolean) = synchronized(lock) {
        if (!stopped) nativeSetFrameRate(handle, frameRate, capped)
    }

    /**
     * The Game view's [surface] is going: its layer goes, and every buffer it held goes back to the
     * game; a layer a newer surface was given meanwhile stays.
     */
    fun detach(surface: Surface) = synchronized(lock) {
        if (!stopped) nativeDetach(handle, surface)
    }

    /** The display shows at [hertz] now (0: not known): each frame's time on the screen counts in its refreshes ([counts]). */
    fun setRefreshRate(hertz: Float) = synchronized(lock) {
        if (!stopped) nativeSetRefreshRate(handle, hertz)
    }

    /** The counts since the server started ([PresentCounts]); null once it stopped. */
    fun counts(): PresentCounts? = synchronized(lock) {
        if (stopped) return null
        val values = LongArray(PresentCounts.SIZE)
        nativeCounts(handle, values)
        PresentCounts.of(values)
    }

    /**
     * Detailed logging's pointer split ([PointerSplit]): [on] while Detailed logging is on and the
     * game runs. Mesa times the game's frames only while it is on; a change forgets the moves and
     * the window. The last window's summary; null once the server stopped.
     */
    fun split(on: Boolean): PointerSplit? = synchronized(lock) {
        if (stopped) return null
        val values = LongArray(PointerSplit.SIZE)
        nativeSplit(handle, on, values)
        PointerSplit.of(values)
    }

    /**
     * The game's window alone on the X screen, where it is ([window]); null while it is not (a
     * window over or beside it, or none named yet): the X screen shows.
     */
    fun setGameWindow(window: GameWindow?) {
        synchronized(lock) {
            if (stopped) return
            if (window == null) {
                nativeSetWindow(handle, true, 0, 0, 0, 0, 0, 0)
            } else {
                nativeSetWindow(handle, false, window.x, window.y, window.width, window.height, window.screenWidth, window.screenHeight)
            }
        }
    }

    /**
     * [read] the X screen as the game's buffer on the screen shows it ([ShownFrame]):
     * [Shown.Read]; [Shown.NotReady] while that buffer shows but cannot be read now (its GPU
     * writes have not finished: the X screen does not show that frame either); null while the X
     * screen itself shows. Off the main thread.
     */
    fun <T> readShown(read: (ShownFrame) -> T): Shown<T>? = synchronized(lock) {
        if (stopped) return null
        // Seven numbers of the frame read, then the native side's word when it read none.
        val size = IntArray(8)
        val pixels = nativeLockShown(handle, size) ?: return if (size[7] == SHOWN_NOT_READY) Shown.NotReady else null
        try {
            val buffer = ScreenPixels(XwdHeader(size[0], size[1], 32, size[2], 0), pixels.order(ByteOrder.LITTLE_ENDIAN))
            Shown.Read(read(ShownFrame(buffer, size[3], size[4], size[5], size[6])))
        } finally {
            nativeUnlockShown(handle)
        }
    }

    private fun stop() = synchronized(lock) {
        if (stopped) return
        stopped = true
        nativeStop(handle)
    }

    companion object {
        init {
            System.loadLibrary("framecopy")
        }

        /** nativeLockShown's word (the size array's last number): the game's buffer shows, not ready to read. */
        private const val SHOWN_NOT_READY = 1

        private val _current = MutableStateFlow<PresentServer?>(null)
        /** The running game environment's server, for the Game view; null while it is stopped. */
        val current: StateFlow<PresentServer?> = _current.asStateFlow()

        /**
         * Listens on [socket] (any old one replaced), telling Mesa where its frames go through the
         * present counter's page [counter] ([PresentCounter]); null when it cannot.
         */
        fun start(socket: File, counter: File?): PresentServer? {
            stopCurrent()
            val handle = nativeStart(socket.path, counter?.path)
            return if (handle == 0L) null else PresentServer(handle).also { _current.value = it }
        }

        /**
         * Detailed logging: the Game view wrote a pointer move into x-input-relay's socket at
         * [nanos] ([System.nanoTime]), for the running server's pointer split ([split]). Without a
         * server, or while its split is off, nothing. Never waits for the server's lock.
         */
        fun pointerMoved(nanos: Long) = nativePointerMoved(nanos)

        /** The running server stops: the game's frames go to the X screen. */
        fun stopCurrent() {
            val server = _current.value ?: return
            _current.value = null
            server.stop()
        }

        /** The Game view shows the game's own buffers over the whole view. */
        const val SHOWING_WHOLE = 1

        /** The Game view shows the game's own buffers over the game's window only (a windowed game's); the X screen's copy around it. */
        const val SHOWING_WINDOW = 2

        /**
         * Whether the Game view shows the game's own buffers now (present_server.h): 0 no (the X
         * screen's copy shows the game, with Xvfb's own pointer), [SHOWING_WHOLE] or [SHOWING_WINDOW].
         */
        fun showing(): Int = nativeShowing()

        @JvmStatic
        private external fun nativeShowing(): Int

        @JvmStatic
        private external fun nativeStart(socket: String, counter: String?): Long

        @JvmStatic
        private external fun nativeAttach(handle: Long, surface: Surface, frameRate: Float, capped: Boolean)

        @JvmStatic
        private external fun nativeDetach(handle: Long, surface: Surface)

        @JvmStatic
        private external fun nativeResize(handle: Long, surface: Surface, width: Int, height: Int)

        @JvmStatic
        private external fun nativeSetFrameRate(handle: Long, frameRate: Float, capped: Boolean)

        @JvmStatic
        private external fun nativeSetWindow(
            handle: Long, covered: Boolean, x: Int, y: Int, width: Int, height: Int, screenWidth: Int, screenHeight: Int,
        )

        @JvmStatic
        private external fun nativeSetRefreshRate(handle: Long, hertz: Float)

        @JvmStatic
        private external fun nativeCounts(handle: Long, out: LongArray)

        @JvmStatic
        private external fun nativeSplit(handle: Long, on: Boolean, out: LongArray)

        @JvmStatic
        private external fun nativePointerMoved(nanos: Long)

        @JvmStatic
        private external fun nativeLockShown(handle: Long, size: IntArray): java.nio.ByteBuffer?

        @JvmStatic
        private external fun nativeUnlockShown(handle: Long)

        @JvmStatic
        private external fun nativeStop(handle: Long)
    }
}

/**
 * The direct present's counts since its server started (present_server.cpp, counts()): the
 * swapchains that became the active one so far ([swapchain]: the active one's number), the active
 * one's size, the mode of its last PRESENT ([mode]: the VkPresentModeKHR + 1, 0 when Mesa sends
 * none) and its PRESENTs ([swapchainPresents]); the PRESENTs by mode ([byMode]: IMMEDIATE,
 * MAILBOX, FIFO, FIFO_RELAXED, none sent); the frames by their time on the screen in refreshes of
 * the display ([onScreen]: 1, 2, 3, 4, 5 or more) and the sum of those times; how long the layer
 * showed; the display's refresh period; and the last 5 s line ([stats]: its number, 0 for none;
 * the rates, the longest gap, where the frames showed: [WHERE]).
 */
data class PresentCounts(
    val swapchain: Long,
    val width: Int,
    val height: Int,
    val mode: Int,
    val swapchainPresents: Long,
    val byMode: List<Long>,
    val onScreen: List<Long>,
    val onScreenNanos: Long,
    val visibleNanos: Long,
    val refreshNanos: Long,
    val stats: Long = 0,
    val presentsPerSecond: Double = 0.0,
    val shownPerSecond: Double = 0.0,
    val longestGapMs: Double = 0.0,
    val backPerSecond: Double = 0.0,
    val where: Int = 0,
) {
    companion object {
        /** How many numbers nativeCounts gives (present_server.cpp's COUNTS). */
        const val SIZE = 24

        /** The modes in [byMode]'s order; the last: none sent. */
        val MODES = listOf("IMMEDIATE", "MAILBOX", "FIFO", "FIFO_RELAXED", "not sent")

        /** [where]'s words, as the native 5 s line has them (present_server.cpp's WHERE). */
        val WHERE = listOf("", "showing", "showing in the game's window", "the X screen", "no layer: the X screen", "no view: the newest frame kept")

        val NONE = PresentCounts(0, 0, 0, 0, 0, List(5) { 0L }, List(5) { 0L }, 0, 0, 0)

        fun of(v: LongArray) = PresentCounts(
            v[0], v[1].toInt(), v[2].toInt(), v[3].toInt(), v[4], v.slice(5..9), v.slice(10..14), v[15], v[16], v[17],
            v[18], v[19] / 1000.0, v[20] / 1000.0, v[21] / 1000.0, v[22] / 1000.0, v[23].toInt(),
        )
    }
}

/**
 * Detailed logging's split of the pointer's delay after the Game view's write of a move into
 * x-input-relay's socket, for the last window of the direct present (present_server.cpp,
 * pointer_split.h; a window ends 5 s after its first frame, or at 2048 moves): its number
 * ([serial], 0: none yet), the time from its first frame to its last ([spanNanos]), the [moves]
 * counted, the [frames] that showed them, and the moves [notCounted] (no time on the screen, a wait
 * of more than 500 ms for a frame, or too many); each part's median and 95th percentile in
 * microseconds ([medianMicros], [p95Micros], in [PARTS]' order; -1: not known, clock -> latch
 * without latch times). Each move counts with the first frame shown whose acquire came at or after
 * its write. Mesa's acquire and present are on D2GL's render thread, which draws a frame that the
 * game's thread made before (D2GL's frame latency), so the pointer in a frame can be older than its
 * acquire; the line says so.
 */
data class PointerSplit(
    val serial: Long,
    val spanNanos: Long,
    val moves: Int,
    val frames: Int,
    val notCounted: Int,
    val medianMicros: List<Long>,
    val p95Micros: List<Long>,
) {
    /** The service log's line for this window. */
    fun line(): String {
        val head = String.format(Locale.ROOT, "pointer split (5 s): %d moves in %.1f s", moves, spanNanos / 1e9)
        if (moves == 0) return "$head, $notCounted not counted (no time on the screen, a wait of more than 500 ms for a frame, or too many)"
        fun part(i: Int) = if (medianMicros[i] < 0) {
            "${PARTS[i]} not known"
        } else {
            String.format(Locale.ROOT, "%s %.1f / %.1f", PARTS[i], medianMicros[i] / 1e3, p95Micros[i] / 1e3)
        }
        return "$head, shown by $frames frames, $notCounted not counted; median / p95 in ms: " +
            "${part(0)}; ${part(1)}; ${part(2)} (${part(3)}); ${part(4)}; ${part(5)}; ${part(6)}; ${part(7)}; $RENDER_THREAD"
    }

    companion object {
        /**
         * The parts, as pointer_split.h's enum split_part has them: the times are the app's write,
         * Mesa's four, the app's transaction, the compositor's latch and the present fence.
         */
        val PARTS = listOf(
            "write -> acquire", "acquire -> present", "present -> clock", "present -> GPU done", "clock -> apply", "apply -> screen",
            "clock -> latch", "write -> screen",
        )

        /** The end of each line: whose times Mesa's are. */
        const val RENDER_THREAD = "acquire and present are D2GL's render thread's: the pointer in a frame can be older than its acquire"

        /** How many numbers nativeSplit gives (present_server.cpp's SPLIT_COUNTS). */
        val SIZE = 5 + 2 * PARTS.size

        fun of(v: LongArray) = PointerSplit(
            v[0], v[1], v[2].toInt(), v[3].toInt(), v[4].toInt(), v.slice(5 until 5 + PARTS.size), v.slice(5 + PARTS.size until SIZE),
        )
    }
}

/** What [PresentServer.readShown] found while the game's buffer shows. */
sealed interface Shown<out T> {
    /** The buffer on the screen, read. */
    data class Read<out T>(val value: T) : Shown<T>

    /** The buffer shows, but cannot be read now (its GPU writes have not finished). */
    data object NotReady : Shown<Nothing>

    companion object {
        /**
         * One sample of what shows: the buffer read, else the X screen ([xScreen]) while the X screen
         * shows; null (no sample) while the buffer shows but is not ready, as the X screen does not
         * show that frame.
         */
        fun <T> sample(shown: Shown<T>?, xScreen: () -> T): T? = when (shown) {
            null -> xScreen()
            is Read -> shown.value
            NotReady -> null
        }
    }
}

/**
 * The X screen ([width] x [height]) as the game's buffer on the screen shows it: the buffer's
 * pixels ([buffer], R8G8B8A8 words) where the game's window is, its top-left corner at [left],
 * [top], and opaque black around it (the X screen's root window there). Alpha is opaque throughout,
 * so a sample's colour does not depend on which side of the window's edge it falls.
 */
class ShownFrame(private val buffer: ScreenPixels, private val left: Int, private val top: Int, val width: Int, val height: Int) {
    /** The pixel at ([x], [y]) on the X screen. */
    fun pixel(x: Int, y: Int): Int {
        val bx = x - left
        val by = y - top
        if (bx < 0 || by < 0 || bx >= buffer.header.width || by >= buffer.header.height) return OPAQUE_BLACK
        return buffer.pixel(bx, by) or OPAQUE_BLACK
    }

    private companion object {
        const val OPAQUE_BLACK = 0xFF000000.toInt()
    }
}
