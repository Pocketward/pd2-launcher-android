package com.pd2launcher.app

/**
 * A game that the official PD2 launcher started (its Play): the engine's launcher session goes on
 * while it runs, so the session alone does not say whether the launcher's window or the game
 * shows. The game runs when a process named [PROCESS] is among the app's own processes
 * ([UidCleanup.scan]); [SessionService] looks every [POLL_MS] while the launcher session runs, and
 * the Game view then gives the game's menu, on-screen controls and fps counter.
 */
object LauncherGame {
    /** The game client's process name (its `/proc/<pid>/stat` comm). */
    const val PROCESS = "Game.exe"

    /** How often the launcher session's processes are looked at. */
    const val POLL_MS = 1_000L

    /**
     * Whether the game runs among [processes]. Any letter case: Wine names the process as the
     * launcher wrote the file's name, and Windows file names ignore case.
     */
    fun running(processes: List<UidProcess>): Boolean = processes.any { it.name.equals(PROCESS, ignoreCase = true) }
}
