package com.pd2launcher.app

import java.io.File

/**
 * The app's clock (libframecopy.so's frame_clock.c): the virtual display's vertical blank. A game
 * with vsync on (D2GL's default) waits for its tick before each frame (Mesa patch 0008), so it
 * draws at most the rate the player picked ([AppSettings.frameRate]; Full: the screen's full rate,
 * [ScreenRate]), on every screen; a game with vsync off is never held. It runs for the game environment's
 * lifetime, through the present counter's page ([PresentCounter]); the Game view sets its rate.
 */
object FrameClock {
    init {
        System.loadLibrary("framecopy")
    }

    /**
     * Starts the clock at [fps] ticks a second through the page of the present counter [counter] (a
     * clock still running stops first); false (logged under pd2-clock) when it cannot.
     */
    fun start(counter: File, fps: Float): Boolean {
        nativeStop()
        return nativeStart(counter.path, FrameRateVote.intervalNanos(fps))
    }

    /** The clock ticks [fps] times a second from now on. */
    fun setRate(fps: Float) = nativeSetInterval(FrameRateVote.intervalNanos(fps))

    /** Stops it: no game waits for it any more. */
    fun stop() = nativeStop()

    @JvmStatic
    private external fun nativeStart(counter: String, interval: Long): Boolean

    @JvmStatic
    private external fun nativeSetInterval(interval: Long)

    @JvmStatic
    private external fun nativeStop()
}
