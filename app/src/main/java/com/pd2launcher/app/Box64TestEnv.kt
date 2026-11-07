package com.pd2launcher.app

import java.io.File
import java.io.FileOutputStream
import java.nio.file.Files
import java.nio.file.StandardCopyOption

/**
 * Settings → Testing (test builds only, [AppUpdater.TEST_BUILD]): the player's values for some of
 * Box64's dynamic recompiler settings ([KNOBS]), so that a player can compare them on the device.
 * They are stored as one string ([AppSettings.box64TestEnv], [stored]) and reach Box64 as values of
 * HKCU\Environment in the prefix's `user.reg` ([edit]), which the app writes before each Play and
 * each official PD2 launcher while no Wine runs ([applyTo]). Wine sets the values of that key over
 * the environment of every program it starts with no Windows parent, so they apply to everything the
 * app runs in the prefix: the game, the official launcher, the installers. Box64 then names them in
 * its log ("Variables overridden"). A program that the official launcher starts gets Box64's
 * defaults for them again (the engine's BOX64_DYNAREC_PROGRAMS names only the launcher's programs).
 *
 * Only the values of [KNOBS] are ever written or removed; every other line of the file stays as it is,
 * byte for byte. A build that is not a test build never writes ([applyTo] with `testBuild` false).
 */
object Box64TestEnv {
    /** The prefix of every variable here. */
    const val PREFIX = "BOX64_DYNAREC_"

    /** The line that each Play logs, and device.txt has, before the values ([line]). */
    const val LINE = "box64 test env"

    /**
     * One setting: its [name] without [PREFIX], Box64's own [default] in WowBox64, and the [values]
     * the page offers (those that Box64's settings table accepts for it; see [KNOBS]).
     */
    data class Knob(val name: String, val default: Int, val values: List<Int>) {
        /** The variable's full name. */
        val variable: String get() = PREFIX + name
    }

    /**
     * The settings, in the page's order, with the values and defaults of the Box64 that the app
     * builds (its settings table, `src/include/env.h`, and its usage notes, `docs/USAGE.md`).
     * FORWARD takes only these five values (others fall back to 128). CALLRET's 2 does not work in
     * WowBox64 and its 3 is for LoongArch only, so the page offers 0 and 1.
     */
    val KNOBS: List<Knob> = listOf(
        Knob("SAFEFLAGS", 1, listOf(0, 1, 2)),
        Knob("FASTNAN", 1, listOf(0, 1)),
        Knob("FASTROUND", 1, listOf(0, 1, 2)),
        Knob("X87DOUBLE", 0, listOf(0, 1, 2)),
        Knob("BIGBLOCK", 2, listOf(0, 1, 2, 3)),
        Knob("STRONGMEM", 0, listOf(0, 1, 2, 3, 4)),
        Knob("FORWARD", 128, listOf(0, 128, 256, 512, 1024)),
        Knob("CALLRET", 0, listOf(0, 1)),
        Knob("WAIT", 1, listOf(0, 1)),
        Knob("WEAKBARRIER", 1, listOf(0, 1, 2)),
        Knob("NATIVEFLAGS", 1, listOf(0, 1)),
    )

    private val byName = KNOBS.associateBy { it.name }

    /**
     * The stored string ([stored]: `NAME=value` with `,` between them) as the player's choices, in
     * [KNOBS]' order: a name that is not a knob, or a value the knob does not offer, is left out.
     */
    fun parse(stored: String?): Map<String, Int> {
        if (stored.isNullOrEmpty()) return emptyMap()
        val picked = HashMap<String, Int>()
        for (part in stored.split(',')) {
            val equals = part.indexOf('=')
            if (equals <= 0) continue
            val knob = byName[part.substring(0, equals).trim()] ?: continue
            val value = part.substring(equals + 1).trim().toIntOrNull() ?: continue
            if (value in knob.values) picked[knob.name] = value
        }
        return ordered(picked)
    }

    /** [choices] as the stored string; null when none is set. */
    fun stored(choices: Map<String, Int>): String? =
        ordered(choices).entries.joinToString(",") { (name, value) -> "$name=$value" }.ifEmpty { null }

    /** [choices] with [name] set to [value]; null: back to Box64's default (not set). A value the knob does not offer changes nothing. */
    fun with(choices: Map<String, Int>, name: String, value: Int?): Map<String, Int> {
        val knob = byName[name] ?: return ordered(choices)
        val next = HashMap(choices)
        when {
            value == null -> next.remove(name)
            value in knob.values -> next[name] = value
        }
        return ordered(next)
    }

    /** The choices that apply: the stored ones in a test build ([testBuild]), none in any other build. */
    fun active(stored: String?, testBuild: Boolean): Map<String, Int> = if (testBuild) parse(stored) else emptyMap()

    /** "STRONGMEM=1 WAIT=0", or "none". */
    fun summary(choices: Map<String, Int>): String =
        ordered(choices).entries.joinToString(" ") { (name, value) -> "$name=$value" }.ifEmpty { "none" }

    /** The service log's and device.txt's line: "box64 test env: STRONGMEM=1 WAIT=0", or "box64 test env: none". */
    fun line(choices: Map<String, Int>): String = "$LINE: ${summary(choices)}"

    private fun ordered(choices: Map<String, Int>): Map<String, Int> {
        val out = LinkedHashMap<String, Int>()
        for (knob in KNOBS) choices[knob.name]?.let { out[knob.name] = it }
        return out
    }

    /** Whether [line] is the key line of HKCU\Environment: `[Environment]`, with or without its time, in any case. */
    private fun environmentKey(line: String): Boolean {
        if (!line.startsWith("[")) return false
        val end = line.indexOf(']')
        if (end < 0) return false
        val rest = line.substring(end + 1)
        return line.substring(1, end).equals("Environment", ignoreCase = true) && (rest.isEmpty() || rest.startsWith(" "))
    }

    /** The variables of [KNOBS], without case. */
    private val ours: Map<String, Knob> = KNOBS.associateBy { it.variable.lowercase() }

    private fun valueLine(knob: Knob, value: Int) = "\"${knob.variable}\"=\"$value\""

    /**
     * The text of a `user.reg` ([text]) with [choices] as the values of HKCU\Environment: a value of
     * ours that is there takes the new value in its place, one that [choices] does not set goes, and
     * the others are added at the end of the key. Every other line stays as it is, so the same
     * [choices] give the same text again. With no Environment key, one is added at the end of the
     * file when [choices] sets a value ([time]: its key time, in seconds).
     */
    fun edit(text: String, choices: Map<String, Int>, time: Long = System.currentTimeMillis() / 1000): String {
        val wanted = ordered(choices)
        val lines = text.split('\n')
        val out = ArrayList<String>(lines.size + wanted.size)
        val written = HashSet<String>()
        var inside = false
        var found = false
        // The line before goes on to this one (it ends with '\'): this line is part of that value.
        var continued = false
        // While a value of ours is removed, its following lines go with it.
        var dropping = false
        fun addMissing() {
            // Before the empty lines that close the key.
            var at = out.size
            while (at > 0 && out[at - 1].isEmpty()) at--
            val missing = wanted.filterKeys { it !in written }.map { (name, value) -> valueLine(byName.getValue(name), value) }
            out.addAll(at, missing)
            written += wanted.keys
        }
        for (line in lines) {
            val starts = !continued
            continued = line.endsWith("\\")
            if (starts && line.startsWith("[")) {
                if (inside) addMissing()
                inside = environmentKey(line)
                found = found || inside
                dropping = false
                out += line
                continue
            }
            if (inside && starts) {
                dropping = false
                val knob = WineRegistry.valueName(line)?.let(ours::get)
                if (knob != null) {
                    val value = wanted[knob.name]
                    if (value != null && knob.name !in written) {
                        out += valueLine(knob, value)
                        written += knob.name
                    }
                    // Removed, or a second line of the same name: its following lines go too.
                    dropping = true
                    continue
                }
            } else if (inside && dropping) {
                continue
            }
            out += line
        }
        if (inside) addMissing()
        if (!found && wanted.isNotEmpty()) {
            // Wine starts each key after an empty line, and the file ends with the key's last value line.
            var at = out.size
            while (at > 0 && out[at - 1].isEmpty()) at--
            val key = listOf("", "[Environment] $time") + wanted.map { (name, value) -> valueLine(byName.getValue(name), value) }
            out.subList(at, out.size).clear()
            out.addAll(key)
            out += ""
        }
        return out.joinToString("\n")
    }

    /** The values of [KNOBS] in HKCU\Environment of [text] (a `user.reg`), by knob name, as the file writes them. */
    fun read(text: String): Map<String, String> {
        val found = HashMap<String, String>()
        var inside = false
        var continued = false
        for (line in text.split('\n')) {
            val starts = !continued
            continued = line.endsWith("\\")
            if (!starts) continue
            if (line.startsWith("[")) {
                inside = environmentKey(line)
                continue
            }
            if (!inside) continue
            val knob = WineRegistry.valueName(line)?.let(ours::get) ?: continue
            found.putIfAbsent(knob.name, line.substringAfter("\"=").removeSurrounding("\""))
        }
        return KNOBS.mapNotNull { knob -> found[knob.name]?.let { knob.name to it } }.toMap()
    }

    /** What [applyTo] did. */
    enum class Outcome {
        /** Not a test build: nothing read or written. */
        NOT_A_TEST_BUILD,

        /** The prefix has no `user.reg` (none made yet). */
        NO_PREFIX,

        /** The file had these values already: not written. */
        UNCHANGED,

        /** The file was written. */
        WRITTEN,
    }

    /**
     * [choices] into `user.reg` of [prefix] ([edit]) in a test build ([testBuild]); in any other
     * build nothing is read or written. The caller holds the launches' lock and the prefix lock, and
     * no Wine runs (Wine keeps the hive in memory and writes it back at its end). The file is read
     * and written as bytes (Latin-1 keeps each byte), through a temporary file that is synced and
     * renamed over it.
     */
    fun applyTo(prefix: File, choices: Map<String, Int>, testBuild: Boolean): Outcome {
        if (!testBuild) return Outcome.NOT_A_TEST_BUILD
        val file = File(prefix, "user.reg")
        if (!file.isFile) return Outcome.NO_PREFIX
        val before = String(file.readBytes(), Charsets.ISO_8859_1)
        val after = edit(before, choices)
        if (after == before) return Outcome.UNCHANGED
        val temporary = File(prefix, "user.reg.box64-test")
        FileOutputStream(temporary).use { stream ->
            stream.write(after.toByteArray(Charsets.ISO_8859_1))
            stream.fd.sync()
        }
        Files.move(temporary.toPath(), file.toPath(), StandardCopyOption.REPLACE_EXISTING, StandardCopyOption.ATOMIC_MOVE)
        return Outcome.WRITTEN
    }

    /** Whether `user.reg` of [prefix] holds other values of [KNOBS] than [choices] (false without the file). */
    fun differs(prefix: File, choices: Map<String, Int>): Boolean {
        val file = File(prefix, "user.reg")
        if (!file.isFile) return false
        return read(String(file.readBytes(), Charsets.ISO_8859_1)) != ordered(choices).mapValues { it.value.toString() }
    }
}
