package com.pd2launcher.app

import com.pd2launcher.app.control.OperationStatus
import com.pd2launcher.app.control.Snapshot

/** Protocol 5 has no operation ID. Observe a cleared old result before sending a new operation. */
internal class OperationStart(
    private val connection: Any,
    private val afterSnapshot: Long,
    val clearCommand: Long,
    val startedAt: Long,
    private val dispatch: () -> Unit,
    private val cancelled: () -> Unit,
) {
    private var pending = true

    fun observe(source: Any, sequence: Long, snapshot: Snapshot): Boolean {
        if (!pending || source !== connection || sequence <= afterSnapshot ||
            snapshot.activeJob != null || snapshot.operation != null) return false
        pending = false
        dispatch()
        return true
    }

    fun cancel() {
        if (!pending) return
        pending = false
        cancelled()
    }

    companion object {
        fun terminal(snapshot: Snapshot?): Boolean = snapshot?.operation?.status in setOf(
            OperationStatus.SUCCEEDED, OperationStatus.FAILED, OperationStatus.CANCELLED, OperationStatus.INDETERMINATE,
        )
    }
}

/** Called only by the session worker. Cleanup finishes before observers can admit another launch. */
internal class SessionSnapshots {
    private var connection: Any? = null
    private var sequence = -1L

    fun apply(source: Any, current: Any?, nextSequence: Long, cleanup: () -> Unit, publish: () -> Unit): Boolean {
        if (source !== current) return false
        if (source === connection && nextSequence <= sequence) return false
        connection = source
        sequence = nextSequence
        cleanup()
        publish()
        return true
    }
}
