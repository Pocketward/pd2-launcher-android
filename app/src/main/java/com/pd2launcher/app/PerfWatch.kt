package com.pd2launcher.app

import android.os.PowerManager
import java.io.File
import java.io.IOException
import java.util.Locale

/**
 * Detailed logging: what the game's frame rate depends on, one line for each look while the game
 * runs (the session service's memory look, every [MemoryLog.POLL_MS]). Issue 21: the same tablet gave
 * one player 20 fps less than ours, and the logs could not say why. The line has the frames on the
 * screen since the last look, the CPU time of the app's busiest processes and of the game's busiest
 * thread (in cores: 1.00 is one core all the time; a thread near 1.00 limits the game), each CPU
 * cluster's clock now and its highest clock, and Android's thermal status and headroom. An app cannot
 * read the GPU's clock or load or the CPU clock's cap (SELinux), so the line has none of them.
 * Not thread-safe: the session service's worker uses it alone.
 */
class PerfWatch {
    /** One CPU cluster: its [cpus] ("0-3"), its clock [nowKhz] and its highest [maxKhz] (null: not readable). */
    data class Cluster(val cpus: String, val nowKhz: Long?, val maxKhz: Long?)

    /**
     * One look at [now] (elapsed real time, ms): the present counter's [presents] (null: none), the
     * CPU ticks of each process by name ([processTicks]), of each thread of the game by its id
     * ([gameThreadTicks]), the [clusters], Android's [thermal] status and its [headroom] (null: not
     * known), and whether the game was on the screen ([shown]).
     */
    data class Look(
        val now: Long,
        val presents: Long?,
        val processTicks: Map<String, Long>,
        val gameThreadTicks: Map<Int, Long>,
        val clusters: List<Cluster>,
        val thermal: String,
        val headroom: Float?,
        val shown: Boolean,
    )

    private var last: Look? = null

    /** The line for [look], against the look before; null for the first look of a game. [ticksPerSecond] is the kernel's clock tick rate. */
    fun next(look: Look, ticksPerSecond: Long): String? {
        val before = last
        last = look
        if (before == null || look.now <= before.now) return null
        val seconds = (look.now - before.now) / 1000.0
        val cores = { ticks: Long -> ticks / (ticksPerSecond * seconds) }
        val fps = if (look.presents != null && before.presents != null) {
            String.format(Locale.ROOT, "%.1f fps", ((look.presents - before.presents) and 0xFFFFFFFFL) / seconds) +
                if (look.shown && before.shown) "" else " (the game was away from the screen)"
        } else {
            "no present counter"
        }
        val processes = look.processTicks.mapNotNull { (name, ticks) -> before.processTicks[name]?.let { name to ticks - it } }
            .filter { it.second > 0 }.sortedByDescending { it.second }.take(TOP_PROCESSES)
            .joinToString(", ") { (name, ticks) -> "$name ${format(cores(ticks))}" }.ifEmpty { "none" }
        val thread = look.gameThreadTicks.mapNotNull { (id, ticks) -> before.gameThreadTicks[id]?.let { ticks - it } }.maxOrNull()
        val clusters = look.clusters.joinToString(", ") { "cpu${it.cpus} ${ghz(it.nowKhz)}/${ghz(it.maxKhz)} GHz" }.ifEmpty { "not readable" }
        val headroom = look.headroom?.let { String.format(Locale.ROOT, ", headroom %.2f", it) } ?: ""
        return String.format(Locale.ROOT, "perf %.1f s: ", seconds) + fps +
            "; CPU in cores: $processes" + (thread?.let { "; the game's busiest thread ${format(cores(it))}" } ?: "") +
            "; clocks now/highest: $clusters; thermal ${look.thermal}$headroom"
    }

    /** The game ended (or Detailed logging went off): the next game starts again. */
    fun reset() {
        last = null
    }

    private fun format(cores: Double): String = String.format(Locale.ROOT, "%.2f", cores)

    private fun ghz(khz: Long?): String = khz?.let { String.format(Locale.ROOT, "%.2f", it / 1_000_000.0) } ?: "?"

    companion object {
        const val TOP_PROCESSES = 5

        /** The name the line and device.txt give Android's thermal [status] ([PowerManager.getCurrentThermalStatus]). */
        fun thermal(status: Int): String = when (status) {
            PowerManager.THERMAL_STATUS_NONE -> "none"
            PowerManager.THERMAL_STATUS_LIGHT -> "light"
            PowerManager.THERMAL_STATUS_MODERATE -> "moderate"
            PowerManager.THERMAL_STATUS_SEVERE -> "severe"
            PowerManager.THERMAL_STATUS_CRITICAL -> "critical"
            PowerManager.THERMAL_STATUS_EMERGENCY -> "emergency"
            PowerManager.THERMAL_STATUS_SHUTDOWN -> "shutdown"
            else -> "level $status"
        }

        /** The user and system CPU ticks in a `/proc/<pid>/stat` or `/proc/<pid>/task/<tid>/stat` [text]; null when it is not one. */
        fun ticks(text: String): Long? {
            // The name (field 2) can hold spaces and parentheses: the fields after it start after the last ')'.
            val fields = text.substringAfterLast(')', "").trim().split(' ')
            // utime and stime are fields 14 and 15: the 12th and 13th after the name.
            return if (fields.size > 12) fields[11].toLongOrNull()?.let { user -> fields[12].toLongOrNull()?.let { user + it } } else null
        }

        /** The CPU ticks of [processes] and of the app's own process [appPid] (as [APP]) under [proc], summed by name. */
        fun processTicks(proc: File, processes: List<UidProcess>, appPid: Int): Map<String, Long> =
            (processes + UidProcess(appPid, APP)).mapNotNull { process -> read(File(proc, "${process.pid}/stat"))?.let(::ticks)?.let { process.name to it } }
                .groupBy({ it.first }, { it.second }).mapValues { it.value.sum() }

        /** The CPU ticks of each thread of the process [pid] under [proc], by thread id. */
        fun threadTicks(proc: File, pid: Int): Map<Int, Long> =
            File(proc, "$pid/task").listFiles().orEmpty().mapNotNull { task ->
                task.name.toIntOrNull()?.let { id -> read(File(task, "stat"))?.let(::ticks)?.let { id to it } }
            }.toMap()

        /** Each CPU cluster under [cpufreq] (`/sys/devices/system/cpu/cpufreq`): its `policy*` folders. */
        fun clusters(cpufreq: File): List<Cluster> =
            cpufreq.listFiles().orEmpty().filter { it.name.startsWith("policy") }.sortedBy { it.name.removePrefix("policy").toIntOrNull() ?: 0 }
                .map { policy ->
                    val cpus = read(File(policy, "related_cpus"))?.trim()?.split(Regex("\\s+"))?.mapNotNull(String::toIntOrNull).orEmpty()
                    val named = when {
                        cpus.isEmpty() -> policy.name.removePrefix("policy")
                        cpus.size == 1 -> "${cpus[0]}"
                        else -> "${cpus.first()}-${cpus.last()}"
                    }
                    Cluster(named, read(File(policy, "scaling_cur_freq"))?.trim()?.toLongOrNull(), read(File(policy, "cpuinfo_max_freq"))?.trim()?.toLongOrNull())
                }

        /** The app's own process in [processTicks]. */
        const val APP = "the app"

        private fun read(file: File): String? = try {
            file.readText()
        } catch (_: IOException) {
            null
        }
    }
}
