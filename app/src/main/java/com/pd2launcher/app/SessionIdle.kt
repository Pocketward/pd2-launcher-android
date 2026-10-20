package com.pd2launcher.app

/**
 * When the session ends by itself: the foreground service stops when nothing runs and the
 * Android app is closed. [SessionService] ends once [idle] has held for [stopAfter]: a short
 * time after the player closed the app, a longer one after the app only left the screen. The
 * activity starts the session again when the player comes back.
 */
object SessionIdle {
    /** How long the session waits, idle, after the player closed the app (Back on Play, or swiped it away). */
    const val CLOSED_STOP_MS = 30_000L

    /** How long it waits after the app only left the screen (Home, the screen lock, another app): a return within it finds the session. */
    const val LEFT_STOP_MS = 10 * 60_000L

    /** How often [SessionService] looks. */
    const val POLL_MS = 5_000L

    /**
     * How the app's screens went away, at [since] (elapsed real time): [closed] by the player
     * (Back on Play, or the app swiped away), or only left (Home, the screen lock, another app).
     */
    data class Away(val closed: Boolean, val since: Long)

    /**
     * Whether the session may end now ([now]: elapsed real time): none of the app's screens is
     * started ([foreground] false), the game is installed ([record], so the session goes on
     * through the whole setup), the engine is up or has given up (not while it starts or
     * restarts), and nothing runs that the end would stop: no game environment, nothing
     * [AppUpdate.waitFor] waits for (the game, the official PD2 launcher, a job or a change to the
     * installation, a folder [copying], a launch, an unpack) and no app update at work
     * ([updating]).
     */
    fun idle(
        status: SessionStatus,
        foreground: Boolean,
        record: SetupRecord,
        copying: Boolean,
        updating: Boolean,
        now: Long,
    ): Boolean =
        !foreground && record == SetupRecord.INSTALLED && !updating && !status.gameEnvironment &&
            (status.engine == SessionStatus.Engine.CONNECTED || status.engine == SessionStatus.Engine.FAILED) &&
            AppUpdate.waitFor(status, copying, now) == null

    /**
     * Whether the app update [state] needs the session: a check, a download or the download's
     * checks run, or Android's install window waits for the player. A new session at the
     * player's return would start the engine, and the update would be taken back
     * ([AppUpdate.waitFor]).
     */
    fun updating(state: AppUpdate.State): Boolean =
        state is AppUpdate.State.Checking || state is AppUpdate.State.Downloading ||
            state is AppUpdate.State.Verifying || state is AppUpdate.State.Installing

    /**
     * When the idle time began, at [now], with [idle] as it is now: null while not idle; else the
     * earlier start [since], but not before the app's screens last went [away], so a short return
     * of the app counts even when no look saw it, and closing the app after it only left the
     * screen starts the short wait from the close.
     */
    fun idleSince(since: Long?, idle: Boolean, now: Long, away: Away): Long? =
        if (idle) maxOf(since ?: now, away.since) else null

    /** How long the session waits, idle, by how the app's screens went [away]. */
    fun stopAfter(away: Away): Long = if (away.closed) CLOSED_STOP_MS else LEFT_STOP_MS

    /** Whether a session idle since [since] (elapsed real time) ends at [now], by how the app's screens went [away]. */
    fun due(since: Long, now: Long, away: Away): Boolean = now - since >= stopAfter(away)
}
