package com.pd2launcher.app

import com.pd2launcher.app.control.Settings
import kotlinx.serialization.json.Json

/**
 * Every setting the app keeps, for Save logs' `settings.txt`: each of the app's preference files
 * (all of them, so a new setting is in the logs with no change here) with every key and value, then
 * the engine's settings as its last snapshot sent them. [SettingsLog] gives the short line at each
 * Play; this is the whole of it, so a report can run the player's game again. A value that names a
 * folder or file the player picked (a document URI, or any value of a key with "folder" in its name)
 * is hidden ([HIDDEN]).
 */
object SettingsSnapshot {
    const val FILE = "settings.txt"
    const val HIDDEN = "<a picked folder>"
    private val URI = Regex("""(?i)\b(?:content|file)://""")
    private val json = Json { prettyPrint = true }

    /** The text for [preferences] (file name to its keys and values) and the engine's [engine] settings (null: none yet). */
    fun text(preferences: Map<String, Map<String, *>>, engine: Settings?): String = buildString {
        for ((file, values) in preferences.toSortedMap()) {
            append('[').append(file).append("]\n")
            if (values.isEmpty()) append("(empty)\n")
            for ((key, value) in values.toSortedMap()) {
                append(key).append(" = ").append(if (key.contains("folder", ignoreCase = true) && value != null) HIDDEN else shown(value)).append('\n')
            }
            append('\n')
        }
        append("[engine]\n")
        append(engine?.let { json.encodeToString(Settings.serializer(), it) } ?: "none yet (the engine has sent no settings)")
        append('\n')
    }

    private fun shown(value: Any?): String = when (value) {
        is String -> if (URI.containsMatchIn(value)) HIDDEN else value
        is Set<*> -> value.map(::shown).sorted().toString()
        else -> value.toString()
    }
}
