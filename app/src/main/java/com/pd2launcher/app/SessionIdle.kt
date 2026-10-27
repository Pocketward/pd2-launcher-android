package com.pd2launcher.app

/**
 * When the session ends by itself: the session service stops when nothing runs that the end would
 * stop (all that [BackgroundWork.keeps] counts, and also the engine's start and an app update's
 * check or install window) and the Android app is not on the screen, also during the setup.
 * [SessionService] ends once [idle] has held for [stopAfter]: at once after the player closed the
 * app, a short time after the app only left the screen. The activity starts the session again
 * when the player comes back.
 */
object SessionIdle {
    /** How long the session waits, idle, after the player closed the app (Back on Play, or swiped it away): it ends at once. */
    const val CLOSED_STOP_MS = 0L

    /**
     * How long it waits after the app only left the screen (Home, the screen lock, another app): a
     * return within it, as after a quick look at another app, finds the session, and the engine
     * does not start again.
     */
    const val LEFT_STOP_MS = 10_000L

    /** How often [SessionService] looks. */
    const val POLL_MS = 2_000L

    /**
     * How the app's screens went away, at [since] (elapsed real time): [closed] by the player
     * (Back on Play, or the app swiped away), or only left (Home, the screen lock, another app).
     */
    data class Away(val closed: Boolean, val since: Long)

    /**
     * Whether the session may end now ([now]: elapsed real time): none of the app's screens is
     * started ([foreground] false), the engine is up or has given up (not while it starts or
     * restarts), and nothing runs that the end would stop: no game environment of an install
     * ([installEnvironment]; one left with nothing in it after a launch that failed does not
     * count), nothing [AppUpdate.waitFor] waits for (the game, the official PD2 launcher, a job or
     * a change to the installation, a folder [copying], a launch, an unpack) and no app update at
     * work ([updating]). Whatever [BackgroundWork.keeps] counts, this counts too.
     */
    fun idle(
        status: SessionStatus,
        foreground: Boolean,
        installEnvironment: Boolean,
        copying: Boolean,
        updating: Boolean,
        now: Long,
    ): Boolean =
        !foreground && !updating && !(installEnvironment && status.gameEnvironment) &&
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
     * screen starts the wait from the close.
     */
    fun idleSince(since: Long?, idle: Boolean, now: Long, away: Away): Long? =
        if (idle) maxOf(since ?: now, away.since) else null

    /** How long the session waits, idle, by how the app's screens went [away]. */
    fun stopAfter(away: Away): Long = if (away.closed) CLOSED_STOP_MS else LEFT_STOP_MS

    /** Whether a session idle since [since] (elapsed real time) ends at [now], by how the app's screens went [away]. */
    fun due(since: Long, now: Long, away: Away): Boolean = now - since >= stopAfter(away)
}
