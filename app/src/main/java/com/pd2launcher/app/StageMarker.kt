package com.pd2launcher.app

import java.io.File
import java.io.RandomAccessFile
import java.nio.file.Files
import java.nio.file.LinkOption
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.intOrNull

/**
 * The engine's interrupted-stage marker in the managed root (`.pd2launcher-stage.json`), read as
 * the engine reads it (`src/stage_marker.rs`): at most 1024 bytes, a JSON object with exactly the
 * fields `schema` (the number 1), `stage` (`prefix`, `base-game` or `expansion`) and, once the
 * stage's step returned, `finished` (`true`). Anything else that is there, a link too, is no
 * marker the engine wrote, and fails closed, as there. The app does not write it.
 */
object StageMarker {
    /** The marker's name in the managed root (`stage_marker::STAGE_MARKER`). */
    const val NAME = ".pd2launcher-stage.json"
    private const val LIMIT = 1024
    private val STAGES = setOf("prefix", "base-game", "expansion")
    private val FIELDS = setOf("schema", "stage", "finished")

    /** What the managed root says (`stage_marker::MarkerState`). */
    enum class State {
        ABSENT,

        /** A stage started and its step has not returned: an install step, or a Start over on the Android host. */
        STARTED,

        /** The stage's step returned; only the engine's proof and clear are left. */
        FINISHED,

        /** There, but not a marker the engine wrote. */
        INVALID,
    }

    fun read(root: File): State {
        val file = File(root, NAME).toPath()
        if (!Files.exists(file, LinkOption.NOFOLLOW_LINKS)) return State.ABSENT
        if (!Files.isRegularFile(file, LinkOption.NOFOLLOW_LINKS)) return State.INVALID
        val text = runCatching {
            RandomAccessFile(file.toFile(), "r").use {
                if (it.length() > LIMIT) return State.INVALID
                ByteArray(it.length().toInt()).also { bytes -> it.readFully(bytes) }.decodeToString()
            }
        }.getOrNull() ?: return State.INVALID
        val record = runCatching { Json.parseToJsonElement(text) }.getOrNull() as? JsonObject ?: return State.INVALID
        if (!FIELDS.containsAll(record.keys)) return State.INVALID
        val schema = record["schema"] as? JsonPrimitive
        val stage = record["stage"] as? JsonPrimitive
        val finished = record["finished"]?.let { it as? JsonPrimitive ?: return State.INVALID }
        if (schema == null || schema.isString || schema.intOrNull != 1) return State.INVALID
        if (stage == null || !stage.isString || stage.content !in STAGES) return State.INVALID
        return when {
            finished == null -> State.STARTED
            finished.isString -> State.INVALID
            finished.booleanOrNull == true -> State.FINISHED
            finished.booleanOrNull == false -> State.STARTED
            else -> State.INVALID
        }
    }

    /**
     * Whether a stage of the engine is under way in [root]: a marker there that is not a finished
     * one (an invalid one counts, as the engine fails closed on it). While one is, the app makes no
     * prefix again at a start ([LegacyPrefix.stale], [LegacyPrefix.broken]): the engine's audit
     * shows the step as interrupted, and only Start over resolves it.
     */
    fun underWay(root: File): Boolean = read(root).let { it == State.STARTED || it == State.INVALID }
}
