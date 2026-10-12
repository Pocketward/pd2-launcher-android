package com.pd2launcher.app

import java.io.File
import java.io.IOException
import java.nio.ByteBuffer

/**
 * The CD keys a debug build pre-fills the key screen with: the file [FILE] in the development
 * build's own files folder, which `debug-keys.sh` writes there over adb from the developer's
 * `.env.local` (docs/BUILDING.md). The keys are in no APK; the file is read as characters that
 * never become a String, and every buffer is cleared.
 */
object DebugKeys {
    const val FILE = "debug-keys.properties"

    /** [D2_KEY, D2_LOD_KEY] from [filesDir]'s file; null when it or either key is missing. The caller clears them. */
    fun read(filesDir: File): Array<CharArray>? {
        val bytes = try {
            File(filesDir, FILE).takeIf { it.isFile }?.readBytes()
        } catch (_: IOException) {
            null
        } ?: return null
        val chars = Charsets.UTF_8.decode(ByteBuffer.wrap(bytes))
        bytes.fill(0)
        val text = CharArray(chars.remaining()).also { chars.get(it) }
        if (chars.hasArray()) chars.array().fill('\u0000')
        return try {
            parse(text)
        } finally {
            text.fill('\u0000')
        }
    }

    /**
     * [D2_KEY, D2_LOD_KEY] from the `NAME=value` lines of [text] (the first of each name; quotes,
     * spaces and a carriage return dropped); null unless both are there and each is only letters,
     * digits and dashes.
     */
    fun parse(text: CharArray): Array<CharArray>? {
        val keys = listOf(value(text, "D2_KEY"), value(text, "D2_LOD_KEY"))
        if (keys.any { key -> key == null || key.isEmpty() || !key.all { it.isLetterOrDigit() || it == '-' } }) {
            keys.forEach { it?.fill('\u0000') }
            return null
        }
        return arrayOf(keys[0]!!, keys[1]!!)
    }

    private fun value(text: CharArray, name: String): CharArray? {
        val prefix = "$name="
        var start = 0
        while (start < text.size) {
            var end = start
            while (end < text.size && text[end] != '\n') end++
            if (end - start >= prefix.length && prefix.indices.all { text[start + it] == prefix[it] }) {
                val kept = CharArray(end - start - prefix.length)
                var n = 0
                for (i in start + prefix.length until end) {
                    val c = text[i]
                    if (c != '"' && c != '\'' && c != ' ' && c != '\r') kept[n++] = c
                }
                return kept.copyOf(n).also { kept.fill('\u0000') }
            }
            start = end + 1
        }
        return null
    }
}
