package com.pd2launcher.app

/**
 * The official PD2 launcher that runs on when the player is done with it. On some phones its close
 * button (X) can leave PD2Launcher.exe running with no window: one of its threads (the ".NET
 * Finalizer") then uses a whole CPU core without end, the player sees only the black X screen,
 * and the engine's launcher job runs on until Close launcher. Its Play starts the game
 * (Game.exe) and leaves the launcher running beside it.
 *
 * The service looks every [LauncherGame.POLL_MS] while the launcher's session runs ([next]), and
 * the launcher stops ([Stop]) when:
 * - [Why.NO_WINDOW]: its main window had shown and no window has shown on the X screen for
 *   [NO_WINDOW_MS] while the same PD2Launcher.exe still runs and none of its other programs runs
 *   ([watched]). The session stops as Close launcher stops it (the engine's safe stop).
 * - [Why.GAME]: its game has run for [GAME_MS] and none of its updaters runs. Only
 *   PD2Launcher.exe ends; the game runs on, and the session ends with it.
 *
 * So these never stop it:
 * - its start, before its main window shows (that can take a minute or more), also when a
 *   message box shows before it;
 * - its update of itself: the old process ends and UpdateUtility.exe starts the new one, whose
 *   main window must show again before it counts;
 * - its minimize button: without a window manager its window stays on the X screen;
 * - a look at which Xvfb did not answer: it counts as a window.
 */
object LauncherHang {
    /** The launcher's process name (its `/proc/<pid>/stat` comm). */
    const val PROCESS = "PD2Launcher.exe"

    /**
     * Its updaters, as `/proc` shows them (the comm keeps the first 15 letters): UpdateUtility.exe,
     * and SteamPD2.exe and TempLauncher.exe.
     */
    private val UPDATERS = listOf("UpdateUtility.e", "SteamPD2.exe", "TempLauncher.ex")

    /**
     * How long the launcher may run on without a window. Its X ends it within a few seconds; at
     * its update of itself UpdateUtility.exe runs before its window closes.
     */
    const val NO_WINDOW_MS = 3_000L

    /** How long its game runs before the launcher stops: a game that ends at once leaves the launcher open. */
    const val GAME_MS = 2_000L

    /** How long PD2Launcher.exe has after SIGTERM, beside its game, before SIGKILL. */
    const val END_GRACE_MS = 1_000L

    /** The width of the smallest window that counts as the launcher's main window (800 x 600); its message boxes are smaller. */
    const val MAIN_WIDTH = 400

    /** The height of the smallest window that counts as the launcher's main window. */
    const val MAIN_HEIGHT = 300

    /**
     * What the watch knows from the looks before: [launchers], the pids of PD2Launcher.exe at the
     * last look; [windowAt], when a window last showed after their main window had shown, or when
     * the last stop was asked (null: their main window has not shown yet); [gameAt], when their
     * game was first seen, or when the last stop for it was asked (null: it does not run).
     */
    data class Watch(val launchers: Set<Int> = emptySet(), val windowAt: Long? = null, val gameAt: Long? = null)

    /** Why the launcher stops. */
    enum class Why { NO_WINDOW, GAME }

    /** A stop: [why], and [afterMs], how long no window had shown ([Why.NO_WINDOW]) or the game had run ([Why.GAME]). */
    data class Stop(val why: Why, val afterMs: Long)

    /** The watch after one look, and the stop to make now (null: none). */
    data class Step(val watch: Watch, val stop: Stop? = null)

    /**
     * Whether the X screen's windows count among [processes]: PD2Launcher.exe runs and none of its
     * other programs. The service asks Xvfb only then.
     */
    fun watched(processes: List<UidProcess>): Boolean =
        processes.any { it.name.equals(PROCESS, ignoreCase = true) } && !updating(processes) && !LauncherGame.running(processes)

    /**
     * The look at [now] (elapsed milliseconds): the app's [processes], and the windows that show on
     * the X screen ([XWindows.showing]; null when Xvfb did not answer or was not asked).
     */
    fun next(watch: Watch, processes: List<UidProcess>, windows: List<XWindows.Window>?, now: Long): Step {
        val launchers = processes.filter { it.name.equals(PROCESS, ignoreCase = true) }.map { it.pid }.toSet()
        if (launchers.isEmpty()) return Step(Watch())
        val updating = updating(processes)
        val game = !updating && LauncherGame.running(processes)
        val main = !updating && !game && windows != null && windows.any { it.width >= MAIN_WIDTH && it.height >= MAIN_HEIGHT }
        // A new launcher process (its update of itself): only a main window of its own counts.
        if (launchers != watch.launchers) return Step(Watch(launchers, now.takeIf { main }, now.takeIf { game }))
        if (game) {
            val gameAt = watch.gameAt ?: now
            // The window's time starts again, once the main window had shown.
            val windowAt = watch.windowAt?.let { now }
            if (now - gameAt < GAME_MS) return Step(Watch(launchers, windowAt, gameAt))
            // The time starts again too: a stop that did not take is asked again after it.
            return Step(Watch(launchers, windowAt, now), Stop(Why.GAME, now - gameAt))
        }
        val windowAt = watch.windowAt ?: return Step(Watch(launchers, now.takeIf { main }))
        // An updater (with the game or not), a window, or no answer: the time starts again.
        if (updating || windows == null || windows.isNotEmpty()) return Step(Watch(launchers, now))
        if (now - windowAt < NO_WINDOW_MS) return Step(Watch(launchers, windowAt))
        return Step(Watch(launchers, now), Stop(Why.NO_WINDOW, now - windowAt))
    }

    private fun updating(processes: List<UidProcess>): Boolean =
        processes.any { process -> UPDATERS.any { process.name.equals(it, ignoreCase = true) } }
}
