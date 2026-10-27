package com.pd2launcher.app

import com.pd2launcher.app.control.Game
import com.pd2launcher.app.control.Job

/**
 * What must go on while the app is in the background, and how the notification's Close stops it.
 * [SessionService] is a foreground service, with its notification, only while something [keeps]
 * it: the game, the official PD2 launcher, or work that changes the installation or downloads
 * (any engine job, so a new kind of job counts by itself; the unpack of the game support files;
 * the folder copy of an import; the download of an app update and its checks). When nothing
 * keeps it, the session has no notification, and it ends a short time after the app's screens
 * went away ([SessionIdle]).
 */
object BackgroundWork {
    /** What runs in the engine's part of the session, and so how Close stops it ([stop]). */
    enum class Stop {
        /** The official PD2 launcher's session, with a game it started: the engine's safe stop, as Close launcher. */
        OFFICIAL_LAUNCHER,

        /** The game, or its start after Play: the game environment stops, as Stop game in the game's menu. */
        GAME,

        /**
         * An engine job of any kind, or its change to the installation: the engine's safe stop, as
         * Stop. A stopped install pauses, and Resume continues it later.
         */
        JOB,

        /** Nothing runs in the engine's part. */
        NOTHING,
    }

    /**
     * How long Close waits for [stop] to finish ([stopped]). The game ends with its game
     * environment, so the engine only has to see it end: [GAME_STOP_MS]. The app waits for the
     * engine's safe stop without a limit of its own, as Stop and Close launcher do; the engine
     * gives a stuck installer window up to ten minutes, and [SAFE_STOP_MS] is longer still, so
     * Close always ends.
     */
    fun waitMs(stop: Stop): Long = when (stop) {
        Stop.GAME -> GAME_STOP_MS
        Stop.OFFICIAL_LAUNCHER, Stop.JOB -> SAFE_STOP_MS
        Stop.NOTHING -> 0
    }

    /** The longest Close waits for the engine to see the game end. */
    const val GAME_STOP_MS = 10_000L

    /** The longest Close waits for the engine's safe stop: longer than the engine's own ten minutes. */
    const val SAFE_STOP_MS = 15 * 60_000L

    /**
     * The longest Close waits for a folder copy to stop and remove what it copied
     * ([SourceImport.cancel]): one block to read, then a few gigabytes of files to delete. Past it,
     * the next session start removes the rest ([SourceImport.removePartialCopies]).
     */
    const val COPY_STOP_MS = 30_000L

    /**
     * What runs in the engine's part at [now] (elapsed real time), as [AppUpdate.waitFor] sees it,
     * and so how Close stops it. The official PD2 launcher comes first: a game it started stops
     * with its session.
     */
    fun stop(status: SessionStatus, now: Long): Stop {
        if (status.snapshot?.activeJob == Job.OFFICIAL_LAUNCHER) return Stop.OFFICIAL_LAUNCHER
        return when (AppUpdate.waitFor(status, copying = false, now)) {
            AppUpdate.Wait.GAME, AppUpdate.Wait.STARTING -> Stop.GAME
            AppUpdate.Wait.OFFICIAL_LAUNCHER -> Stop.OFFICIAL_LAUNCHER
            AppUpdate.Wait.WORK -> Stop.JOB
            AppUpdate.Wait.PREPARING, null -> Stop.NOTHING
        }
    }

    /**
     * Whether Close's [stop] has finished in [status]: the official PD2 launcher's job, the game
     * or the job no longer runs. An engine that is not connected runs nothing more to wait for.
     */
    fun stopped(stop: Stop, status: SessionStatus, now: Long): Boolean {
        val snapshot = status.snapshot
        if (status.engine != SessionStatus.Engine.CONNECTED || snapshot == null) return true
        return when (stop) {
            Stop.OFFICIAL_LAUNCHER -> snapshot.activeJob != Job.OFFICIAL_LAUNCHER
            Stop.GAME -> snapshot.game != Game.RUNNING && !status.launcherGame
            Stop.JOB -> AppUpdate.waitFor(status, copying = false, now) != AppUpdate.Wait.WORK
            Stop.NOTHING -> true
        }
    }

    /**
     * Whether the app's update is at work that must go on in the background: its download or the
     * download's checks. A check is short, and Android's install window is the player's.
     */
    fun appUpdating(state: AppUpdate.State): Boolean =
        state is AppUpdate.State.Downloading || state is AppUpdate.State.Verifying

    /**
     * Whether the session must go on in the background at [now], so it is a foreground service
     * with its notification: something runs in the engine's part ([stop]), the game environment
     * runs for an install ([installEnvironment]: also between Install Diablo II and Install Project
     * Diablo 2, which follows without input), the app unpacks its game support files ("Updating
     * game files…", after an app update), a folder is [copying], or an app update is at work
     * ([appUpdating]). The engine's start and its first check are short and do not count: the
     * notification would show at each start of the app.
     */
    fun keeps(status: SessionStatus, installEnvironment: Boolean, copying: Boolean, appUpdating: Boolean, now: Long): Boolean =
        stop(status, now) != Stop.NOTHING || (installEnvironment && status.gameEnvironment) ||
            status.userland is SessionStatus.Userland.Unpacking || copying || appUpdating
}
