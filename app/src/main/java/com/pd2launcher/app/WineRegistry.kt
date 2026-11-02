package com.pd2launcher.app

/**
 * Wine's registry hive files (`system.reg`, `user.reg`) as text, which the app reads and writes while
 * no Wine runs (the prefix is disposable: [PrefixState] keeps the player's values and, for the source
 * Blizzard's installers, what those installers wrote, and writes them into a new prefix's hives).
 *
 * A hive is a head (the `WINE REGISTRY Version 2` line, comments, `#arch=`) and keys: a key line
 * `[<name>] <time>`, its `#` lines (`#time=`, `#class=`, `#link`) and its values, one per line, where a
 * value goes on to the next line while its line ends with `\`. Key names are compared without case, as
 * Windows does; they stay in the escaped form of the file (`Software\\Wine`).
 */
object WineRegistry {
    private val KEY_LINE = Regex("""^\[(.*)\](?: \d+)?$""")

    /** One key: its name as the file writes it and its lines (the key line first, no empty line). */
    data class Key(val name: String, val lines: List<String>) {
        /** The name without case. */
        val id: String get() = name.lowercase()
    }

    /** A hive's text as its head and its keys. */
    data class Hive(val head: List<String>, val keys: List<Key>) {
        /** The hive's text: the head, an empty line, then each key with an empty line after it. */
        fun text(): String {
            val head = head.joinToString("\n").trimEnd('\n')
            if (keys.isEmpty()) return "$head\n"
            return head + "\n\n" + keys.joinToString("") { it.lines.joinToString("\n") + "\n\n" }.trimEnd('\n') + "\n"
        }
    }

    /** [text] as a [Hive]. Empty lines go: a value has none, they only stand between the keys. */
    fun parse(text: String): Hive {
        val head = ArrayList<String>()
        val keys = ArrayList<Pair<String, MutableList<String>>>()
        for (line in text.removeSuffix("\n").split('\n')) {
            val match = KEY_LINE.matchEntire(line)
            when {
                match != null -> keys += match.groupValues[1] to mutableListOf(line)
                keys.isNotEmpty() -> if (line.isNotEmpty()) keys.last().second += line
                else -> head += line
            }
        }
        return Hive(head, keys.map { (name, lines) -> Key(name, lines) })
    }

    /** The name of the value that [line] starts, without case: `@` for the default value; null for any other line. */
    fun valueName(line: String): String? {
        if (line.startsWith("@=")) return "@"
        if (!line.startsWith("\"")) return null
        var i = 1
        while (i < line.length) {
            when (line[i]) {
                '\\' -> i += 2
                '"' -> return if (line.getOrNull(i + 1) == '=') line.substring(1, i).lowercase() else null
                else -> i++
            }
        }
        return null
    }

    /** The values of [key], by their names without case, each with its lines; `#` lines are not values. */
    fun values(key: Key): Map<String, List<String>> {
        val out = LinkedHashMap<String, MutableList<String>>()
        var current: MutableList<String>? = null
        for (line in key.lines.drop(1)) {
            if (current != null && current.last().endsWith("\\")) {
                current += line
                continue
            }
            current = valueName(line)?.let { name -> mutableListOf(line).also { out[name] = it } }
        }
        return out
    }

    /**
     * The keys of [hive] at [name] and below it (its subkeys), [name] in the file's escaped form
     * (`Software\\Blizzard Entertainment\\Diablo II`), compared without case.
     */
    fun subtree(hive: Hive, name: String): List<Key> {
        val id = name.lowercase()
        return hive.keys.filter { it.id == id || it.id.startsWith("$id\\\\") }
    }

    /**
     * [hive] with [keys] written in: each replaces the key of its name (as a whole: the key is then
     * the one of [keys]), and a key that is not there is added at the end. Wine reads its keys in any
     * order and writes them sorted at its next save.
     */
    fun merge(hive: Hive, keys: List<Key>): Hive {
        val byId = keys.associateBy { it.id }
        val kept = hive.keys.map { byId[it.id] ?: it }
        val there = hive.keys.mapTo(HashSet()) { it.id }
        return Hive(hive.head, kept + keys.filter { it.id !in there })
    }

    /**
     * The keys of [prefix] that are not in [template] as they are: a key that the template does not
     * have, or one with a value that is new or other (its time lines do not count), as a whole, as
     * the prefix has it. Keys for which [leftOut] is true are not taken. A value or a key that the
     * prefix lost is not a difference: an installer seldom removes one, and the hive text has no way
     * to say it.
     */
    fun difference(prefix: Hive, template: Hive, leftOut: (String) -> Boolean): List<Key> {
        val templateKeys = template.keys.associateBy { it.id }
        return prefix.keys.filter { key ->
            if (leftOut(key.id)) return@filter false
            val other = templateKeys[key.id] ?: return@filter true
            val theirs = values(other)
            values(key).any { (name, lines) -> theirs[name] != lines }
        }
    }

    /**
     * [key] with each string value that starts with [from] (without case) starting with [to]
     * instead. [from] and [to] are in the file's escaped form (`G:\\Diablo II`).
     */
    fun replacePathPrefix(key: Key, from: String, to: String): Key {
        val lines = key.lines.mapIndexed { index, line ->
            if (index == 0) return@mapIndexed line
            val name = valueName(line) ?: return@mapIndexed line
            val start = if (name == "@") 2 else line.indexOf("\"=") + 2
            val value = line.substring(start)
            if (value.startsWith("\"") && value.regionMatches(1, from, 0, from.length, ignoreCase = true)) {
                line.substring(0, start) + "\"" + to + value.substring(1 + from.length)
            } else {
                line
            }
        }
        return Key(key.name, lines)
    }

    /**
     * The hive parts of a file in the golden layer's form (`runtime/golden.reg`): each line
     * `;; @hive <file>` starts the keys of that hive file.
     */
    fun hiveParts(text: String): Map<String, List<Key>> {
        val parts = LinkedHashMap<String, MutableList<String>>()
        var current: MutableList<String>? = null
        for (line in text.split('\n')) {
            if (line.startsWith(";; @hive ")) {
                current = ArrayList<String>().also { parts[line.removePrefix(";; @hive ").trim().substringBefore(' ')] = it }
            } else {
                current?.add(line)
            }
        }
        return parts.mapValues { (_, lines) -> parse(lines.joinToString("\n")).keys }
    }

    /** [parts] in the form [hiveParts] reads, after the comment lines [comments]. */
    fun hivePartsText(comments: List<String>, parts: Map<String, List<Key>>): String {
        val out = StringBuilder("WINE REGISTRY Version 2\n")
        for (comment in comments) out.append(";; ").append(comment).append('\n')
        for ((hive, keys) in parts) {
            out.append('\n').append(";; @hive ").append(hive).append('\n')
            for (key in keys) out.append('\n').append(key.lines.joinToString("\n")).append('\n')
        }
        return out.toString()
    }

    /** A key name of the file's escaped form from its parts (`keyName("Software", "Wine")` is `Software\\Wine`). */
    fun keyName(vararg parts: String): String = parts.joinToString("\\\\")
}
