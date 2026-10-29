package com.pd2launcher.app

import com.pd2launcher.app.control.Job
import com.pd2launcher.app.control.Snapshot
import com.pd2launcher.app.ui.InstallProgress

/**
 * The install job that the session service sent ([start]) and owns until the engine shows its
 * end ([shouldContinueToPd2]). The request is kept from the send, before a snapshot of the job can
 * arrive: a job whose first snapshot already shows its end (no snapshot of it running) still stops
 * the game environment that was started for it. Worker thread only.
 */
internal class InstallWork {
    private data class Request(val job: Job, val imported: Boolean, val afterSnapshot: Long, val sequence: Long)
    private var request: Request? = null
    /** The `seq` of the install command that is owned now; null when none is. */
    val sequence: Long? get() = request?.sequence

    /**
     * [prepare] starts what the job needs (false: nothing is sent), then [send] sends the command;
     * a command that is not sent [stop]s what [prepare] started. [afterSnapshot]: the newest
     * snapshot before the send, which cannot show this job.
     */
    fun start(
        job: Job, imported: Boolean, afterSnapshot: Long,
        prepare: () -> Boolean, send: () -> Long?, stop: () -> Unit,
    ): Boolean {
        if (!prepare()) return false
        val sequence = send()
        if (sequence == null) {
            stop()
            return false
        }
        request = Request(job, imported, afterSnapshot, sequence)
        return true
    }

    /**
     * Snapshot [next] (number [sequence]) shows the end of the owned job: the request goes, and
     * [stop] stops the game environment, unless Install Project Diablo 2 follows (true; never
     * after an import).
     */
    fun shouldContinueToPd2(sequence: Long, next: Snapshot, stop: () -> Unit): Boolean {
        val owned = request ?: return false
        if (sequence <= owned.afterSnapshot ||
            (next.activeJob != owned.job && next.operation?.job != owned.job)) return false
        val continues = InstallProgress.continuesToPd2(owned.job, next, owned.imported)
        val ended = InstallProgress.installEnded(owned.job, next, owned.imported)
        if (!continues && !ended) return false
        request = null
        if (ended) stop()
        return continues
    }

    /**
     * The engine refused command [sequence]: the owned request goes when it is that command and
     * [snapshot] shows no job of its type at work. Protocol 5 can give the refusal of an earlier
     * command the newest seq, so a refusal with the install's seq while its job runs keeps the
     * request: its end still stops the game environment or continues to Install Project Diablo 2.
     * Whether it went.
     */
    fun refused(sequence: Long, snapshot: Snapshot?): Boolean {
        val owned = request ?: return false
        if (owned.sequence != sequence) return false
        val working = snapshot?.activeJob == owned.job ||
            (snapshot?.operation?.job == owned.job && !OperationStart.terminal(snapshot))
        if (working) return false
        request = null
        return true
    }

    fun clear() { request = null }
}
