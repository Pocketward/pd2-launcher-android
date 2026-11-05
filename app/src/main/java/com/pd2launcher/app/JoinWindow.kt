package com.pd2launcher.app

import java.util.Locale

/**
 * What the phone did while the game joined an online game (issue #17): from the engine's network
 * line of a connect to a game server (port [GAME_PORT]) to that socket's close, or at most
 * [MAX_MS], the service samples the phone [SAMPLES_PER_SECOND] times a second ([Sample]). Each
 * sample gives one line ([sampleLine]): the time from the connect, the game's CPU use, its three
 * busiest threads (name, state, context switches), its presents, the speed of the fastest cores
 * and the free memory. The window's end gives one more line ([summary]) for the whole window. A
 * close at about 15 s with the game's busiest thread near 100 % points to a client that is too
 * busy to finish the join; a close with an idle game points to an answer that did not come. The
 * engine's network lines come only with Detailed logging, so the window does too. Not
 * thread-safe: the service uses it on its worker.
 */
class JoinWindow {
    /**
     * One thread of the game at a sample: its CPU ticks (user + system, 1/100 s) and its state
     * (`R` running, `S` asleep, `D` in an uninterruptible wait, ...) from its `stat`; for the
     * busiest threads only ([details]) also its name (`comm`) and its voluntary and nonvoluntary
     * context switches so far (`status`), null where they were not read.
     */
    data class ThreadSample(
        val ticks: Long,
        val state: String,
        val name: String? = null,
        val voluntary: Long? = null,
        val nonvoluntary: Long? = null,
    )

    /**
     * One look at the phone at [atMs] (elapsed ms): each thread of the game ([threads], by
     * thread id; empty when the game is not found), the game's presents so far ([presents], a
     * 32-bit count), the speed of each CPU now ([coreKhz], by CPU number), MemAvailable
     * ([availableKb]), the app's network traffic so far ([traffic]; null: Android does not count
     * it) and the Wi-Fi signal ([wifi], [WifiFacts.signal]; null: no Wi-Fi or not readable); null
     * where it could not be read.
     */
    data class Sample(
        val atMs: Long,
        val threads: Map<Int, ThreadSample>,
        val presents: Long?,
        val coreKhz: Map<Int, Long>,
        val availableKb: Long?,
        val traffic: Traffic? = null,
        val wifi: String? = null,
    )

    private class Open(val socket: String, val peer: String, val startMs: Long) {
        val samples = mutableListOf<Sample>()
    }

    private var open: Open? = null

    /** Whether a window is open: the service samples [SAMPLES_PER_SECOND] times a second while it is. */
    val isOpen: Boolean get() = open != null

    /** The open window's connect (elapsed ms), else null. */
    val startMs: Long? get() = open?.startMs

    /**
     * The engine's [line] at [nowMs] (elapsed ms): a connect to a game server opens the window
     * (null), the close of its socket ends it (the summary line). Every other line gives null.
     */
    fun line(line: String, nowMs: Long, maxKhz: Map<Int, Long>): String? {
        if (!line.contains(NETWORK)) return null
        connect(line)?.let { (socket, peer) ->
            if (open == null) open = Open(socket, peer, nowMs)
            return null
        }
        val current = open ?: return null
        if (closed(line) == current.socket) {
            open = null
            return summary(current, nowMs, "socket closed", maxKhz)
        }
        return null
    }

    /**
     * A [sample] while the window is open: its line, and the summary line when the window
     * reached [MAX_MS]. Nothing when no window is open.
     */
    fun sample(sample: Sample, maxKhz: Map<Int, Long>): List<String> {
        val current = open ?: return emptyList()
        val lines = mutableListOf(sampleLine(current.startMs, current.samples.lastOrNull(), sample, maxKhz))
        current.samples += sample
        if (sample.atMs - current.startMs >= MAX_MS) {
            open = null
            lines += summary(current, sample.atMs, "socket still open (the join went on)", maxKhz)
        }
        return lines
    }

    companion object {
        /** The game servers' port (PD2's game servers, as Diablo II's). */
        const val GAME_PORT = 4000

        /** The longest window: the player's failed joins closed the socket about 15 s after its connect. */
        const val MAX_MS = 20_000L

        /** How often the service samples the phone while a window is open. */
        const val SAMPLES_PER_SECOND = 4

        /** The busiest threads that each sample line names. */
        const val BUSIEST = 3

        /** The ticks of a second in `/proc/<pid>/task/<tid>/stat` (USER_HZ). */
        const val TICKS_PER_SECOND = 100.0

        private const val NETWORK = "pd2-launcher: network "

        /** `pid socket` and `address:port` of a connect line to [GAME_PORT], else null. */
        fun connect(line: String): Pair<String, String>? {
            val match = CONNECT.find(line) ?: return null
            val (pid, socket, address, port) = match.destructured
            if (port.toIntOrNull() != GAME_PORT) return null
            return "$pid $socket" to "$address:$port"
        }

        /** `pid socket` of a closesocket line, else null. */
        fun closed(line: String): String? {
            val match = CLOSE.find(line) ?: return null
            val (pid, socket) = match.destructured
            return "$pid $socket"
        }

        // The engine writes a Winsock line after its UTC time, or in a record window after the
        // time from the window's connect ("+0.250 s"). The lines of the 10 s before a window
        // ("-3.000 s") were seen before, so they open and end nothing. Android's ICU regex needs
        // '}' escaped.
        private val CONNECT = Regex("""(?:UTC|\+[0-9]+\.[0-9]+ s) ([0-9a-f]+):[0-9a-f]+ connect socket (0x[0-9a-f]+), addr \{ family AF_INET6?, address ([0-9a-f.:]+), port (\d+) \}""")
        private val CLOSE = Regex("""(?:UTC|\+[0-9]+\.[0-9]+ s) ([0-9a-f]+):[0-9a-f]+ closesocket (0x[0-9a-f]+)""")

        /**
         * Each thread of the process [pid] from `<proc>/<pid>/task/<tid>/stat`: its state (field
         * 3) and its CPU ticks (fields 14 and 15), after the name in parentheses; empty when the
         * process is gone.
         */
        fun threads(proc: java.io.File, pid: Int): Map<Int, ThreadSample> =
            java.io.File(proc, "$pid/task").listFiles().orEmpty().mapNotNull { task ->
                val tid = task.name.toIntOrNull() ?: return@mapNotNull null
                val stat = try {
                    java.io.File(task, "stat").readText()
                } catch (_: java.io.IOException) {
                    return@mapNotNull null
                }
                val fields = stat.substringAfterLast(") ").split(' ')
                val state = fields.getOrNull(0)?.takeIf { it.isNotEmpty() } ?: return@mapNotNull null
                val utime = fields.getOrNull(11)?.toLongOrNull() ?: return@mapNotNull null
                val stime = fields.getOrNull(12)?.toLongOrNull() ?: return@mapNotNull null
                tid to ThreadSample(utime + stime, state)
            }.toMap()

        /**
         * The [count] busiest of [threads] since [previous] (by CPU ticks; with no previous
         * sample, by all their ticks), the busiest first.
         */
        fun busiest(previous: Map<Int, ThreadSample>?, threads: Map<Int, ThreadSample>, count: Int = BUSIEST): List<Int> =
            threads.entries
                .sortedWith(compareByDescending<Map.Entry<Int, ThreadSample>> { (tid, now) -> now.ticks - (previous?.get(tid)?.ticks ?: 0L) }.thenBy { it.key })
                .take(count)
                .map { it.key }

        /** [threads] with the name and the context switches of each of [tids] (`comm`, `status`). */
        fun details(proc: java.io.File, pid: Int, threads: Map<Int, ThreadSample>, tids: List<Int>): Map<Int, ThreadSample> =
            threads.mapValues { (tid, thread) ->
                if (tid !in tids) return@mapValues thread
                val task = java.io.File(proc, "$pid/task/$tid")
                val name = try {
                    java.io.File(task, "comm").readText().trim()
                } catch (_: java.io.IOException) {
                    null
                }
                val status = try {
                    java.io.File(task, "status").readLines()
                } catch (_: java.io.IOException) {
                    emptyList()
                }
                fun field(key: String) = status.firstOrNull { it.startsWith("$key:") }?.substringAfter(':')?.trim()?.toLongOrNull()
                thread.copy(name = name, voluntary = field("voluntary_ctxt_switches"), nonvoluntary = field("nonvoluntary_ctxt_switches"))
            }

        /** The value of `<cpu>/cpuN/cpufreq/<name>` (kHz) of each CPU N that has one. */
        fun coreKhz(cpu: java.io.File, name: String): Map<Int, Long> =
            cpu.listFiles().orEmpty().mapNotNull { core ->
                val n = core.name.removePrefix("cpu").takeIf { core.name.startsWith("cpu") }?.toIntOrNull() ?: return@mapNotNull null
                val khz = try {
                    java.io.File(core, "cpufreq/$name").readText().trim().toLongOrNull()
                } catch (_: java.io.IOException) {
                    null
                } ?: return@mapNotNull null
                n to khz
            }.toMap()

        /**
         * The line of one [sample] of a window that opened at [startMs], after the [previous]
         * sample (null for the first): `join sample +1.25 s: Game.exe 1.02 cores; threads 4243
         * "Game.exe" 98 % R vol 120 nonvol 34, ...; presents 60.0/s; fastest cores 2323 MHz;
         * MemAvailable 1171 MB`. The CPU use and the presents are since the previous sample; the
         * context switches are the thread's counts so far.
         */
        fun sampleLine(startMs: Long, previous: Sample?, sample: Sample, maxKhz: Map<Int, Long>): String {
            val parts = mutableListOf<String>()
            val span = previous?.let { (sample.atMs - it.atMs) / 1000.0 }?.takeIf { it > 0 }
            if (sample.threads.isEmpty()) {
                parts += "Game.exe not found"
            } else {
                val before = previous?.threads?.takeIf { it.isNotEmpty() && span != null }
                if (before != null && span != null) {
                    val used = sample.threads.entries.sumOf { (tid, now) -> now.ticks - (before[tid]?.ticks ?: now.ticks) }
                    parts += String.format(Locale.ROOT, "Game.exe %.2f cores", used / TICKS_PER_SECOND / span)
                } else {
                    parts += "Game.exe ${sample.threads.size} threads"
                }
                parts += "threads " + busiest(before, sample.threads).joinToString(", ") { tid ->
                    val thread = sample.threads.getValue(tid)
                    val share = if (before != null && span != null) {
                        String.format(Locale.ROOT, " %.0f %%", (thread.ticks - (before[tid]?.ticks ?: thread.ticks)) / TICKS_PER_SECOND / span * 100)
                    } else {
                        ""
                    }
                    "$tid \"${thread.name ?: "?"}\"$share ${thread.state} vol ${thread.voluntary ?: "?"} nonvol ${thread.nonvoluntary ?: "?"}"
                }
            }
            val presents = sample.presents
            val earlier = previous?.presents
            parts += if (presents != null && earlier != null && span != null) {
                String.format(Locale.ROOT, "presents %.1f/s", ((presents - earlier) and 0xFFFFFFFFL) / span)
            } else if (presents != null) {
                "presents $presents"
            } else {
                "presents not measured"
            }
            val top = maxKhz.values.maxOrNull()
            val speeds = maxKhz.filterValues { it == top }.keys.mapNotNull { sample.coreKhz[it] }
            parts += if (top != null && speeds.isNotEmpty()) {
                if (speeds.min() == speeds.max()) "fastest cores ${speeds.min() / 1000} MHz" else "fastest cores ${speeds.min() / 1000} to ${speeds.max() / 1000} MHz"
            } else {
                "CPU speed not readable"
            }
            sample.availableKb?.let { parts += "MemAvailable ${it / 1024} MB" }
            parts += Traffic.change(previous?.traffic, sample.traffic)
            // The Wi-Fi signal at the first sample and at each change.
            if (sample.wifi != null && (previous == null || previous.wifi != sample.wifi)) parts += "Wi-Fi ${sample.wifi}"
            return String.format(Locale.ROOT, "join sample %+.2f s: ", (sample.atMs - startMs) / 1000.0) + parts.joinToString("; ")
        }

        /** The line for the window [open] that ended at [endMs] because of [why]. */
        private fun summary(open: Open, endMs: Long, why: String, maxKhz: Map<Int, Long>): String {
            val seconds = (endMs - open.startMs) / 1000.0
            val samples = open.samples
            val parts = mutableListOf<String>()
            val first = samples.firstOrNull { it.threads.isNotEmpty() }
            val last = samples.lastOrNull { it.threads.isNotEmpty() }
            if (first != null && last != null && last.atMs > first.atMs) {
                val span = (last.atMs - first.atMs) / 1000.0
                val used = last.threads.mapValues { (tid, thread) -> thread.ticks - (first.threads[tid]?.ticks ?: thread.ticks) }
                val cores = used.values.sum() / TICKS_PER_SECOND / span
                val busiest = (used.values.maxOrNull() ?: 0L) / TICKS_PER_SECOND / span * 100
                parts += String.format(Locale.ROOT, "Game.exe %.2f cores (busiest thread %.0f %%)", cores, busiest)
            } else {
                parts += "Game.exe not measured"
            }
            val counts = samples.filter { it.presents != null }
            if (counts.size >= 2) {
                val rates = counts.zipWithNext { a, b -> ((b.presents!! - a.presents!!) and 0xFFFFFFFFL) * 1000.0 / (b.atMs - a.atMs) }
                val all = ((counts.last().presents!! - counts.first().presents!!) and 0xFFFFFFFFL) * 1000.0 / (counts.last().atMs - counts.first().atMs)
                parts += String.format(Locale.ROOT, "presents %.1f/s (each second %.0f to %.0f)", all, rates.min(), rates.max())
            } else {
                parts += "presents not measured"
            }
            val top = maxKhz.values.maxOrNull()
            val fastest = maxKhz.filterValues { it == top }.keys
            val speeds = samples.flatMap { sample -> fastest.mapNotNull { sample.coreKhz[it] } }
            if (top != null && speeds.isNotEmpty()) {
                parts += "fastest cores ${speeds.min() / 1000} to ${speeds.max() / 1000} MHz of ${top / 1000}"
            } else {
                parts += "CPU speed not readable"
            }
            val free = samples.mapNotNull { it.availableKb }
            if (free.isNotEmpty()) parts += "MemAvailable ${free.min() / 1024} to ${free.max() / 1024} MB"
            val firstTraffic = samples.firstOrNull()?.traffic
            val lastTraffic = samples.lastOrNull()?.traffic
            if (samples.isNotEmpty()) {
                parts += if (firstTraffic != null && lastTraffic != null) "traffic " + (lastTraffic - firstTraffic).text() else "traffic not counted"
            }
            return String.format(Locale.ROOT, "join window to game server %s, %.1f s, %s: ", open.peer, seconds, why) +
                parts.joinToString(", ") + ", ${samples.size} samples"
        }
    }
}
