package com.pd2launcher.app

import java.nio.file.Files
import java.nio.file.Path
import java.security.MessageDigest

/**
 * The App userland's placeholder data folder ([UserlandManifest.placeholder], the one every program
 * is built for) and the real data folder [dataDir] that replaces it as the tree is unpacked: in a
 * binary file in place, [dataDir] padded with '/' to the placeholder's length, so no offset or
 * length in the file changes (repeated slashes name the same path); in a text file
 * ([ManifestEntry.text]) [dataDir] exactly, so the file shrinks.
 */
internal class Relocation(placeholder: String, val dataDir: String) {
    val placeholder: ByteArray = placeholder.toByteArray()
    private val exact = dataDir.toByteArray()
    private val padded: ByteArray

    init {
        problem(dataDir, this.placeholder.size)?.let { throw IllegalArgumentException(it) }
        padded = exact + ByteArray(this.placeholder.size - exact.size) { '/'.code.toByte() }
    }

    /** What replaces each placeholder of [entry]. */
    fun replacement(entry: ManifestEntry): ByteArray = if (entry.text) exact else padded

    /** [entry]'s size once rewritten. */
    fun rewrittenSize(entry: ManifestEntry): Long = (entry.size ?: 0L) - entry.placeholders.size * shrink(entry)

    /** Where [entry]'s placeholders are once rewritten, in order. */
    fun rewrittenAt(entry: ManifestEntry): List<Long> = entry.placeholders.mapIndexed { i, at -> at - i * shrink(entry) }

    private fun shrink(entry: ManifestEntry): Long = if (entry.text) (placeholder.size - exact.size).toLong() else 0L

    /**
     * Null when [entry]'s placeholder offsets fit its size, in order and apart; else why not. The
     * unpack and the critical-file check take none that do not.
     */
    fun offsetProblem(entry: ManifestEntry): String? {
        var end = 0L
        for (at in entry.placeholders) {
            if (at < end) return "${entry.path}: placeholder offsets out of order or overlapping"
            end = at + placeholder.size
        }
        return if (end > (entry.size ?: 0L)) "${entry.path}: placeholder past the end" else null
    }

    companion object {
        /** A canonical absolute path of the characters Android's data folders use. */
        private val SAFE = Regex("^(/[A-Za-z0-9._-]+)+$")

        /**
         * Why [dataDir] cannot replace a placeholder of [placeholderBytes] bytes, or null when it
         * can: it must be an absolute canonical path no longer than the placeholder, of letters,
         * digits, '.', '_', '-' and '/' only (the userland's files join paths with ':' and spaces).
         */
        fun problem(dataDir: String, placeholderBytes: Int): String? = when {
            !SAFE.matches(dataDir) || dataDir.split('/').any { it == "." || it == ".." } ->
                "data folder $dataDir: not a canonical path of letters, digits, '.', '_', '-' and '/'"
            dataDir.length > placeholderBytes ->
                "data folder $dataDir: ${dataDir.length} bytes, longer than the App userland's $placeholderBytes"
            else -> null
        }
    }
}

/**
 * Passes a file's bytes on as they arrive, with the placeholder at each of [at] (offsets in the
 * original, in order) replaced by [replacement]. The bytes at each offset must be [placeholder]:
 * nothing else is changed. [finish] checks that every offset was reached.
 */
internal class PlaceholderWriter(
    private val at: List<Long>,
    private val placeholder: ByteArray,
    private val replacement: ByteArray,
    private val out: (ByteArray, Int, Int) -> Unit,
) {
    private var position = 0L
    private var next = 0

    fun write(buffer: ByteArray, length: Int) {
        var i = 0
        while (i < length) {
            val start = if (next < at.size) at[next] else Long.MAX_VALUE
            if (position < start) {
                val n = minOf((length - i).toLong(), start - position).toInt()
                out(buffer, i, n)
                i += n
                position += n
            } else {
                val k = (position - start).toInt()
                if (buffer[i] != placeholder[k]) throw CorruptUserland("no placeholder at offset $start")
                i++
                position++
                if (k + 1 == placeholder.size) {
                    out(replacement, 0, replacement.size)
                    next++
                }
            }
        }
    }

    fun finish() {
        if (next < at.size) throw CorruptUserland("placeholder at offset ${at[next]} past the end")
    }
}

/**
 * SHA-256 of [file] with the [found] bytes at each of [at] (in order, apart) put back to
 * [placeholder]: the hash the file had as packed, if those bytes are what the unpack wrote there.
 * Null when they are not, or the file is shorter.
 */
internal fun restoredSha256(file: Path, at: List<Long>, found: ByteArray, placeholder: ByteArray): String? {
    val digest = MessageDigest.getInstance("SHA-256")
    val buffer = ByteArray(1 shl 16)
    val region = ByteArray(found.size)
    Files.newInputStream(file).use { input ->
        fun hash(count: Long): Boolean {
            var left = count
            while (left > 0) {
                val n = input.read(buffer, 0, minOf(left, buffer.size.toLong()).toInt())
                if (n < 0) return false
                digest.update(buffer, 0, n)
                left -= n
            }
            return true
        }
        fun readRegion(): Boolean {
            var done = 0
            while (done < region.size) {
                val n = input.read(region, done, region.size - done)
                if (n < 0) return false
                done += n
            }
            return true
        }
        var position = 0L
        for (start in at) {
            if (!hash(start - position)) return null
            if (!readRegion() || !region.contentEquals(found)) return null
            digest.update(placeholder)
            position = start + found.size
        }
        while (true) {
            val n = input.read(buffer)
            if (n < 0) break
            digest.update(buffer, 0, n)
        }
    }
    return digest.digest().joinToString("") { "%02x".format(it) }
}
