package com.pd2launcher.app

import com.pd2launcher.app.control.Job

/**
 * The official PD2 launcher's start as the Game view shows it. From the start of its session
 * until its first window shows, the X screen stays black: PD2Launcher.exe, a .NET program, can
 * work for a minute or more on a slow phone before it shows a window. Meanwhile a notice over the
 * black screen says that the launcher starts, under a spinner ([R.string.launcher_starting]), and
 * from [SLOW_MS] on ([slow]) that it can take a minute on slower phones
 * ([R.string.launcher_starting_slow]). The notice goes when the X screen shows more than the
 * X pointer ([shows]: the launcher's window, or a message it shows first), and does not come back
 * in that session; it also goes with the session.
 */
object LauncherStart {
    /** Samples across and down the X screen. */
    private const val COLUMNS = 64
    private const val ROWS = 36

    /**
     * Samples that are not black that tell a window: more than the X pointer (at most 32 x 32
     * pixels) covers on an X screen of 640 x 480 or more, much fewer than the smallest message box.
     */
    const val DRAWN = 16

    /** A colour channel above this is not black. */
    private const val DARK = 8

    /** How often the Game view looks at the X screen while the notice shows. */
    const val LOOK_MS = 500L

    /** How long the launcher starts (ms) before the notice also says that slower phones can take a minute. */
    const val SLOW_MS = 20_000L

    /**
     * When the official launcher's session started (elapsed real time), as the session's status
     * keeps it ([SessionStatus.launcherSince]): [now] when its job starts ([next] after
     * [previous]), [since] while it goes on, null while it does not run.
     */
    fun since(since: Long?, previous: Job?, next: Job?, now: Long): Long? = when {
        next != Job.OFFICIAL_LAUNCHER -> null
        previous != Job.OFFICIAL_LAUNCHER || since == null -> now
        else -> since
    }

    /**
     * Whether a [width] x [height] X screen whose pixel at x, y is [pixel] (32 bits, the colour in
     * the low 24) shows a window: at least [DRAWN] of its samples are not black. The X screen's
     * root window is black, and the X pointer alone is too small.
     */
    fun shows(width: Int, height: Int, pixel: (Int, Int) -> Int): Boolean {
        if (width <= 0 || height <= 0) return false
        var drawn = 0
        for (row in 0 until ROWS) for (column in 0 until COLUMNS) {
            val color = pixel(width * (2 * column + 1) / (2 * COLUMNS), height * (2 * row + 1) / (2 * ROWS))
            if ((color and 0xFF) > DARK || (color shr 8 and 0xFF) > DARK || (color shr 16 and 0xFF) > DARK) {
                if (++drawn >= DRAWN) return true
            }
        }
        return false
    }

    /** Whether [SLOW_MS] or more have passed from [since] to [now] (elapsed real time). */
    fun slow(since: Long, now: Long): Boolean = now - since >= SLOW_MS
}
