package com.pd2launcher.app

import java.io.File
import java.io.IOException

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

/**
 * A helper's start on the little cores through `taskset` (Xvfb, [GameEnvironment.start]), and
 * once more without it when that start ends before the helper is ready: some firmware refuses
 * taskset's own `sched_setaffinity` (issue #13, a Xiaomi POCO on HyperOS 3: "taskset: failed to
 * set pid ...'s affinity: Operation not permitted", status 1), and the helper never runs. After
 * that, every start goes without taskset for as long as this object lives (the app's process).
 */
class LittleCoresStart {
    /** Whether a start through taskset ended before its helper was ready. */
    @Volatile var refused = false
        private set

    /**
     * [start] with [pinned] (taskset's program and argv; null: none) while taskset was not
     * [refused], else with [direct]; [ready] waits until the process is ready and throws
     * [EndedEarly] when it ended first. Gives the process that is ready.
     */
    fun <P> start(
        pinned: Pair<String, List<String>>?,
        direct: Pair<String, List<String>>,
        log: (String) -> Unit,
        start: (String, List<String>) -> P,
        ready: (P) -> Unit,
    ): P {
        if (pinned != null && !refused) {
            val process = start(pinned.first, pinned.second)
            try {
                ready(process)
                return process
            } catch (error: EndedEarly) {
                refused = true
                log("taskset could not keep ${direct.second.first()} on the little cores (${error.message}): started without taskset, also for the rest of the session")
            }
        } else if (pinned != null) {
            log("${direct.second.first()} starts without taskset: taskset failed earlier in this session")
        }
        return start(direct.first, direct.second).also(ready)
    }
}

/** A process ended before it was ready ([LittleCoresStart]). */
class EndedEarly(message: String) : IOException(message)
