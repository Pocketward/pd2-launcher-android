package com.pd2launcher.app

/**
 * When the engine starts again after it ended unexpectedly (or never answered): each failure in a
 * row waits twice as long as the one before, from [FIRST_DELAY_MS] up to [MAX_DELAY_MS], and the
 * one after [maxFailures] in a row is not restarted (FAILED) until the player tries again
 * ([reset]) or something else starts it ([started]). An engine that stayed connected for
 * [healthyMs] has recovered: its end is the first failure again. Times are elapsed real time in
 * milliseconds. Not thread-safe: the session worker's.
 */
class RestartPolicy(
    private val maxFailures: Int = MAX_FAILURES,
    private val healthyMs: Long = HEALTHY_MS,
) {
    private var failures = 0
    private var connectedAt: Long? = null

    /** The engine answered at [now]. */
    fun connected(now: Long) {
        connectedAt = now
    }

    /** The engine ended at [now]: how long to wait before starting it again, or null to give up. */
    fun failed(now: Long): Long? {
        val since = connectedAt
        connectedAt = null
        if (since != null && now - since >= healthyMs) failures = 0
        failures++
        if (failures > maxFailures) return null
        return (FIRST_DELAY_MS shl (failures - 1).coerceAtMost(MAX_SHIFT)).coerceAtMost(MAX_DELAY_MS)
    }

    /** The player asked for another start: the failures so far no longer count. */
    fun reset() {
        failures = 0
        connectedAt = null
    }

    /**
     * The engine starts other than by this policy's restart: after it gave up, the failures so
     * far no longer count (as [reset]); before, they still do.
     */
    fun started() {
        if (failures > maxFailures) reset()
    }

    companion object {
        const val FIRST_DELAY_MS = 2_000L
        const val MAX_DELAY_MS = 60_000L
        /** 2, 4, 8, 16 and 32 s: given up on about a minute after the first failure. */
        const val MAX_FAILURES = 5
        /** Connected this long, an engine has recovered. */
        const val HEALTHY_MS = 5 * 60_000L
        private const val MAX_SHIFT = 20
    }
}
