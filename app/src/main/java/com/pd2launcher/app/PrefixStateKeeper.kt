package com.pd2launcher.app

import com.pd2launcher.app.control.Command
import com.pd2launcher.app.control.Game
import java.io.File
import java.nio.file.Files
import java.nio.file.Path
import java.util.concurrent.atomic.AtomicBoolean

/**
 * What a new prefix needs back ([PrefixState]) in the managed root [root], kept by the session
 * service ([keep]) as the installation stands Ready with nothing running (after each game session
 * and each official-launcher session, and after an install): the player's registry values, and,
 * after an install, update or repair job ([PrefixState.captureIsDue]) in a prefix of the PLAIN
 * template (the source Blizzard's installers), what the installers wrote outside the game folder.
 * Not while Wine runs ([wineRuns]): the hives are whole only once the game environment has
 * stopped. The Ready snapshot can come before the game environment of an install has stopped:
 * such a capture is put off, and as the game environment has stopped the service takes it ([takePutOff]) and runs [keep] again, once.
 * Only while the installation is Ready ([ready]): a Start over that a stop left part way is never
 * Ready (the engine's stage marker), so no capture writes `player.reg` again then. The caller
 * holds the launches' lock and the prefix lock ([SaveSyncFolder.locked], then [PrefixState.locked]).
 */
class PrefixStateKeeper(
    private val root: File,
    private val log: (String) -> Unit,
    /** Whether the installation stands Ready (the engine's last snapshot). */
    private val ready: () -> Boolean,
    /** Whether Wine runs: the game environment, a job, the game, the official launcher's game, a launch. */
    private val wineRuns: () -> Boolean,
    /** Makes a folder's entries durable (fsync on the folder); the JVM tests pass their own. */
    private val syncDirectory: (Path) -> Unit = ::fsyncDirectory,
) {
    private val waiting = AtomicBoolean(false)

    /**
     * Whether a capture that Wine's run put off waits for the game environment's stop; the mark
     * goes with the answer, so the service runs it once ([keep] puts it off again should Wine
     * run again).
     */
    fun takePutOff(): Boolean = waiting.getAndSet(false)

    /** The capture, now (see the class). */
    fun keep() {
        if (!ready()) {
            waiting.set(false)
            return
        }
        // The mark before the check: a game environment that stops between the two still finds it
        // (at worst the capture then runs twice, which changes nothing).
        waiting.set(true)
        if (wineRuns()) {
            log("prefix state not kept now: Wine runs; it is kept once the game environment has stopped")
            return
        }
        waiting.set(false)
        val prefix = File(root, "prefix")
        if (!File(prefix, "user.reg").isFile) return
        val state = PrefixState(root, log, syncDirectory)
        try {
            if (state.captureIsDue()) {
                if (PrefixState.markedKind(prefix) == PrefixState.Companion.Kind.PLAIN) {
                    state.captureInstallers(prefix, File(root, AppRuntime.PREFIX_TEMPLATE))
                } else {
                    Files.deleteIfExists(state.captureDue.toPath())
                }
            }
            state.capturePlayer(prefix)
        } catch (error: Exception) {
            log("prefix state not kept: $error")
        }
    }

    companion object {
        /** How long a job command sent counts as one under way with no answer ([wineRuns]). */
        const val JOB_REQUEST_WAIT_MS = 30_000L

        /**
         * Whether [command] asks the engine for a job that changes the installation: Start over,
         * an install, an import, a download, Install Project Diablo 2, an update. From its send
         * until a snapshot shows a job (or the engine refuses it, or the engine ends) it counts as
         * under way ([SessionStatus.jobRequestedAt]): the snapshot before it still says Ready with
         * nothing running. (Repair starts only from RepairRequired, and Install Diablo II, whose
         * keys go by `ControlChannel.sendAuthorize`, only from NeedsBaseGame: neither is Ready.)
         */
        fun requestsJob(command: Command): Boolean = command is Command.StartOver || command is Command.ContinueInstallation ||
            command is Command.DownloadAll || command is Command.ImportExistingInstall || command is Command.InstallProjectDiablo ||
            command is Command.ApplyUpdate || command is Command.DiscardDownloads

        /**
         * Whether Wine runs, or a program or a job that changes the prefix may start at once, as
         * [status] stands at [now] (elapsed real time): the game environment, a job, the game, the
         * official launcher's game, a launch requested, or a job command sent in the last
         * [JOB_REQUEST_WAIT_MS] that no snapshot shows yet. The session service's [wineRuns].
         */
        fun wineRuns(status: SessionStatus, now: Long): Boolean =
            status.gameEnvironment || status.snapshot?.activeJob != null || status.snapshot?.game == Game.RUNNING ||
                status.launcherGame || status.launchRequestedAt != null ||
                status.jobRequestedAt?.let { now - it < JOB_REQUEST_WAIT_MS } == true
    }
}
