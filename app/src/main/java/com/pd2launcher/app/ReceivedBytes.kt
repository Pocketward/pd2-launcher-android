package com.pd2launcher.app

import android.net.TrafficStats
import android.os.Process
import java.io.File
import java.io.IOException

/**
 * The bytes this app has received ([TrafficStats.getUidRxBytes] for its own user, which Blizzard's
 * downloader under Wine shares), written to [SessionPaths.receivedBytes] while a job downloads or
 * installs, for the engine's stall rule: a download that receives data counts as working even
 * while it writes nothing (a slow connection filling a large piece, say). Written whole, through a
 * temporary file and a rename, so the engine never reads half a number.
 */
object ReceivedBytes {
    /** How often it is written while a job runs; the engine looks each second. */
    const val INTERVAL_MS = 2_000L

    /** The app's received bytes, or null when Android does not count them. */
    fun current(): Long? = TrafficStats.getUidRxBytes(Process.myUid()).takeIf { it >= 0 }

    /**
     * Writes [bytes] to [file] whole; a write that fails leaves the last one. Its folder is the
     * session's own ([SessionPaths.createFolders] makes it owner-only), never made here.
     */
    fun write(file: File, bytes: Long) {
        val temporary = File(file.parentFile, "${file.name}.tmp")
        try {
            temporary.writeText("$bytes\n")
            if (!temporary.renameTo(file)) temporary.delete()
        } catch (_: IOException) {
            temporary.delete()
        }
    }
}
