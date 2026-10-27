package com.pd2launcher.app

/**
 * The official PD2 launcher that does not end after its window closed. On some phones its close
 * button (X) can leave PD2Launcher.exe running with no window: one of its threads (the ".NET
 * Finalizer") then uses a whole CPU core without end, the player sees only the black X screen,
 * and the engine's launcher job runs on until Close launcher.
 *
 * The service looks every [LauncherGame.POLL_MS] while the launcher's session runs ([next]): when
 * the launcher's main window had shown and no window has shown on the X screen for
 * [NO_WINDOW_MS] while the same PD2Launcher.exe still runs and none of its other programs runs
 * ([watched]), the session stops as Close launcher stops it (the engine's safe stop). So these
 * never stop it:
 * - its start, before its main window shows (that can take a minute or more), also when a
 *   message box shows before it;
 * - its update of itself: the old process ends and UpdateUtility.exe starts the new one, whose
 *   main window must show again before it counts;
 * - a game it started (Game.exe), and the first [NO_WINDOW_MS] after it;
 * - its minimize button: without a window manager its window stays on the X screen;
 * - a look at which Xvfb did not answer: it counts as a window.
 */
object LauncherHang {
    /** The launcher's process name (its `/proc/<pid>/stat` comm). */
    const val PROCESS = "PD2Launcher.exe"

    /**
     * Its other programs, as `/proc` shows them (the comm keeps the first 15 letters): the game,
     * its updater UpdateUtility.exe, and SteamPD2.exe and TempLauncher.exe.
     */
    private val OTHERS = listOf(LauncherGame.PROCESS, "UpdateUtility.e", "SteamPD2.exe", "TempLauncher.ex")

    /**
     * How long the launcher may run on without a window. Its X ends it within a few seconds, and
     * at its update of itself the old process ends within about 5 s after its window closed.
     */
    const val NO_WINDOW_MS = 20_000L

    /** The width of the smallest window that counts as the launcher's main window (800 x 600); its message boxes are smaller. */
    const val MAIN_WIDTH = 400

    /** The height of the smallest window that counts as the launcher's main window. */
    const val MAIN_HEIGHT = 300

    /**
     * What the watch knows from the looks before: [launchers], the pids of PD2Launcher.exe at the
     * last look; [windowAt], when a window last showed after their main window had shown, or when
     * the last stop was asked (null: their main window has not shown yet).
     */
    data class Watch(val launchers: Set<Int> = emptySet(), val windowAt: Long? = null)

    /** The watch after one look, and whether the session must stop now. */
    data class Step(val watch: Watch, val stop: Boolean)

    /**
     * Whether the X screen's windows count among [processes]: PD2Launcher.exe runs and none of its
     * other programs. The service asks Xvfb only then.
     */
    fun watched(processes: List<UidProcess>): Boolean =
        processes.any { it.name.equals(PROCESS, ignoreCase = true) } && !others(processes)

    /**
     * The look at [now] (elapsed milliseconds): the app's [processes], and the windows that show on
     * the X screen ([XWindows.showing]; null when Xvfb did not answer or was not asked).
     */
    fun next(watch: Watch, processes: List<UidProcess>, windows: List<XWindows.Window>?, now: Long): Step {
        val launchers = processes.filter { it.name.equals(PROCESS, ignoreCase = true) }.map { it.pid }.toSet()
        if (launchers.isEmpty()) return Step(Watch(), false)
        val busy = others(processes)
        val main = !busy && windows != null && windows.any { it.width >= MAIN_WIDTH && it.height >= MAIN_HEIGHT }
        // A new launcher process (its update of itself): only a main window of its own counts.
        if (launchers != watch.launchers) return Step(Watch(launchers, now.takeIf { main }), false)
        val windowAt = watch.windowAt ?: return Step(watch.copy(windowAt = now.takeIf { main }), false)
        // Another program of the launcher, a window, or no answer: the time starts again.
        if (busy || windows == null || windows.isNotEmpty()) return Step(watch.copy(windowAt = now), false)
        if (now - windowAt < NO_WINDOW_MS) return Step(watch, false)
        // The time starts again too: a stop the service could not send is asked again after it.
        return Step(watch.copy(windowAt = now), true)
    }

    private fun others(processes: List<UidProcess>): Boolean =
        processes.any { process -> OTHERS.any { process.name.equals(it, ignoreCase = true) } }
}
