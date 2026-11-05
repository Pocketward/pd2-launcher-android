package com.pd2launcher.app

import android.os.ParcelFileDescriptor
import android.system.ErrnoException
import android.system.Os
import android.util.Log
import java.io.ByteArrayOutputStream
import java.io.File
import java.io.FileInputStream
import java.io.IOException
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean

/** A process the session service started. */
data class OwnedProcess(val pid: Int, val name: String)

/** `kill(2)` with Android's [ErrnoException] as an [IOException]. */
fun sendSignal(pid: Int, signal: Int) {
    try {
        Os.kill(pid, signal)
    } catch (error: ErrnoException) {
        throw IOException(error)
    }
}

/**
 * The processes the session service started and has not yet seen end.
 * Every one of them runs from the App userland, so this
 * is also the installer's "a userland program runs" ([UserlandInstaller] switches `current`
 * only while it is empty).
 */
object OwnedProcesses : UserlandProcesses {
    private val running = ConcurrentHashMap<Int, String>()

    override fun anyRunning(): Boolean = running.isNotEmpty()

    fun isRunning(pid: Int): Boolean = running.containsKey(pid)

    internal fun add(process: OwnedProcess) {
        running[process.pid] = process.name
    }

    internal fun remove(pid: Int) {
        running.remove(pid)
    }
}

/**
 * Starts owned processes through [Spawner]: standard output and error go through a pipe
 * into the process's own [RotatingLog] in [logs], each line with the time the app read it
 * ([StampedOutput]), and every start and exit is logged with pid, name and status or signal in
 * [serviceLog]. The process's own log gets its start line, with the start's time, only before its
 * first output, and then its exit line too: a helper that writes nothing (`stop-wine`, say) has
 * no file of its own, as the service log has both lines. The start lines name the long folders
 * [folders] by their short names ([shortened]: `<userland>`, `<apk>`, `<data>`; device.txt
 * names the data folder).
 *
 * Debug builds: when [harness] gives a program (the seccomp test harness, `seccomp-run`), each
 * process starts under it, `seccomp-run --notify <path> <argv after argv[0]>`: the process and
 * all it starts run under the app seccomp policy of the harness's allowlist, and the harness's
 * own lines (`seccomp-run: BLOCKED ...`, one for each blocked call) also go to logcat with the tag
 * [HARNESS_TAG]. The harness passes signals on to the process; the process's argv[0] is [path].
 */
class ProcessStarter(
    private val logs: File,
    private val serviceLog: RotatingLog,
    private val harness: () -> File? = { null },
    private val folders: List<Pair<String, String>> = emptyList(),
) {
    private val lock = Any()
    /**
     * One log per name: a process started again under a name (the engine after a restart, each
     * `wineserver -k`) writes the same file while an earlier one's drain may still be writing it,
     * and two logs of one file would each count its size and rotate it under the other.
     */
    private val logsByName = HashMap<String, RotatingLog>()

    /**
     * Starts [path] with [argv] and [env] as [name]; [stdin] (null: /dev/null) and [fd3] (the
     * child's descriptor 3, when given) stay owned by the caller. Each line of its output also
     * goes to [mirror], when there is one. [rewrite] gives each line as its log writes it (null:
     * left out): the expected lines of a helper can go in one line. [onExit] runs on the reaper
     * thread after the exit is logged.
     */
    fun start(
        name: String,
        path: String,
        argv: List<String>,
        env: List<String>,
        stdin: ParcelFileDescriptor? = null,
        fd3: ParcelFileDescriptor? = null,
        mirror: ((String) -> Unit)? = null,
        rewrite: (String) -> String? = { it },
        onExit: (OwnedProcess, ExitStatus) -> Unit = { _, _ -> },
    ): OwnedProcess {
        val wrapper = harness()
        val (program, arguments) = if (wrapper == null) path to argv else wrapper.path to listOf(wrapper.path, "--notify", path) + argv.drop(1)
        val lines: ((String) -> Unit)? = if (wrapper == null) {
            mirror
        } else {
            { line ->
                if (line.startsWith(HARNESS_LINE)) Log.w(HARNESS_TAG, "$name: $line")
                mirror?.invoke(line)
            }
        }
        val log = synchronized(lock) {
            logsByName.getOrPut(name) {
                val (limit, keep) = logSize(name, AppSettingsStore.testBuild() != null)
                RotatingLog(logs, name, limit, keep)
            }
        }
        // The process's own log has its start line once its first output comes ([drain]).
        val opened = AtomicBoolean(false)
        var startLine = ""
        val startedAt = System.currentTimeMillis()
        val open = { if (opened.compareAndSet(false, true)) log.line(startLine, startedAt) }
        val pipe = ParcelFileDescriptor.createPipe()
        val devNull = if (stdin == null) ParcelFileDescriptor.open(File("/dev/null"), ParcelFileDescriptor.MODE_READ_ONLY) else null
        val drained = CountDownLatch(1)
        val process: OwnedProcess
        try {
            // Held until the process is registered, so its exit cannot be handled first.
            synchronized(lock) {
                var started: OwnedProcess? = null
                val pid = Spawner.spawn(program, arguments, env, stdin ?: devNull!!, pipe[1], fd3) { status ->
                    synchronized(lock) { OwnedProcesses.remove(started?.pid ?: -1) }
                    // The exit is handled whatever the logging does: the spawner drops an
                    // exception here, and with it the exit, so the engine would show as running.
                    try {
                        // Most output is in by now; a descendant holding the pipe must not delay the exit line.
                        drained.await(1, TimeUnit.SECONDS)
                        val line = "exit pid=${started?.pid} name=$name ${describe(status)}"
                        if (opened.get()) log.line(line)
                        log.close()
                        serviceLog.line(line)
                        Log.i(TAG, line)
                    } finally {
                        started?.let { onExit(it, status) }
                    }
                }
                process = OwnedProcess(pid, name)
                started = process
                OwnedProcesses.add(process)
            }
        } catch (error: IOException) {
            pipe[0].close()
            serviceLog.line("start name=$name failed: ${error.message}")
            throw error
        } finally {
            pipe[1].close()
            devNull?.close()
        }
        val line = "start pid=${process.pid} name=$name: ${shortened(arguments.joinToString(" "), folders)}"
        startLine = line
        serviceLog.line(line, startedAt)
        Log.i(TAG, line)
        Thread({ drain(pipe[0], log, drained, open, rewrite, lines) }, "pd2-log-$name").apply { isDaemon = true }.start()
        return process
    }

    /** The output in [read] into [log] ([StampedOutput]); [open] writes the start line before the first output. */
    private fun drain(
        read: ParcelFileDescriptor,
        log: RotatingLog,
        drained: CountDownLatch,
        open: () -> Unit,
        rewrite: (String) -> String?,
        mirror: ((String) -> Unit)?,
    ) {
        val output = StampedOutput(log, open, rewrite, mirror)
        try {
            ParcelFileDescriptor.AutoCloseInputStream(read).use { input: FileInputStream ->
                val buffer = ByteArray(8192)
                while (true) {
                    val n = input.read(buffer)
                    if (n < 0) break
                    output.feed(buffer, n)
                }
                output.end()
            }
        } catch (error: IOException) {
            output.end()
            open()
            log.line("log pipe failed: ${error.message}")
        } finally {
            drained.countDown()
        }
    }

    companion object {
        private const val TAG = "pd2-processes"

        /**
         * The size of one file of the log of the process [name] and the files kept: in a test
         * build ([AppSettingsStore.testBuild]) the engine's log keeps 4 MB in each of 5 files, as
         * Detailed logging's record windows write every network call of a game's join (about
         * 0.7 MB a minute while the player creates games); else [RotatingLog]'s own.
         */
        fun logSize(name: String, testBuild: Boolean): Pair<Long, Int> =
            if (testBuild && name == ENGINE) (4L shl 20) to 5 else RotatingLog.LIMIT to RotatingLog.KEEP

        /** The engine's process name, which names its log (`engine.log`). */
        const val ENGINE = "engine"
        /** The logcat tag of the seccomp test harness's lines (debug builds, [harness]). */
        const val HARNESS_TAG = "pd2-seccomp"
        private const val HARNESS_LINE = "seccomp-run:"

        fun describe(status: ExitStatus): String =
            if (status.signal != null) "signal=${status.signal}${signalName(status.signal)}" else "status=${status.code}"

        /**
         * [text] with each folder of [folders] (its path, then its short name; the more specific
         * first) as its short name: the userland's and the APK's folders are long, and device.txt
         * names the data folder.
         */
        fun shortened(text: String, folders: List<Pair<String, String>>): String =
            folders.fold(text) { shorter, (path, short) -> if (path.isEmpty()) shorter else shorter.replace(path, short) }

        private fun signalName(signal: Int): String = when (signal) {
            1 -> " (SIGHUP)"
            2 -> " (SIGINT)"
            6 -> " (SIGABRT)"
            9 -> " (SIGKILL)"
            11 -> " (SIGSEGV)"
            15 -> " (SIGTERM)"
            else -> ""
        }
    }
}

/** A live process of the app's user ID, as /proc shows it. */
data class UidProcess(val pid: Int, val name: String)

/**
 * Ends every other process of the app's user ID: SIGTERM to
 * all, up to [grace] ms for them to go, then SIGKILL to what is left. [list] returns the live
 * processes of the uid without the app process itself; every step is logged.
 */
class UidCleanup(
    private val list: () -> List<UidProcess>,
    private val signal: (pid: Int, signal: Int) -> Unit,
    private val log: (String) -> Unit,
    private val sleep: (Long) -> Unit = Thread::sleep,
    private val now: () -> Long = { System.nanoTime() / 1_000_000 },
) {
    /** Returns the processes that needed SIGKILL. */
    fun run(grace: Long = GRACE_MS): List<UidProcess> {
        val targets = list()
        if (targets.isEmpty()) {
            log("cleanup: no other process of the uid")
            return emptyList()
        }
        log("cleanup: SIGTERM to ${targets.joinToString { "${it.pid} ${it.name}" }}")
        targets.forEach { send(it, SIGTERM) }
        val termAt = now()
        val deadline = termAt + grace
        var alive = alive(targets)
        while (alive.isNotEmpty() && now() < deadline) {
            sleep(POLL_MS)
            alive = alive(targets)
        }
        (targets - alive.toSet()).forEach { log("cleanup: pid ${it.pid} ${it.name} ended after SIGTERM") }
        if (alive.isEmpty()) return emptyList()
        log("cleanup: SIGKILL ${now() - termAt} ms after SIGTERM to ${alive.joinToString { "${it.pid} ${it.name}" }}")
        alive.forEach { send(it, SIGKILL) }
        // Brief wait so the log can say whether SIGKILL took.
        val killDeadline = now() + KILL_WAIT_MS
        var left = alive(alive)
        while (left.isNotEmpty() && now() < killDeadline) {
            sleep(POLL_MS)
            left = alive(alive)
        }
        (alive - left.toSet()).forEach { log("cleanup: pid ${it.pid} ${it.name} ended after SIGKILL") }
        left.forEach { log("cleanup: pid ${it.pid} ${it.name} still present after SIGKILL") }
        // Leftovers that started meanwhile are not chased: the engine is not running to start more.
        return alive
    }

    /**
     * SIGKILL, with no grace, to every process [list] returns, and again to every one it returns
     * after that, each [POLL_MS], until it returns none and [settled] holds, or [timeout] ms have
     * passed. Returns the processes still listed then.
     */
    fun killAll(timeout: Long, settled: () -> Boolean = { true }): List<UidProcess> {
        val deadline = now() + timeout
        var left = list()
        while ((left.isNotEmpty() || !settled()) && now() < deadline) {
            left.forEach { send(it, SIGKILL) }
            sleep(POLL_MS)
            left = list()
        }
        left.forEach { log("cleanup: pid ${it.pid} ${it.name} still present $timeout ms after SIGKILL") }
        return left
    }

    /** [targets] still present, matched by pid and name (a reused pid is not the same process). */
    private fun alive(targets: List<UidProcess>): List<UidProcess> {
        val now = list().toSet()
        return targets.filter { it in now }
    }

    private fun send(process: UidProcess, number: Int) {
        try {
            signal(process.pid, number)
        } catch (error: Exception) {
            log("cleanup: signal $number to ${process.pid} ${process.name}: ${error.message}")
        }
    }

    companion object {
        const val GRACE_MS = 5_000L
        private const val KILL_WAIT_MS = 2_000L
        private const val POLL_MS = 100L
        const val SIGTERM = 15
        const val SIGKILL = 9

        /**
         * Live (not zombie) processes in [proc] owned by [uid] ([ownerOf] gives a `/proc/<pid>`
         * folder's owner, null if unknown), without [self]. A zombie is already dead; its parent
         * reaps it. Only `/proc/<pid>/stat` of the uid's own processes is read: `status` waits
         * for the memory map lock, which an exiting FEX process holds for seconds, and other
         * apps' processes are never opened.
         */
        fun scan(proc: File, uid: Int, self: Int, ownerOf: (File) -> Int?): List<UidProcess> =
            proc.listFiles().orEmpty().mapNotNull { dir ->
                val pid = dir.name.toIntOrNull() ?: return@mapNotNull null
                if (pid == self || ownerOf(dir) != uid) return@mapNotNull null
                // "<pid> (<comm>) <state> ...": comm may contain spaces and parentheses.
                val stat = try {
                    File(dir, "stat").readText()
                } catch (_: IOException) {
                    return@mapNotNull null
                }
                val close = stat.lastIndexOf(')')
                if (close < 0 || stat.getOrNull(close + 2) == 'Z') return@mapNotNull null
                UidProcess(pid, stat.substring(stat.indexOf('(') + 1, close))
            }.sortedBy { it.pid }
    }
}

/**
 * A process's output into its [log] line by line ([LineSplitter]; a line longer than 4096 bytes in
 * pieces), each with the time the app read it ([RotatingLog.stamp]), which is when the process
 * wrote it, give or take the pipe: one stamp and one write of the file for each read ([feed]). A
 * line with no newline at the end of the output gets its stamp too ([end]). [open] runs before the
 * first write (the start line, [ProcessStarter]); [rewrite] gives each line as the log writes it
 * (null: left out); each line also goes to [mirror] as it came.
 */
class StampedOutput(
    private val log: RotatingLog,
    private val open: () -> Unit = {},
    private val rewrite: (String) -> String? = { it },
    private val mirror: ((String) -> Unit)? = null,
) {
    private val out = ByteArrayOutputStream()
    private var stamp = ""
    private val splitter = LineSplitter { line ->
        rewrite(line)?.let { out.write("$stamp $it\n".toByteArray()) }
        mirror?.invoke(line)
    }

    /** [length] bytes of [bytes], read at [at] (wall clock, ms). */
    fun feed(bytes: ByteArray, length: Int = bytes.size, at: Long = System.currentTimeMillis()) {
        stamp = RotatingLog.stamp(at)
        splitter.feed(bytes, length)
        write()
    }

    /** The output ended at [at]: what is left without a newline is a line too. */
    fun end(at: Long = System.currentTimeMillis()) {
        stamp = RotatingLog.stamp(at)
        splitter.flush()
        write()
    }

    private fun write() {
        if (out.size() == 0) return
        open()
        log.append(out.toByteArray())
        out.reset()
    }
}

/**
 * Cuts a process's output into lines for [onLine]: a line ends at a newline, and one longer
 * than [limit] bytes is handed on in pieces of that size. Bytes are UTF-8.
 */
class LineSplitter(private val limit: Int, private val onLine: (String) -> Unit) {
    private val pending = java.io.ByteArrayOutputStream()

    constructor(onLine: (String) -> Unit) : this(LIMIT, onLine)

    private companion object {
        const val LIMIT = 4096
    }

    fun feed(bytes: ByteArray, length: Int = bytes.size) {
        // Whole runs between newlines at a time: this runs for every byte the engine writes.
        var start = 0
        for (i in 0 until length) {
            if (bytes[i] == '\n'.code.toByte()) {
                take(bytes, start, i)
                emit()
                start = i + 1
            }
        }
        take(bytes, start, length)
    }

    /** Bytes [from] until [to] of [bytes] into the pending line, a piece handed on each time it reaches [limit]. */
    private fun take(bytes: ByteArray, from: Int, to: Int) {
        var at = from
        while (at < to) {
            val count = minOf(to - at, limit - pending.size())
            pending.write(bytes, at, count)
            at += count
            if (pending.size() >= limit) emit()
        }
    }

    /** Hands on what is left without a newline (the output ended). */
    fun flush() {
        if (pending.size() > 0) emit()
    }

    private fun emit() {
        onLine(pending.toString(Charsets.UTF_8.name()).trimEnd('\r'))
        pending.reset()
    }
}
