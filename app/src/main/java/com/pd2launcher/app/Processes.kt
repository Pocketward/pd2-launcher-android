package com.pd2launcher.app

import android.os.ParcelFileDescriptor
import android.system.ErrnoException
import android.system.Os
import android.util.Log
import java.io.File
import java.io.FileInputStream
import java.io.IOException
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit

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
 * into the process's own [RotatingLog] in [logs], and
 * every start and exit is logged with pid, name and status or signal, both in that log and
 * in [serviceLog].
 */
class ProcessStarter(private val logs: File, private val serviceLog: RotatingLog) {
    private val lock = Any()

    /**
     * Starts [path] with [argv] and [env] as [name]; [stdin] (null: /dev/null) and [fd3] (the
     * child's descriptor 3, when given) stay owned by the caller. Each line of its output also
     * goes to [mirror], when there is one. [onExit] runs on the reaper thread after the exit is logged.
     */
    fun start(
        name: String,
        path: String,
        argv: List<String>,
        env: List<String>,
        stdin: ParcelFileDescriptor? = null,
        fd3: ParcelFileDescriptor? = null,
        mirror: ((String) -> Unit)? = null,
        onExit: (OwnedProcess, ExitStatus) -> Unit = { _, _ -> },
    ): OwnedProcess {
        val log = RotatingLog(logs, name)
        val pipe = ParcelFileDescriptor.createPipe()
        val devNull = if (stdin == null) ParcelFileDescriptor.open(File("/dev/null"), ParcelFileDescriptor.MODE_READ_ONLY) else null
        val drained = CountDownLatch(1)
        val process: OwnedProcess
        try {
            // Held until the process is registered, so its exit cannot be handled first.
            synchronized(lock) {
                var started: OwnedProcess? = null
                val pid = Spawner.spawn(path, argv, env, stdin ?: devNull!!, pipe[1], fd3) { status ->
                    synchronized(lock) { OwnedProcesses.remove(started?.pid ?: -1) }
                    // Most output is in by now; a descendant holding the pipe must not delay the exit line.
                    drained.await(1, TimeUnit.SECONDS)
                    val line = "exit pid=${started?.pid} name=$name ${describe(status)}"
                    log.line(line)
                    log.close()
                    serviceLog.line(line)
                    Log.i(TAG, line)
                    started?.let { onExit(it, status) }
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
        val line = "start pid=${process.pid} name=$name: ${argv.joinToString(" ")}"
        log.line(line)
        serviceLog.line(line)
        Log.i(TAG, line)
        Thread({ drain(pipe[0], log, drained, mirror?.let(::LineSplitter)) }, "pd2-log-$name").apply { isDaemon = true }.start()
        return process
    }

    private fun drain(read: ParcelFileDescriptor, log: RotatingLog, drained: CountDownLatch, lines: LineSplitter?) {
        try {
            ParcelFileDescriptor.AutoCloseInputStream(read).use { input: FileInputStream ->
                val buffer = ByteArray(8192)
                while (true) {
                    val n = input.read(buffer)
                    if (n < 0) break
                    log.append(buffer, n)
                    lines?.feed(buffer, n)
                }
                lines?.flush()
            }
        } catch (error: IOException) {
            log.line("log pipe failed: ${error.message}")
        } finally {
            drained.countDown()
        }
    }

    companion object {
        private const val TAG = "pd2-processes"

        fun describe(status: ExitStatus): String =
            if (status.signal != null) "signal=${status.signal}${signalName(status.signal)}" else "status=${status.code}"

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
        for (i in 0 until length) {
            val byte = bytes[i]
            if (byte == '\n'.code.toByte()) {
                emit()
            } else {
                pending.write(byte.toInt())
                if (pending.size() >= limit) emit()
            }
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
