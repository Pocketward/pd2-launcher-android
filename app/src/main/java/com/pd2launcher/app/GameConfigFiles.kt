package com.pd2launcher.app

import java.io.File
import java.io.IOException
import java.security.MessageDigest
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive

/**
 * Project Diablo 2's own settings files in the game folder's ProjectD2 ([LauncherFiles.FOLDER]):
 * D2GL's, the resolution mod's, PD2's game options, BH's item display (its JSON and its older cfg)
 * and DirectDraw's. The player changes them in the game (Options, D2GL's Ctrl+O), and they change
 * what the GPU and the CPU do, so a report needs them to run the player's game again: Save logs
 * packs each as `game-config/<name>` ([shareTexts]), and each Play logs a short fingerprint of each
 * ([fingerprints]), so the log tells which games ran with the same files. The app never writes them.
 * PD2 keeps the account name, the last game's name and its password under `bnet`: every text value
 * there is hidden ([HIDDEN]); a JSON file that cannot be read as JSON is left out, and the other
 * files are [LauncherFiles.redact]ed line by line.
 */
object GameConfigFiles {
    val FILES = listOf("d2gl.json", "SGD2FreeResolution.json", "ProjectDiablo.json", "BH.json", "ProjectDiablo.cfg", "ddraw.ini")
    const val HIDDEN = "<hidden>"
    private const val MAX_BYTES = 256 * 1024L
    private const val PRIVATE_SECTION = "bnet"
    private val json = Json { isLenient = true; prettyPrint = true }

    /** Each of [FILES] in [project] that is there, as `game-config/<name>` with its private values hidden. */
    fun shareTexts(project: File?): List<Pair<String, String>> {
        val folder = project?.takeIf { it.isDirectory } ?: return emptyList()
        return FILES.mapNotNull { name -> read(File(folder, name))?.let { "game-config/$name" to shareable(name, it) } }
    }

    /** The text of [name] that Save logs packs: [text] with its private values hidden. */
    fun shareable(name: String, text: String): String {
        if (!name.endsWith(".json")) return text.lines().joinToString("\n") { LauncherFiles.redact(it) }
        val root = try {
            json.parseToJsonElement(text.removePrefix("﻿"))
        } catch (_: Exception) {
            return "[PD2 Launcher: not readable as JSON (${text.length} characters), left out]\n"
        }
        return json.encodeToString(JsonElement.serializer(), hidePrivate(root, inPrivate = false)) + "\n"
    }

    /**
     * One line for the service log at Play: each of [FILES] in [project] with its size and the first
     * 8 hex digits of its SHA-256, or "none"; "no game folder" without [project].
     */
    fun fingerprints(project: File?): String {
        val folder = project?.takeIf { it.isDirectory } ?: return "game config files: no game folder"
        return "game config files: " + FILES.joinToString(", ") { name ->
            val bytes = readBytes(File(folder, name))
            if (bytes == null) "$name none" else "$name ${bytes.size} B ${sha256(bytes).take(8)}"
        }
    }

    private fun hidePrivate(element: JsonElement, inPrivate: Boolean): JsonElement = when (element) {
        is JsonObject -> JsonObject(element.mapValues { (key, value) -> hidePrivate(value, inPrivate || key.equals(PRIVATE_SECTION, ignoreCase = true)) })
        is JsonArray -> JsonArray(element.map { hidePrivate(it, inPrivate) })
        is JsonPrimitive -> if (inPrivate && element.isString && element.content.isNotEmpty()) JsonPrimitive(HIDDEN) else element
    }

    private fun read(file: File): String? = readBytes(file)?.decodeToString()

    private fun readBytes(file: File): ByteArray? = try {
        if (file.isFile && file.length() <= MAX_BYTES) file.readBytes() else null
    } catch (_: IOException) {
        null
    }

    private fun sha256(bytes: ByteArray): String =
        MessageDigest.getInstance("SHA-256").digest(bytes).joinToString("") { "%02x".format(it) }
}
