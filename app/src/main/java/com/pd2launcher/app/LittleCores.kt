package com.pd2launcher.app

import java.io.File

/**
 * The CPUs of the slowest cluster: those whose highest frequency is the lowest of the device's
 * (`cpufreq/cpuinfo_max_freq`). Helper programs that stream frames through memory (Xvfb) run
 * there, off the big cores whose shared cache the game's main thread under FEX depends on.
 * None on a device with one cluster or unreadable frequencies.
 */
object LittleCores {
    /** [maxFrequencies]: CPU number to its highest frequency. */
    fun of(maxFrequencies: Map<Int, Long>): List<Int> {
        val lowest = maxFrequencies.values.minOrNull() ?: return emptyList()
        if (maxFrequencies.values.all { it == lowest }) return emptyList()
        return maxFrequencies.filterValues { it == lowest }.keys.sorted()
    }

    /** `taskset`'s hexadecimal mask for [cpus], or null for none. */
    fun mask(cpus: List<Int>): String? =
        if (cpus.isEmpty() || cpus.any { it !in 0..62 }) null
        else cpus.fold(0L) { mask, cpu -> mask or (1L shl cpu) }.toString(16)

    /** This device's little cores. */
    fun ofDevice(): List<Int> {
        val cpus = File("/sys/devices/system/cpu").listFiles().orEmpty()
            .mapNotNull { Regex("cpu([0-9]+)").matchEntire(it.name)?.groupValues?.get(1)?.toInt() }
        val frequencies = cpus.associateWith { cpu ->
            try {
                File("/sys/devices/system/cpu/cpu$cpu/cpufreq/cpuinfo_max_freq").readText().trim().toLong()
            } catch (_: Exception) {
                return emptyList()
            }
        }
        return of(frequencies)
    }
}
