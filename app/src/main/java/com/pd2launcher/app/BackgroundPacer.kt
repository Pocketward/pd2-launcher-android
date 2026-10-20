package com.pd2launcher.app

import java.io.File

/**
 * The game's frame rate while the game is not on the screen (libframecopy.so's
 * background_pacer.c): with the app in the background, the screen off, or the app's own screens
 * over the game, no Game view takes the game's frames, and the game would draw as fast as it can
 * for no one to see. Started with the game environment while "Lower frame rate in the background"
 * is on ([AppSettings.lowFrameRateInBackground]), it holds the game to [FPS] frames a second
 * whenever no Game view paces it, through the present counter's page ([PresentCounter]).
 */
object BackgroundPacer {
    /**
     * Frames a second while the game is not on the screen: Diablo II's own game logic rate (25 game
     * frames a second), and the limit D2GL itself sets on a PC while the game's window is not the
     * active one. At least 13: the game's presents wait at most 100 ms for a frame to be taken.
     */
    const val FPS = 25

    init {
        System.loadLibrary("framecopy")
    }

    /**
     * Starts holding the game to [fps] frames a second while it is not on the screen, through the
     * page of the present counter [counter] (a pacer still running stops first); false (logged
     * under pd2-pacer) when it cannot.
     */
    fun start(counter: File, fps: Int = FPS): Boolean {
        nativeStop()
        return nativeStart(counter.path, FrameRateVote.intervalNanos(fps))
    }

    /** Stops it: the game is no longer held while it is not on the screen. */
    fun stop() = nativeStop()

    @JvmStatic
    private external fun nativeStart(counter: String, interval: Long): Boolean

    @JvmStatic
    private external fun nativeStop()
}
