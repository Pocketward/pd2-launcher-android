package com.pd2launcher.app

import android.util.Log
import java.io.File
import java.io.FileOutputStream
import java.io.IOException
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale

/**
 * One owned process's bounded log in `files/logs`:
 * `<name>.log` is written until it reaches [limit] bytes, then renamed to `<name>.log.1`
 * (older ones shift up) and a new one starts; [keep] files are kept in all, so at most
 * [keep] * [limit] bytes plus one write.
 *
 * Writing never throws: a write that fails (a full disk, say) is dropped and the next one opens
 * the file again, since the log's callers are the reaper, the log drains and the snapshot
 * collection, which an exception would end or take the app down with. [warn] hears of the first
 * dropped write and, once writing works again, how many were dropped.
 */
class RotatingLog(
    private val directory: File,
    val name: String,
    private val limit: Long = LIMIT,
    private val keep: Int = KEEP,
    private val warn: (String) -> Unit = { Log.w(TAG, it) },
) {
    private val lock = Any()
    private var out: FileOutputStream? = null
    private var size = 0L
    private var dropped = 0L

    init {
        require(keep >= 1) { "keep at least the current file" }
    }

    val file: File get() = File(directory, "$name.log")

    /** Appends raw output of the process. */
    fun append(bytes: ByteArray, length: Int = bytes.size) {
        if (length <= 0) return
        synchronized(lock) {
            try {
                open().write(bytes, 0, length)
                size += length
                if (dropped > 0) {
                    warn("$name.log: writing works again; $dropped writes were dropped")
                    dropped = 0
                }
                if (size >= limit) rotate()
            } catch (error: IOException) {
                closeQuietly()
                if (dropped++ == 0L) warn("$name.log: a write failed ($error); writes are dropped until one works")
            }
        }
    }

    /**
     * Appends one line, prefixed with the wall-clock time and its offset from UTC (`+09:00`, `Z`):
     * the game's, Battle.net's and the official launcher's own logs are in UTC.
     */
    fun line(text: String) {
        val stamp = SimpleDateFormat("yyyy-MM-dd HH:mm:ss.SSSXXX", Locale.ROOT).format(Date())
        append("$stamp $text\n".toByteArray())
    }

    fun close() = synchronized(lock) { closeQuietly() }

    private fun closeQuietly() {
        val current = out ?: return
        out = null
        try {
            current.close()
        } catch (_: IOException) {
        }
    }

    private fun open(): FileOutputStream {
        out?.let { return it }
        directory.mkdirs()
        val current = file
        // Rotated once at most: a rename that fails (a read-only folder) must not recurse.
        if (current.isFile && current.length() >= limit) rotate()
        size = if (current.isFile) current.length() else 0L
        return FileOutputStream(current, true).also { out = it }
    }

    private fun rotate() {
        closeQuietly()
        File(directory, "$name.log.${keep - 1}").delete()
        for (index in keep - 2 downTo 1) {
            val older = File(directory, "$name.log.$index")
            if (older.exists()) older.renameTo(File(directory, "$name.log.${index + 1}"))
        }
        if (keep > 1) file.renameTo(File(directory, "$name.log.1")) else file.delete()
        size = 0
    }

    companion object {
        const val LIMIT = 1L shl 20
        const val KEEP = 3
        private const val TAG = "pd2-log"
    }
}
