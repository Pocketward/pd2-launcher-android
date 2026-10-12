package com.pd2launcher.app

import android.view.Surface
import com.pd2launcher.app.input.GameWindow
import java.io.File
import java.nio.ByteOrder
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

    /** The Game view's [surface] carries the game's layer, asking the display for [frameRate]. */
    fun attach(surface: Surface, frameRate: Float) = synchronized(lock) {
        if (!stopped) nativeAttach(handle, surface, frameRate)
    }

    /**
     * The Game view's [surface] is going: its layer goes, and every buffer it held goes back to the
     * game; a layer a newer surface was given meanwhile stays.
     */
    fun detach(surface: Surface) = synchronized(lock) {
        if (!stopped) nativeDetach(handle, surface)
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
     * [read] the X screen as the game's buffer on the screen shows it ([ShownFrame]); null while
     * the X screen itself shows. Off the main thread.
     */
    fun <T> readShown(read: (ShownFrame) -> T): T? = synchronized(lock) {
        if (stopped) return null
        val size = IntArray(7)
        val pixels = nativeLockShown(handle, size) ?: return null
        try {
            val buffer = ScreenPixels(XwdHeader(size[0], size[1], 32, size[2], 0), pixels.order(ByteOrder.LITTLE_ENDIAN))
            read(ShownFrame(buffer, size[3], size[4], size[5], size[6]))
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

        /** The running server stops: the game's frames go to the X screen. */
        fun stopCurrent() {
            val server = _current.value ?: return
            _current.value = null
            server.stop()
        }

        @JvmStatic
        private external fun nativeStart(socket: String, counter: String?): Long

        @JvmStatic
        private external fun nativeAttach(handle: Long, surface: Surface, frameRate: Float)

        @JvmStatic
        private external fun nativeDetach(handle: Long, surface: Surface)

        @JvmStatic
        private external fun nativeSetWindow(
            handle: Long, covered: Boolean, x: Int, y: Int, width: Int, height: Int, screenWidth: Int, screenHeight: Int,
        )

        @JvmStatic
        private external fun nativeLockShown(handle: Long, size: IntArray): java.nio.ByteBuffer?

        @JvmStatic
        private external fun nativeUnlockShown(handle: Long)

        @JvmStatic
        private external fun nativeStop(handle: Long)
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
