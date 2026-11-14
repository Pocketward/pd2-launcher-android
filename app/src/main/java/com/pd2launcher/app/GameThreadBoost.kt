package com.pd2launcher.app

import android.content.Context
import android.os.Build
import android.os.PerformanceHintManager
import java.io.File
import java.io.IOException
import java.util.Locale
import java.util.concurrent.TimeUnit

/**
 * Test and debug builds (issue 23): two helps for the game's main thread, the game's busiest
 * thread, which Box64 runs and which limits the frame rate on most phones.
 *
 * - Big cores: the thread may run only on the cores that are not the little ones ([LittleCores]),
 *   so that Android's scheduler cannot move it to a slow core. Only that thread: Wine's other
 *   threads, the sound and wineserver stay where Android puts them.
 * - Performance hints (Android's ADPF, [PerformanceHintManager]): a hint session for the thread with
 *   the frame time of the Frame rate as its target, and for each look its CPU time per frame on the
 *   screen as the work time, so that Android raises the CPU clock or moves the thread when frames
 *   take longer than the target.
 *
 * Each one may be refused (firmware that refuses `sched_setaffinity`, as in issue #13; Android with
 * no hint sessions, or one that refuses a thread of the app's Wine child process): then it only
 * writes a log line, and the game runs as without it.
 *
 * Not thread-safe: one loop of the session service uses it.
 */
class GameThreadBoost(private val effects: Effects, private val log: (String) -> Unit) {
    /** What the boost does on the device. */
    interface Effects {
        /** The cores that are not the little ones; empty on a device with one cluster. */
        fun bigCores(): List<Int>

        /** Keeps the thread [tid] on [cpus]; null when done, else why not. */
        fun pin(tid: Int, cpus: List<Int>): String?

        /** A hint session for the thread [tid] with [targetNs] per frame, or why there is none. */
        fun hintSession(tid: Int, targetNs: Long): Result<HintSession>
    }

    /** An open hint session. */
    interface HintSession {
        fun report(workNs: Long)
        fun target(ns: Long)
        fun close()
    }

    /** What the boost is to do now, from the settings: [bigCores], [hints], and the frame time [targetNs]. */
    data class Options(val bigCores: Boolean, val hints: Boolean, val targetNs: Long)

    /**
     * One look at [now] (elapsed real time, ms): the CPU time in ns of each thread of the game by its
     * id ([threadCpuNs]; empty: no game), the names of those threads ([names]), and the present
     * counter ([presents], null: none).
     */
    data class Look(val now: Long, val threadCpuNs: Map<Int, Long>, val names: Map<Int, String>, val presents: Long?)

    private var first: Look? = null
    private var thread: Int? = null
    private var pinned = false
    private var session: HintSession? = null
    private var sessionTried = false
    private var target = 0L
    private var lastCpu: Long? = null
    private var lastPresents: Long? = null
    private var reports = 0
    private var reportedNs = 0L

    /** The look [look] with [options]: chooses the thread once, then pins it and reports its work. */
    fun next(look: Look, options: Options) {
        if (look.threadCpuNs.isEmpty()) {
            stop()
            return
        }
        val chosen = thread ?: choose(look, options) ?: return
        val cpu = look.threadCpuNs[chosen]
        if (cpu == null) {
            // The thread ended (a new game process): choose again.
            stop()
            return
        }
        if (options.bigCores && !pinned) {
            pinned = true
            val cores = effects.bigCores()
            if (cores.isEmpty()) log("game thread boost: thread $chosen stays where Android puts it (no little cores on this device)")
            else {
                val refused = effects.pin(chosen, cores)
                log(
                    if (refused == null) "game thread boost: thread $chosen (${look.names[chosen] ?: "?"}) on the big cores ${range(cores)}"
                    else "game thread boost: thread $chosen could not be kept on the big cores ${range(cores)}: $refused",
                )
            }
        }
        if (options.hints) hint(chosen, cpu, look.presents, options.targetNs) else closeSession()
    }

    /** The game ended or the boost is off: the session closes and the next game chooses again. */
    fun stop() {
        closeSession()
        first = null
        thread = null
        pinned = false
        sessionTried = false
        lastCpu = null
        lastPresents = null
    }

    /** The busiest thread from the first look until [CHOOSE_MS] later; null while that time runs. */
    private fun choose(look: Look, options: Options): Int? {
        val start = first
        if (start == null) {
            first = look
            log("game thread boost: big cores ${if (options.bigCores) "On" else "Off"}, performance hints ${if (options.hints) "On" else "Off"} (Settings → Help → Testing)")
            return null
        }
        if (look.now - start.now < CHOOSE_MS) return null
        val busiest = look.threadCpuNs.mapNotNull { (id, ns) -> start.threadCpuNs[id]?.let { id to ns - it } }.maxByOrNull { it.second }
        if (busiest == null || busiest.second <= 0) return null
        thread = busiest.first
        log(
            String.format(
                Locale.ROOT, "game thread boost: the game's busiest thread is %d (%s), %.2f of a core over %.1f s",
                busiest.first, look.names[busiest.first] ?: "?", busiest.second / 1e6 / (look.now - start.now), (look.now - start.now) / 1000.0,
            ),
        )
        return busiest.first
    }

    private fun hint(tid: Int, cpu: Long, presents: Long?, targetNs: Long) {
        if (!sessionTried) {
            sessionTried = true
            effects.hintSession(tid, targetNs).fold(
                onSuccess = {
                    session = it
                    target = targetNs
                    log(String.format(Locale.ROOT, "performance hints: a session for thread %d, target %.1f ms a frame", tid, targetNs / 1e6))
                },
                onFailure = { log("performance hints: no session for thread $tid (${it.message ?: it.javaClass.simpleName})") },
            )
        }
        val open = session ?: return
        if (targetNs != target) {
            target = targetNs
            open.target(targetNs)
        }
        val beforeCpu = lastCpu
        val beforePresents = lastPresents
        lastCpu = cpu
        lastPresents = presents
        if (beforeCpu == null || beforePresents == null || presents == null) return
        val frames = (presents - beforePresents) and 0xFFFFFFFFL
        // No new picture (a menu that waits, the game away): nothing to say about a frame.
        if (frames <= 0) return
        val work = (cpu - beforeCpu) / frames
        if (work <= 0) return
        open.report(work)
        reports++
        reportedNs += work
    }

    private fun closeSession() {
        session?.let {
            it.close()
            log(
                if (reports > 0) String.format(Locale.ROOT, "performance hints: the session closed after %d reports, %.1f ms of work a frame on average", reports, reportedNs / 1e6 / reports)
                else "performance hints: the session closed with no report",
            )
        }
        session = null
        reports = 0
        reportedNs = 0
    }

    companion object {
        /** Whether this build has the boost and its switches (Settings → Help → Testing): test and debug builds. */
        val AVAILABLE: Boolean get() = BuildConfig.DEBUG || BuildConfig.SHOWN_VERSION != BuildConfig.VERSION_NAME

        /** How long the boost watches the game's threads before it chooses the busiest one. */
        const val CHOOSE_MS = 3_000L

        /** How often the session service looks while the game runs. */
        const val POLL_MS = 250L

        /** "4-7" for consecutive [cpus], else "4,6,7". */
        fun range(cpus: List<Int>): String =
            if (cpus.size > 1 && cpus.zipWithNext().all { (a, b) -> b == a + 1 }) "${cpus.first()}-${cpus.last()}" else cpus.joinToString(",")

        /** The run time in ns in a `/proc/<pid>/task/<tid>/schedstat` [text] (its first field); null when it is not one. */
        fun runNs(text: String): Long? = text.trim().split(' ').firstOrNull()?.toLongOrNull()

        /** The run time in ns and the name of each thread of the process [pid] under [proc]. */
        fun threads(proc: File, pid: Int): Pair<Map<Int, Long>, Map<Int, String>> {
            val times = mutableMapOf<Int, Long>()
            val names = mutableMapOf<Int, String>()
            for (task in File(proc, "$pid/task").listFiles().orEmpty()) {
                val id = task.name.toIntOrNull() ?: continue
                read(File(task, "schedstat"))?.let(::runNs)?.let { times[id] = it }
                read(File(task, "comm"))?.trim()?.let { names[id] = it }
            }
            return times to names
        }

        private fun read(file: File): String? = try {
            file.readText()
        } catch (_: IOException) {
            null
        }
    }
}

/** [GameThreadBoost.Effects] on the device: `taskset -p` and [PerformanceHintManager]. */
class DeviceThreadEffects(private val context: Context) : GameThreadBoost.Effects {
    override fun bigCores(): List<Int> {
        val little = LittleCores.ofDevice()
        if (little.isEmpty()) return emptyList()
        val all = File("/sys/devices/system/cpu").listFiles().orEmpty()
            .mapNotNull { Regex("cpu([0-9]+)").matchEntire(it.name)?.groupValues?.get(1)?.toInt() }.sorted()
        return all - little.toSet()
    }

    override fun pin(tid: Int, cpus: List<Int>): String? {
        val mask = LittleCores.mask(cpus) ?: return "no mask for ${cpus.joinToString(",")}"
        if (!File(TASKSET).canExecute()) return "$TASKSET is not there"
        return try {
            val process = ProcessBuilder(TASKSET, "-p", mask, tid.toString()).redirectErrorStream(true).start()
            if (!process.waitFor(5, TimeUnit.SECONDS)) {
                process.destroy()
                "taskset did not end in 5 s"
            } else {
                val output = process.inputStream.bufferedReader().readText().trim()
                if (process.exitValue() == 0) null else "taskset status ${process.exitValue()}: ${output.take(200)}"
            }
        } catch (error: IOException) {
            "taskset: ${error.message}"
        }
    }

    override fun hintSession(tid: Int, targetNs: Long): Result<GameThreadBoost.HintSession> {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.S) return Result.failure(UnsupportedOperationException("Android ${Build.VERSION.RELEASE} has no hint sessions"))
        val manager = context.getSystemService(PerformanceHintManager::class.java)
            ?: return Result.failure(UnsupportedOperationException("no PerformanceHintManager"))
        if (manager.preferredUpdateRateNanos < 0) return Result.failure(UnsupportedOperationException("hint sessions are not supported on this device"))
        return try {
            val session = manager.createHintSession(intArrayOf(tid), targetNs)
                ?: return Result.failure(UnsupportedOperationException("Android gave no session"))
            Result.success(object : GameThreadBoost.HintSession {
                override fun report(workNs: Long) = session.reportActualWorkDuration(workNs)
                override fun target(ns: Long) = session.updateTargetWorkDuration(ns)
                override fun close() = session.close()
            })
        } catch (error: RuntimeException) {
            Result.failure(error)
        }
    }

    companion object {
        private const val TASKSET = "/system/bin/taskset"
    }
}
