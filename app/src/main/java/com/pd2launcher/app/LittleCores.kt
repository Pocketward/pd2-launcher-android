package com.pd2launcher.app

import java.io.File
import java.io.IOException

/**
 * The CPUs of the slowest cluster: those whose highest frequency is the lowest of the device's
 * (`cpufreq/cpuinfo_max_freq`). Helper programs that stream frames through memory (Xvfb) run
 * there, off the big cores whose shared cache the game's main thread depends on.
 * None on a device with one cluster or unreadable frequencies.
 */
object LittleCores {
    /** [maxFrequencies]: CPU number to its highest frequency. */
    fun of(maxFrequencies: Map<Int, Long>): List<Int> {
        val lowest = maxFrequencies.values.minOrNull() ?: return emptyList()
        if (maxFrequencies.values.all { it == lowest }) return emptyList()
        return maxFrequencies.filterValues { it == lowest }.keys.sorted()
    }

    /**
     * The CPUs outside the slowest cluster ([of]): on a phone with a big cluster (and a prime
     * core), those. Empty where [of] is (one cluster, or unreadable frequencies).
     */
    fun others(maxFrequencies: Map<Int, Long>): List<Int> {
        val little = of(maxFrequencies)
        return if (little.isEmpty()) emptyList() else maxFrequencies.keys.filter { it !in little }.sorted()
    }

    /** `taskset`'s hexadecimal mask for [cpus], or null for none. */
    fun mask(cpus: List<Int>): String? =
        if (cpus.isEmpty() || cpus.any { it !in 0..62 }) null
        else cpus.fold(0L) { mask, cpu -> mask or (1L shl cpu) }.toString(16)

    /** This device's little cores. */
    fun ofDevice(): List<Int> = of(deviceFrequencies())

    /** This device's CPUs outside the little cores ([others]). */
    fun othersOfDevice(): List<Int> = others(deviceFrequencies())

    /** Android's taskset, which the app may run on its own processes. */
    const val TASKSET = "/system/bin/taskset"

    /** Each CPU's highest frequency; empty when one of them cannot be read. */
    private fun deviceFrequencies(): Map<Int, Long> {
        val cpus = File("/sys/devices/system/cpu").listFiles().orEmpty()
            .mapNotNull { Regex("cpu([0-9]+)").matchEntire(it.name)?.groupValues?.get(1)?.toInt() }
        return cpus.associateWith { cpu ->
            try {
                File("/sys/devices/system/cpu/cpu$cpu/cpufreq/cpuinfo_max_freq").readText().trim().toLong()
            } catch (_: Exception) {
                return emptyMap()
            }
        }
    }
}

/**
 * The engine's start on the CPUs outside the little cores ([LittleCores.others]) through taskset,
 * so that the wineserver, Game.exe and every other program the engine starts inherit them, while
 * Xvfb stays on the little cores ([LittleCoresStart]). The game's main thread hands its work to its
 * other threads one item at a time, each a wineserver round trip and a wake-up of another thread;
 * with all of them on the fast cores a game entry's loading phase took 576 ms instead of 637 ms on
 * a phone with an SM6115 chip (5 entries each, 2026-11-13), with the same frame rate in town.
 * Firmware that refuses taskset's `sched_setaffinity` (issue #13) is found once per app process by
 * `taskset <mask> true` ([accepts]); the engine then starts as before.
 */
object EngineCores {
    /** The result of [accepts] for this app process, once it ran. */
    @Volatile private var checked: Boolean? = null

    /**
     * The engine's program and arguments: [command] (its program first) through taskset with
     * [mask], or [command] as it is when there is no mask or taskset was refused.
     */
    fun command(mask: String?, accepted: Boolean, command: List<String>): Pair<String, List<String>> =
        if (mask != null && accepted) LittleCores.TASKSET to listOf("taskset", mask) + command
        else command.first() to command

    /**
     * Whether taskset with [mask] runs a program on this phone: [check] runs once per app process
     * (`taskset <mask> true`, exit status 0), later calls give its result.
     */
    fun accepts(check: () -> Boolean): Boolean = checked ?: check().also { checked = it }
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
