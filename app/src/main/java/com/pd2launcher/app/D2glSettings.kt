package com.pd2launcher.app

import java.io.File
import java.io.IOException
import java.util.Locale
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.intOrNull
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive

/**
 * D2GL's settings that the pointer's delay depends on, for Detailed logging's service log beside
 * the pointer split ([PointerSplit]), read only from the game folder's `ProjectD2/d2gl.json`: its
 * V-Sync (`screen.window_vsync`), its frame rate limit in the foreground and in the background
 * (`screen.foreground_fps_active` and `_value`, `screen.background_fps_active` and `_value`), and
 * its frame latency (`other.frame_latency`: how many frames the game's thread can be ahead of D2GL's
 * render thread). The app never writes the file; the engine's first-launch values are in
 * `src/process/d2gl.rs`.
 */
object D2glSettings {
    const val FILE = "d2gl.json"
    private const val MAX_BYTES = 64 * 1024L
    private val json = Json { isLenient = true }

    /** The line for the file's [text] (null: there is no file). */
    fun line(text: String?): String {
        val head = "D2GL settings ($FILE, read only):"
        if (text == null) return "$head none (the game has not written it yet, or it uses another renderer)"
        val root = try {
            json.parseToJsonElement(text.removePrefix("﻿")).jsonObject
        } catch (_: Exception) {
            return "$head not readable as JSON"
        }
        val screen = root.section("screen")
        val vsync = screen.flag("window_vsync")?.let { if (it) "on" else "off" } ?: "not set"
        return "$head V-Sync $vsync, frame rate limit ${limit(screen, "foreground")} in the foreground and " +
            "${limit(screen, "background")} in the background, frame latency " +
            (root.section("other").number("frame_latency")?.toString() ?: "not set (D2GL's own: 1)")
    }

    /** The text of [file], or null when it is not there, is larger than 64 KiB or cannot be read. */
    fun read(file: File): String? = try {
        if (file.isFile && file.length() <= MAX_BYTES) file.readText() else null
    } catch (_: IOException) {
        null
    }

    private fun limit(screen: JsonObject?, which: String): String {
        val active = screen.flag("${which}_fps_active")
        val value = screen.number("${which}_fps_value")
        return when (active) {
            true -> value?.let { String.format(Locale.ROOT, "on at %d", it) } ?: "on (no value)"
            false -> "off"
            null -> "not set"
        }
    }

    private fun JsonObject.section(name: String): JsonObject? = (this[name] as? JsonObject)

    private fun JsonObject?.flag(name: String): Boolean? = try {
        this?.get(name)?.jsonPrimitive?.booleanOrNull
    } catch (_: IllegalArgumentException) {
        null
    }

    private fun JsonObject?.number(name: String): Int? = try {
        this?.get(name)?.jsonPrimitive?.intOrNull
    } catch (_: IllegalArgumentException) {
        null
    }
}

/**
 * D2GL's settings in the service log while the game runs with Detailed logging ([D2glSettings]): a
 * line at the first look of a run and one each time the file changes (its time or size: the player
 * changed D2GL's options in the game, Ctrl+O). One stat of the file a look. Not thread-safe: the
 * session service's frame look uses it alone.
 */
class D2glWatch {
    private var file: File? = null
    private var located = false
    private var seen: Pair<Long, Long>? = null
    private var given = false

    /** The line for a look, if any; [locate] finds the file once a run (null: no game folder). */
    fun next(locate: () -> File?): String? {
        if (!located) {
            file = locate()
            located = true
        }
        val f = file
        val stamp = if (f != null && f.isFile) f.lastModified() to f.length() else null
        if (given && stamp == seen) return null
        given = true
        seen = stamp
        return D2glSettings.line(f?.let(D2glSettings::read))
    }

    /** The run ended (or Detailed logging went off): the next run looks for the file again. */
    fun reset() {
        file = null
        located = false
        seen = null
        given = false
    }
}
