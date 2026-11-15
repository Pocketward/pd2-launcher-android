package com.pd2launcher.app

/**
 * What the app's logs record, by area, at each level. Basic is always on; Detailed adds to it while
 * Settings → Help → Detailed logging is on ([AppSettings.detailedLogging]). This is the one place
 * that says which area is which: every logging place asks [LogPolicy]. Basic is the gateway (the
 * owner, 2026-11-14): enough to tell what kind of problem a player has and to set the player's game
 * up again, all of it cheap; Detailed gives the rest, at a cost in CPU time and log size.
 * The players' page LOGS.md (next to the FAQ) has the same list.
 */
enum class LogArea(val detailedOnly: Boolean) {
    /** Save logs: every app setting (settings.txt) and PD2's settings files (game-config/). */
    SETTINGS_FILES(false),

    /** While the game runs, the perf line ([PerfWatch]): one for each [LogPolicy.windowMs]. */
    PERF(false),

    /** While the game runs, the frame split ([FrameSplit]): one for each [LogPolicy.windowMs]. */
    FRAME_SPLIT(false),

    /** The direct present's 5 s statistics line, with the clock's rate. */
    PRESENT_STATS(true),

    /** The pointer split ([PointerSplit]): the pointer's delay by part, while the pointer moves. */
    POINTER_SPLIT(true),

    /** The Game view's input lines and the pads' states. */
    INPUT(true),

    /** D2GL's settings at the game's start and at each change. */
    D2GL_SETTINGS(true),

    /** A line for each file that the save sync copies. */
    SAVE_SYNC_FILES(true),

    /** Every engine line of an official launcher's run (else only its first ones). */
    LAUNCHER_LINES(true),

    /** Play's server status: each change and each failed fetch. */
    SERVER_STATUS(true),

    /**
     * The engine's Detailed logging, sent with each Play and each press of the official launcher:
     * Wine's and Box64's own logs, the game's network events, and output lines of 1000 characters
     * (else 300).
     */
    ENGINE(true),
}

/** Whether each [LogArea] logs now, and how often the game's summaries come. */
object LogPolicy {
    /** Basic: a perf line and a frame split every 30 s while the game runs. */
    const val BASIC_WINDOW_MS = 30_000L

    /** Detailed: every 10 s. */
    const val DETAILED_WINDOW_MS = 10_000L

    /** Whether Detailed logging is on now. */
    val detailed: Boolean get() = AppSettingsStore.current.value.detailedLogging

    /** Whether [area] logs, with Detailed logging [detailed] (now's by default). */
    fun on(area: LogArea, detailed: Boolean = this.detailed): Boolean = !area.detailedOnly || detailed

    /** How long one perf line or frame split covers, with Detailed logging [detailed] (now's by default). */
    fun windowMs(detailed: Boolean = this.detailed): Long = if (detailed) DETAILED_WINDOW_MS else BASIC_WINDOW_MS
}
