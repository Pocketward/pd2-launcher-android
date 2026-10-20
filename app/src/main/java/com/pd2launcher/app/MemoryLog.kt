package com.pd2launcher.app

import android.app.ActivityManager
import android.app.ApplicationExitInfo
import android.content.ComponentCallbacks2
import java.io.File
import java.io.IOException
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale
import java.util.TimeZone
import kotlin.math.abs

/**
 * Memory pressure in the service log, which Share logs and Save logs give: the device's memory
 * (`/proc/meminfo`) when the game starts and when it stops, and while it runs a short line each
 * time MemAvailable moved by [CHANGE_KB] or more since the last line, at most one in
 * [INTERVAL_MS] ([Watch]); each `onTrimMemory` level ([trimLevel]); and at each start of the
 * session service, how the app's earlier processes ended ([Exit], from
 * [ActivityManager.getHistoricalProcessExitReasons]), so a kill for low memory shows. The app
 * only reads and logs: its place for Android's memory killer stays as it is.
 */
object MemoryLog {
    /** The change of MemAvailable since the last line that gives a line while the game runs. */
    const val CHANGE_KB = 25 * 1024L

    /** The shortest time between two of those lines. */
    const val INTERVAL_MS = 60_000L

    /** How often the session service looks while the game runs. */
    const val POLL_MS = 10_000L

    /** The most process ends logged at one start: the newest. */
    const val MAX_EXITS = 16

    /**
     * The app's preferences that keep the keys ([key]) of the process ends that Android listed at
     * the last look, so each end is logged once. Keys, not a time: a change of the device's clock
     * does not hide an end.
     */
    const val PREFERENCES = "memory_log"
    const val SEEN_EXITS = "seen_exits"

    private const val UNREADABLE = "/proc/meminfo unreadable"
    private val MEMINFO_LINE = Regex("""^(\w+):\s+(\d+) kB$""")
    private val LINE_BREAK = Regex("\r\n|\r|\n")

    /** The device's memory from `/proc/meminfo`, in kB. */
    data class Memory(val totalKb: Long, val availableKb: Long, val swapFreeKb: Long)

    /**
     * MemTotal, MemAvailable and SwapFree of `/proc/meminfo`'s [text]; null when one is missing. A
     * line whose number does not fit a Long is skipped.
     */
    fun parse(text: String): Memory? {
        val values = text.lineSequence()
            .mapNotNull { line ->
                MEMINFO_LINE.matchEntire(line.trim())?.destructured?.let { (name, kb) -> kb.toLongOrNull()?.let { name to it } }
            }
            .toMap()
        return Memory(
            totalKb = values["MemTotal"] ?: return null,
            availableKb = values["MemAvailable"] ?: return null,
            swapFreeKb = values["SwapFree"] ?: return null,
        )
    }

    /** The device's memory now; null when `/proc/meminfo` cannot be read. */
    fun read(): Memory? = try {
        parse(File("/proc/meminfo").readText())
    } catch (_: IOException) {
        null
    }

    /** MemTotal, MemAvailable and SwapFree in MB. */
    fun describe(memory: Memory?): String =
        memory?.let { "MemTotal ${mb(it.totalKb)} MB, ${available(it)}" } ?: UNREADABLE

    /** MemAvailable and SwapFree in MB. */
    fun available(memory: Memory?): String =
        memory?.let { "MemAvailable ${mb(it.availableKb)} MB, SwapFree ${mb(it.swapFreeKb)} MB" } ?: UNREADABLE

    /** Whether MemAvailable [availableKb] at [now] gives a line after the last line, which had [lastKb] at [lastAt]. */
    fun due(lastKb: Long, lastAt: Long, availableKb: Long, now: Long): Boolean =
        now - lastAt >= INTERVAL_MS && abs(availableKb - lastKb) >= CHANGE_KB

    /**
     * The memory lines of the game: one as it starts and one as it stops, and while it runs one
     * each time MemAvailable moved by [CHANGE_KB] or more since the last line, at most one in
     * [INTERVAL_MS] ([due]). Not thread-safe: the session service uses it on its worker only.
     */
    class Watch {
        /** Whether the game ran at the last look. */
        var running = false
            private set
        /** MemAvailable of the last line while the game runs (null: none yet), and when it was (elapsed real time). */
        private var lastKb: Long? = null
        private var lastAt = 0L

        /** The line for now, if any: the game [running] or not, and the device's [memory] at [now]. */
        fun next(running: Boolean, memory: Memory?, now: Long): String? {
            if (running != this.running) {
                this.running = running
                lastKb = memory?.availableKb?.takeIf { running }
                lastAt = now
                return "${if (running) "game started" else "game stopped"}: memory ${describe(memory)}"
            }
            if (!running || memory == null) return null
            val last = lastKb
            if (last != null && !due(last, lastAt, memory.availableKb, now)) return null
            lastKb = memory.availableKb
            lastAt = now
            if (last == null) return "memory: ${describe(memory)}"
            val change = (memory.availableKb - last) / 1024
            return "memory: MemAvailable ${mb(memory.availableKb)} MB (${if (change > 0) "+" else ""}$change MB), SwapFree ${mb(memory.swapFreeKb)} MB"
        }
    }

    /**
     * An `onTrimMemory` level's name with its number. Android 14 and later give only UI_HIDDEN and
     * BACKGROUND; Android 13 also gives the RUNNING levels while the app's process is not cached
     * (in front, or with the session's foreground service).
     */
    @Suppress("DEPRECATION")
    fun trimLevel(level: Int): String {
        val name = when (level) {
            ComponentCallbacks2.TRIM_MEMORY_RUNNING_MODERATE -> "RUNNING_MODERATE"
            ComponentCallbacks2.TRIM_MEMORY_RUNNING_LOW -> "RUNNING_LOW"
            ComponentCallbacks2.TRIM_MEMORY_RUNNING_CRITICAL -> "RUNNING_CRITICAL"
            ComponentCallbacks2.TRIM_MEMORY_UI_HIDDEN -> "UI_HIDDEN"
            ComponentCallbacks2.TRIM_MEMORY_BACKGROUND -> "BACKGROUND"
            ComponentCallbacks2.TRIM_MEMORY_MODERATE -> "MODERATE"
            ComponentCallbacks2.TRIM_MEMORY_COMPLETE -> "COMPLETE"
            else -> return "level $level"
        }
        return "$name ($level)"
    }

    /** One end of an earlier process of the app: [ApplicationExitInfo]'s values, the sizes in kB. */
    data class Exit(
        val timestamp: Long,
        val pid: Int,
        val process: String,
        val reason: Int,
        val status: Int,
        val importance: Int,
        val pssKb: Long,
        val rssKb: Long,
        val description: String?,
    )

    /** The key of [exit] in [SEEN_EXITS]: its pid and its time. */
    fun key(exit: Exit): String = "${exit.pid}@${exit.timestamp}"

    /**
     * The [exits] whose keys the last look did not have ([seen]), oldest first: the newest
     * [MAX_EXITS] at most.
     */
    fun unseen(exits: List<Exit>, seen: Set<String>): List<Exit> =
        exits.filter { key(it) !in seen }.sortedBy { it.timestamp }.takeLast(MAX_EXITS)

    /** The keys to keep after a look at [exits]: those of all of them (Android lists a few only). */
    fun keys(exits: List<Exit>): Set<String> = exits.mapTo(HashSet(), ::key)

    /** One line for [exit], its time in [zone]; a line break in its description becomes a space. */
    fun describe(exit: Exit, zone: TimeZone = TimeZone.getDefault()): String {
        val time = SimpleDateFormat("yyyy-MM-dd HH:mm:ss", Locale.ROOT).apply { timeZone = zone }.format(Date(exit.timestamp))
        return "an earlier app process ended: $time, pid ${exit.pid} (${exit.process}): ${exitReason(exit.reason)}, " +
            "status ${exit.status}, importance ${exit.importance}, Pss ${mb(exit.pssKb)} MB, Rss ${mb(exit.rssKb)} MB" +
            (exit.description?.let { ", \"${it.replace(LINE_BREAK, " ")}\"" } ?: "")
    }

    /** [ApplicationExitInfo]'s reason by its name. */
    fun exitReason(reason: Int): String = when (reason) {
        ApplicationExitInfo.REASON_UNKNOWN -> "UNKNOWN"
        ApplicationExitInfo.REASON_EXIT_SELF -> "EXIT_SELF"
        ApplicationExitInfo.REASON_SIGNALED -> "SIGNALED"
        ApplicationExitInfo.REASON_LOW_MEMORY -> "LOW_MEMORY"
        ApplicationExitInfo.REASON_CRASH -> "CRASH"
        ApplicationExitInfo.REASON_CRASH_NATIVE -> "CRASH_NATIVE"
        ApplicationExitInfo.REASON_ANR -> "ANR"
        ApplicationExitInfo.REASON_INITIALIZATION_FAILURE -> "INITIALIZATION_FAILURE"
        ApplicationExitInfo.REASON_PERMISSION_CHANGE -> "PERMISSION_CHANGE"
        ApplicationExitInfo.REASON_EXCESSIVE_RESOURCE_USAGE -> "EXCESSIVE_RESOURCE_USAGE"
        ApplicationExitInfo.REASON_USER_REQUESTED -> "USER_REQUESTED"
        ApplicationExitInfo.REASON_USER_STOPPED -> "USER_STOPPED"
        ApplicationExitInfo.REASON_DEPENDENCY_DIED -> "DEPENDENCY_DIED"
        ApplicationExitInfo.REASON_OTHER -> "OTHER"
        ApplicationExitInfo.REASON_FREEZER -> "FREEZER"
        ApplicationExitInfo.REASON_PACKAGE_STATE_CHANGE -> "PACKAGE_STATE_CHANGE"
        ApplicationExitInfo.REASON_PACKAGE_UPDATED -> "PACKAGE_UPDATED"
        else -> "reason $reason"
    }

    private fun mb(kb: Long): Long = kb / 1024
}
