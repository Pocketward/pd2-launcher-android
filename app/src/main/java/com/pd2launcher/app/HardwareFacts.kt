package com.pd2launcher.app

import java.io.File
import java.util.Locale
import java.util.TimeZone

/**
 * Facts about the device for `device.txt` (Share logs, Save logs) that a report about speed or a
 * crash needs: the CPU's cores, their highest frequencies and features, the memory, and the time
 * zone, so a log's local times can be read as UTC. Each reads its file and gives the words; a file
 * that cannot be read gives "unknown".
 */
object HardwareFacts {
    /**
     * The CPU: the core count (from [cores], the kernel's list of the cores there,
     * `/sys/devices/system/cpu/present`, else of those there can be, `…/possible`; so a parked
     * core counts too), the cores by their highest frequency ([maxFrequencies]: CPU number to kHz,
     * slowest first; a core whose frequency cannot be read, as a parked one's, as unknown), the
     * features that `/proc/cpuinfo`'s [cpuinfo] lists on every core, and whether those have the
     * LSE atomics ([hasAtomics]).
     */
    fun cpu(cpuinfo: String?, maxFrequencies: Map<Int, Long>, cores: String? = null): String {
        val count = cores?.let(::coreCount)
            ?: maxFrequencies.size.takeIf { it > 0 }
            ?: cpuinfo?.lineSequence()?.count { it.substringBefore(':').trim() == "processor" }?.takeIf { it > 0 }
        val unknown = if (maxFrequencies.isNotEmpty() && count != null) count - maxFrequencies.size else 0
        val clusters = maxFrequencies.values.groupingBy { it }.eachCount().toSortedMap()
            .map { (khz, many) -> "$many × ${String.format(Locale.ROOT, "%.2f", khz / 1e6)} GHz" } +
            (if (unknown > 0) listOf("$unknown × frequency unknown") else emptyList())
        val features = cpuinfo?.let(::commonFeatures)
        val atomics = when (cpuinfo?.let(::hasAtomics)) {
            true -> "yes"
            false -> "no"
            null -> "unknown"
        }
        return buildString {
            append(count?.let { "$it cores" } ?: "cores unknown")
            if (clusters.isNotEmpty()) append(" (").append(clusters.joinToString(" + ")).append(')')
            append("; features ").append(features?.takeIf { it.isNotEmpty() }?.joinToString(" ") ?: "unknown")
            append("; LSE atomics ").append(atomics)
        }
    }

    /**
     * Whether every core's `Features` line in [cpuinfo] (the text of `/proc/cpuinfo`) lists the
     * ARMv8.1 LSE atomics (`atomics`); null when no `Features` line names any feature.
     */
    fun hasAtomics(cpuinfo: String): Boolean? {
        val features = cpuinfo.lineSequence()
            .filter { it.substringBefore(':').trim() == "Features" }
            .map { line -> line.substringAfter(':', "").split(Regex("\\s+")).filter { it.isNotBlank() } }
            .filter { it.isNotEmpty() }
            .toList()
        return if (features.isEmpty()) null else features.all { "atomics" in it }
    }

    /** The kernel's list of the cores there ([present]), else of those there can be ([possible]): the first that is a CPU list. */
    fun coreList(present: String?, possible: String?): String? = listOf(present, possible).firstOrNull { it != null && coreCount(it) != null }

    /** The number of CPUs in a kernel CPU list such as `0-7` or `0-3,6`; null when it is not one. */
    fun coreCount(list: String): Int? {
        val parts = list.trim().split(',').filter { it.isNotEmpty() }
        if (parts.isEmpty()) return null
        return parts.sumOf { part ->
            val bounds = part.split('-').map { it.trim().toIntOrNull() ?: return null }
            when (bounds.size) {
                1 -> 1
                2 -> (bounds[1] - bounds[0] + 1).takeIf { it > 0 } ?: return null
                else -> return null
            }
        }
    }

    /** The features that every `Features` line of [cpuinfo] lists, in the first line's order. */
    fun commonFeatures(cpuinfo: String): List<String> {
        val lines = cpuinfo.lineSequence()
            .filter { it.substringBefore(':').trim() == "Features" }
            .map { line -> line.substringAfter(':', "").split(Regex("\\s+")).filter { it.isNotBlank() } }
            .filter { it.isNotEmpty() }
            .toList()
        val first = lines.firstOrNull() ?: return emptyList()
        return first.filter { feature -> lines.all { feature in it } }
    }

    /** This device's [cpu]. */
    fun cpuOfDevice(): String {
        val cpuinfo = runCatching { File("/proc/cpuinfo").readText() }.getOrNull()
        val frequencies = File("/sys/devices/system/cpu").listFiles().orEmpty()
            .mapNotNull { Regex("cpu([0-9]+)").matchEntire(it.name)?.groupValues?.get(1)?.toInt() }
            .mapNotNull { cpu ->
                runCatching { File("/sys/devices/system/cpu/cpu$cpu/cpufreq/cpuinfo_max_freq").readText().trim().toLong() }
                    .getOrNull()?.let { cpu to it }
            }
            .toMap()
        val read = { name: String -> runCatching { File("/sys/devices/system/cpu/$name").readText() }.getOrNull() }
        return cpu(cpuinfo, frequencies, coreList(read("present"), read("possible")))
    }

    /** MemTotal and SwapTotal of `/proc/meminfo`'s [meminfo], in MB; "unknown" without MemTotal. */
    fun memory(meminfo: String?): String {
        val values = meminfo?.lineSequence().orEmpty()
            .mapNotNull { line -> Regex("""^(\w+):\s+(\d+) kB$""").matchEntire(line.trim())?.destructured }
            .associate { (name, kb) -> name to kb.toLongOrNull() }
        val total = values["MemTotal"] ?: return "unknown"
        return "${total / 1024} MB" + (values["SwapTotal"]?.let { ", swap ${it / 1024} MB" } ?: "")
    }

    fun memoryOfDevice(): String = memory(runCatching { File("/proc/meminfo").readText() }.getOrNull())

    /**
     * The time [zone]'s offset from UTC [atMillis]: "UTC+09:00". Only the offset, which the log
     * times need; the zone's name tells more about where the player lives.
     */
    fun timeZone(zone: TimeZone, atMillis: Long): String {
        val minutes = zone.getOffset(atMillis) / 60_000
        val sign = if (minutes < 0) "-" else "+"
        val abs = kotlin.math.abs(minutes)
        return "UTC$sign${String.format(Locale.ROOT, "%02d:%02d", abs / 60, abs % 60)}"
    }

    /**
     * The screen for `device.txt`: the whole [panel] (width, height in pixels, the bounds of
     * Android's maximum window metrics, turned as the screen is now), the app's [window] inside it
     * (without the bars of the system, so it can be shorter: 1920 x 1025 in a 1920 x 1080 panel),
     * the density, the rate now ([hertzNow], null when unknown) and the full rate that the app
     * measured ([fullHertz], 0 or less when not measured).
     */
    fun screen(panel: Pair<Int, Int>, window: Pair<Int, Int>, densityDpi: Int, hertzNow: Float?, fullHertz: Float): String {
        val now = hertzNow?.let { String.format(Locale.ROOT, "%.0f Hz now", it) } ?: "rate unknown"
        val full = fullHertz.takeIf { it > 0f }?.let { String.format(Locale.ROOT, ", full rate %.0f Hz", it) } ?: ""
        return "panel ${panel.first} x ${panel.second}, window ${window.first} x ${window.second}, $densityDpi dpi, $now$full"
    }
}
