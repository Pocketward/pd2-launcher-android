package com.pd2launcher.app

import com.pd2launcher.app.control.Host
import com.pd2launcher.app.control.Installation
import com.pd2launcher.app.control.Job
import com.pd2launcher.app.control.OperationStatus
import com.pd2launcher.app.control.Snapshot
import com.pd2launcher.app.control.Update
import java.io.File
import java.io.IOException

/**
 * Play looks for a new Project Diablo 2 version before the game starts. While the Launcher's
 * update checks are on ([checks], the default), every press of Play asks the engine to check again
 * (its Check again: the audit, then the update check, about 1.5 s on a fast device) and the game
 * starts when the check says that the game is up to date. A new version opens the question "New
 * version found": Update runs the engine's update, and the game starts when it succeeds; Cancel
 * closes the question, and Play offers Update and "Play without updating" as before. A network
 * failure never keeps the game from starting: with no network, a failed check, or no answer in
 * [WAIT_MS], the game starts and Play says why there was no check. "Play without updating" and a
 * Play with the checks off start the game at once.
 */
object PlayCheck {
    /** How long a press of Play waits for the engine's check before the game starts without it. */
    const val WAIT_MS = 10_000L

    /** Why the game starts without the check's answer. */
    enum class Note {
        /** Android has no network now: no check was sent. */
        NO_NETWORK,
        /** The engine's check failed (PD2's update server could not be reached, say). */
        FAILED,
        /** The engine did not answer in [WAIT_MS], or the check could not be sent. */
        NO_ANSWER,
    }

    /** Where a press of Play stands, from the press until the game starts or Play shows again. */
    sealed interface State {
        /**
         * Check again ([seq], null until it is sent) was sent at [sentAt], when the engine named
         * the update [before]. [audit]: its audit showed since; [checking]: its update check
         * showed since, so the update the snapshot names now is the check's. Both follow every
         * snapshot the app takes ([observe]).
         */
        data class Checking(
            val seq: Long?,
            val sentAt: Long,
            val before: Update? = null,
            val audit: Boolean = false,
            val checking: Boolean = false,
        ) : State

        /** The check found a new version: the question shows. */
        data object Found : State

        /**
         * The player chose Update ([seq], sent at [sentAt]); [seen]: its job showed. The game starts
         * when it succeeds.
         */
        data class Updating(val seq: Long, val sentAt: Long, val seen: Boolean = false, val afterSnapshot: Long = -1) : State

        /** The game starts now; [note] says why without the check's answer. */
        data class Starting(val note: Note?) : State
    }

    /** What follows from a [State] and the session now. */
    sealed interface Outcome {
        /** Nothing yet: [state], with what showed since. */
        data class Wait(val state: State) : Outcome

        /** Start the game; [note] says why without the check's answer. */
        data class Play(val note: Note?) : Outcome

        /** A new version: the question. */
        data object Ask : Outcome

        /**
         * Nothing more to do: the engine went away, refused the command, the installation changed
         * or the update did not succeed. Play shows what stands.
         */
        data object Drop : Outcome
    }

    /**
     * A press of Play checks first: the engine's update checks are on, or it has no such setting
     * (an engine before protocol 5 always checks).
     */
    fun checks(snapshot: Snapshot?): Boolean = snapshot?.settings?.updateChecks != false

    /**
     * [state] after the engine's snapshot [next], for each snapshot the app takes, as it takes it:
     * a check's audit and its update check can show and end between two looks at the session.
     */
    fun observe(state: State?, next: Snapshot): State? =
        if (state is State.Checking) seen(state, next) else state

    private fun seen(state: State.Checking, snapshot: Snapshot) = state.copy(
        audit = state.audit || !Onboarding.audited(snapshot),
        checking = state.checking || snapshot.update == Update.CHECKING,
    )

    /**
     * Where the check ([state]) stands with the session now. The engine's Check again shows its
     * audit (the host audit runs, [Onboarding.audited] is false), then the update check
     * (Checking), then its answer. An answer counts once the check showed, as the snapshot before
     * it still names the update from before the press; after the audit, also an answer other than
     * that one ([State.Checking.before]), as the engine sends only its newest snapshot and a check
     * that fails at once can end before its Checking is sent. Available counts at once, as Play
     * offered none at the press.
     */
    fun next(state: State.Checking, status: SessionStatus, now: Long): Outcome {
        val snapshot = status.snapshot
        if (status.engine != SessionStatus.Engine.CONNECTED || snapshot == null) return Outcome.Drop
        if (state.seq != null && status.refusal?.seq == state.seq) return Outcome.Drop
        val audited = Onboarding.audited(snapshot)
        val seen = seen(state, snapshot)
        if (audited && (seen.audit || seen.checking) &&
            (snapshot.host != Host.SUPPORTED || snapshot.installation != Installation.READY)
        ) {
            return Outcome.Drop
        }
        if (audited) {
            val answered = seen.checking || (seen.audit && snapshot.update != state.before)
            when (snapshot.update) {
                Update.AVAILABLE -> return Outcome.Ask
                Update.UP_TO_DATE -> if (answered) return Outcome.Play(null)
                Update.CHECK_FAILED -> if (answered) return Outcome.Play(Note.FAILED)
                // The audit ended and the engine's checks went off meanwhile: no check follows.
                Update.UNKNOWN -> if (seen.audit && !checks(snapshot)) return Outcome.Play(null)
                Update.CHECKING -> Unit
            }
        }
        if (now - state.sentAt >= WAIT_MS) return Outcome.Play(Note.NO_ANSWER)
        return Outcome.Wait(seen)
    }

    /**
     * The question stays while the new version is still offered (or checked again: a check that
     * was asked for while one ran follows it) and nothing else runs; otherwise it closes (an
     * engine restart, the game started by another way, a job).
     */
    fun stillFound(status: SessionStatus): Boolean {
        val snapshot = status.snapshot ?: return false
        return status.engine == SessionStatus.Engine.CONNECTED &&
            (snapshot.update == Update.AVAILABLE || snapshot.update == Update.CHECKING) &&
            snapshot.installation == Installation.READY && snapshot.activeJob == null &&
            snapshot.game != com.pd2launcher.app.control.Game.RUNNING
    }

    private val UPDATE_RUNNING = setOf(
        OperationStatus.QUEUED, OperationStatus.PLANNING, OperationStatus.RUNNING,
        OperationStatus.VERIFYING, OperationStatus.COMMITTING, OperationStatus.SAFE_STOPPING,
    )

    /**
     * Where the update the question started ([state]) stands: the game starts once it succeeded
     * and the audit after it says Ready; a failure, a stop, a refusal or the engine's end drops the
     * start, and Play's own update view says what happened (Try again, Resume).
     */
    fun afterUpdate(state: State.Updating, status: SessionStatus, now: Long): Outcome {
        val snapshot = status.snapshot
        if (status.engine != SessionStatus.Engine.CONNECTED || snapshot == null) return Outcome.Drop
        if (status.refusal?.seq == state.seq) return Outcome.Drop
        // A job that never showed (nor was refused) holds Play no longer than the check's wait: a
        // refusal can name another command's seq.
        val unseenTooLong = !state.seen && now - state.sentAt >= WAIT_MS
        if (state.afterSnapshot >= 0 && (status.snapshotSeq ?: -1) <= state.afterSnapshot) {
            return if (unseenTooLong) Outcome.Drop else Outcome.Wait(state)
        }
        val operation = snapshot.operation?.takeIf { it.job == Job.UPDATE }
        if (snapshot.activeJob == Job.UPDATE || operation?.status in UPDATE_RUNNING) return Outcome.Wait(state.copy(seen = true))
        // No running snapshot yet: an update can end before one shows, and its end alone counts (below).
        if (operation == null) return if (state.seen || unseenTooLong) Outcome.Drop else Outcome.Wait(state)
        val ready = snapshot.host == Host.SUPPORTED && snapshot.installation == Installation.READY
        return if (operation.status == OperationStatus.SUCCEEDED && ready) Outcome.Play(null) else Outcome.Drop
    }

    /** The game's start after the check ends with the engine: its note goes. */
    fun startingEnds(status: SessionStatus): Boolean = status.engine != SessionStatus.Engine.CONNECTED

    /**
     * Debug builds only: the marker [DEBUG_MARKER] in the app's files folder makes the next check
     * find a new version, for device tests without a real one. It changes the Launcher's own record
     * of the accepted version ([record], `ProjectD2/.pd2launcher/accepted-manifest.json`): one line
     * end more, so the bytes no longer are the server's while every file's digest stays. PD2's
     * files and its server stay as they are, and the update that follows writes the record again.
     * The marker goes once used. Returns what it did, for the log; null when there is no marker.
     */
    fun debugNewVersion(marker: File, record: File?): String? {
        if (!marker.exists()) return null
        marker.delete()
        if (record == null || !record.isFile) return "debug: no record of the accepted version to change"
        return try {
            record.appendText("\n")
            "debug: the Launcher's record of the accepted version changed, so the check finds a new version"
        } catch (error: IOException) {
            "debug: the record of the accepted version could not change: $error"
        }
    }

    /** The debug marker ([debugNewVersion]) in the app's files folder. */
    const val DEBUG_MARKER = "debug-new-game-version"

    /** The Launcher's record of the accepted version in [gameFolder]'s ProjectD2. */
    fun record(gameFolder: File): File = File(gameFolder, "ProjectD2/.pd2launcher/accepted-manifest.json")
}
