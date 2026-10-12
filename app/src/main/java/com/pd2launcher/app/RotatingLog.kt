package com.pd2launcher.app

import java.io.File
import java.io.FileOutputStream
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale

/**
 * One owned process's bounded log in `files/logs`:
 * `<name>.log` is written until it reaches [limit] bytes, then renamed to `<name>.log.1`
 * (older ones shift up) and a new one starts; [keep] files are kept in all, so at most
 * [keep] * [limit] bytes plus one write.
 */
class RotatingLog(
    private val directory: File,
    val name: String,
    private val limit: Long = LIMIT,
    private val keep: Int = KEEP,
) {
    private val lock = Any()
    private var out: FileOutputStream? = null
    private var size = 0L

    init {
        require(keep >= 1) { "keep at least the current file" }
    }

    val file: File get() = File(directory, "$name.log")

    /** Appends raw output of the process. */
    fun append(bytes: ByteArray, length: Int = bytes.size) {
        if (length <= 0) return
        synchronized(lock) {
            open().write(bytes, 0, length)
            size += length
            if (size >= limit) rotate()
        }
    }

    /** Appends one line, prefixed with the wall-clock time. */
    fun line(text: String) {
        val stamp = SimpleDateFormat("yyyy-MM-dd HH:mm:ss.SSS", Locale.ROOT).format(Date())
        append("$stamp $text\n".toByteArray())
    }

    fun close() = synchronized(lock) {
        out?.close()
        out = null
    }

    private fun open(): FileOutputStream {
        out?.let { return it }
        directory.mkdirs()
        val current = file
        size = if (current.isFile) current.length() else 0L
        if (size >= limit) {
            rotate()
            return open()
        }
        return FileOutputStream(current, true).also { out = it }
    }

    private fun rotate() {
        out?.close()
        out = null
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
    }
}
