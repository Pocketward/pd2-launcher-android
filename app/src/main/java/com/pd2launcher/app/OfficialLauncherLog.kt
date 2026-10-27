package com.pd2launcher.app

import com.pd2launcher.app.control.Job
import com.pd2launcher.app.control.Operation
import com.pd2launcher.app.control.OperationStatus
import com.pd2launcher.app.control.ResultCard
import com.pd2launcher.app.control.Snapshot
import java.io.File
import java.util.Locale

/**
 * The official PD2 launcher's own log, `files/logs/official-launcher.log` ([NAME]), which Share
 * logs and Save logs pack with the app's other logs: one section for each run, from a press of its
 * button ([begin]) to the end of the engine's session ([end]), so that the logs say why the
 * launcher did not open. A run has:
 * - what the press found: the X screen, the launcher's files ([LauncherFiles]: PD2Launcher.exe's
 *   size, time and version, its own logs) and the x86 .NET runtime, from the session service, and
 *   the engine's last operation and result of the launcher from before the press;
 * - the engine's answer ([snapshot], [refused], [noSession]): its session's start, each change of
 *   its operation (phase, status, failure code), its end and its result's words;
 * - the processes that start, change their name or end while the session runs ([processes]), by
 *   their name: the launcher (`PD2Launcher.exe`), its updater (`UpdateUtility.exe`, as
 *   `UpdateUtility.e`: Linux keeps 15 characters of a name), Wine's crash dialog (`winedbg.exe`)
 *   and the game (`Game.exe`), with how long each ran;
 * - whether the X screen shows a picture or one color only ([screen], [picture]): one color, the
 *   launcher's window is not drawn;
 * - the engine's output lines meanwhile ([engineLine]), with their time: the engine passes on each
 *   line the launcher's Wine and .NET write as `pd2-launcher: | …`, its error lines among them;
 * - at the end, how many times each process started, those of the first look included (an update
 *   of the launcher shows as UpdateUtility and a second PD2Launcher.exe), whether PD2Launcher.exe
 *   was seen at all, when the X screen first showed a picture, and what changed in the launcher's
 *   files ([LauncherFiles.changes]: a new PD2Launcher.exe, new lines of its logs, a new error
 *   report).
 *
 * Each line has the wall-clock time ([RotatingLog.line], through [write]) and the time since the
 * press ([now], milliseconds). A run has its first [ENGINE_HEAD] and last [ENGINE_TAIL] engine
 * lines (with Detailed logging, all of them as they come up to [DETAILED_HEAD_BYTES], so the run's
 * first lines do not rotate out of the log while the launcher's game runs long; then the last
 * [ENGINE_TAIL]), but never Wine's `+loaddll` lines, and at most [EVENT_LINES] process and screen
 * lines; the rest are only counted, and the engine's are all in `engine.log`. A process that runs a program from a descriptor (the engine
 * starts its Wine helpers so) has the descriptor's number as its name.
 * Nothing here holds a CD key: the engine's lines are those of `engine.log`, which it redacts. The
 * worker, the snapshot collection, the process watch and the engine's log drain all call it. The
 * drain ([engineLine]) takes only [lock], which no file is read under: the files a run's start and
 * end read are read under [filesLock] alone, so a slow read never holds up the engine's output.
 */
class OfficialLauncherLog(private val write: (String) -> Unit, private val now: () -> Long) {
    /** The run's state and its lines. */
    private val lock = Any()
    /** Taken before [lock] by all but [engineLine]: a run's start and end, with their file reads, one at a time and in order. */
    private val filesLock = Any()
    private var run: Run? = null
    @Volatile private var active = false

    private class Run(
        val pressedAt: Long,
        val project: File?,
        val files: LauncherFiles.State,
        var operation: String?,
        val resultBefore: ResultCard?,
        /** Detailed logging: the engine lines written as they come up to [DETAILED_HEAD_BYTES], not [ENGINE_HEAD]. */
        val detailed: Boolean,
    ) {
        var seq: Long? = null
        var sentAt: Long? = null
        var sessionSeen = false
        /** When the session ended while its operation went on (the client check after the launcher). */
        var sessionEndedAt: Long? = null
        /** Engine lines written as they came ([ENGINE_HEAD] at most, or [DETAILED_HEAD_BYTES] of them). */
        var engineLines = 0
        var engineBytes = 0L
        /** The last [ENGINE_TAIL] engine lines after those, each with its time since the press, written at the end. */
        val engineTail = ArrayDeque<String>()
        /** Engine lines neither written nor in [engineTail] any more. */
        var engineDropped = 0
        /** Wine's `+loaddll` trace lines (a development engine's), left out. */
        var engineTraces = 0
        var eventLines = 0
        var eventsDropped = 0
        /** The processes seen at the last look, by pid: name and when first seen; null before the first look. */
        var processes: Map<Int, Pair<String, Long>>? = null
        val starts = LinkedHashMap<String, Int>()
        var picture: Boolean? = null
        var firstPicture: Long? = null
    }

    /** What a run's end still has to read and write after [lock]: the changes of the launcher's files. */
    private class Ending(val project: File?, val files: LauncherFiles.State)

    /** Whether a run is open: a press whose end has not come yet. */
    val running: Boolean get() = active

    /**
     * A press of the official PD2 launcher's button: a new run, whose first lines are [facts]. The
     * launcher's files in [project] are read now ([LauncherFiles.read]), so its end can say what
     * changed. [operation] and [result]: the engine's at the press; an operation of the official
     * launcher's job is from an earlier run, and is logged as that, so a run whose own operation
     * reads the same still shows it. A run still open ends first: its end never came.
     */
    fun begin(facts: List<String>, project: File?, operation: Operation?, result: ResultCard?, detailed: Boolean = false) {
        val files = LauncherFiles.read(project)
        synchronized(filesLock) {
            close(synchronized(lock) { run?.let { finish(it, "a new press came before this run's end") } })
            synchronized(lock) {
                write("=== official PD2 launcher: the button was pressed ===")
                val before = describe(operation?.takeIf { it.job == Job.OFFICIAL_LAUNCHER })
                run = Run(now(), project, files, before, result, detailed)
                active = true
                facts.forEach { write(it) }
                LauncherFiles.describe(files).forEach { write(it) }
                if (before != null) {
                    write("the engine's last operation of the official launcher, from before the press: $before")
                    if (result != null) write("its result then: ${result.title}: ${result.message}")
                }
            }
        }
    }

    /**
     * A press that was not taken, while no run is open ([reason]): one short section, without
     * reading the launcher's files.
     */
    fun notTaken(reason: String) {
        synchronized(filesLock) {
            synchronized(lock) {
                if (run != null) return
                write("=== official PD2 launcher: the button was pressed ===")
                write("not opened: $reason")
                write("=== end of the run ===")
            }
        }
    }

    /** One more line for the open run, with the time since the press; nothing without one. */
    fun note(text: String) {
        synchronized(lock) {
            val current = run ?: return
            write("${elapsed(current)} $text")
        }
    }

    /**
     * The command went to the engine as [seq]. With no session of it seen [NO_SESSION_MS] later,
     * [noSession] ends the run (the session service calls it then).
     */
    fun sent(seq: Long) {
        synchronized(lock) {
            val current = run ?: return
            current.seq = seq
            current.sentAt = now()
            write("${elapsed(current)} sent to the engine (command $seq)")
        }
    }

    /** The engine did not take command [seq] ([reason]): the run ends when it is this run's. */
    fun refused(seq: Long, reason: String) {
        synchronized(filesLock) {
            close(
                synchronized(lock) {
                    val current = run?.takeIf { it.seq == seq } ?: return
                    finish(current, "the engine refused it: $reason")
                },
            )
        }
    }

    /**
     * The engine's [next] snapshot: its session's start (the official launcher's job runs), each
     * new word on its operation, and the end: the job no longer runs; or, with no session seen, a
     * new operation of the job that has ended (the session started and ended between two
     * snapshots). At the end of a session that ran, [processes] gives the last look at the
     * processes.
     */
    fun snapshot(next: Snapshot, processes: () -> List<UidProcess>) {
        if (!active) return
        synchronized(filesLock) {
            close(
                synchronized(lock) {
                    val current = run ?: return
                    val session = next.activeJob == Job.OFFICIAL_LAUNCHER
                    val operation = next.operation?.takeIf { it.job == Job.OFFICIAL_LAUNCHER }
                    val words = describe(operation)
                    if (session && !current.sessionSeen) {
                        current.sessionSeen = true
                        write("${elapsed(current)} the engine's session runs")
                    }
                    val changed = words != null && words != current.operation
                    if (changed) {
                        current.operation = words
                        write("${elapsed(current)} engine: $words")
                    }
                    val ended = operation != null && operation.status in ENDED
                    val reason = when {
                        current.sessionSeen && !session -> sessionEnd(current, ended || operation == null, processes)
                        current.sessionSeen || current.seq == null -> null
                        changed && ended -> "the engine's session started and ended between two of its snapshots"
                        else -> null
                    } ?: return
                    finish(current, "$reason (${current.operation ?: "no operation"})${result(current, next.result, ended)}")
                },
            )
        }
    }

    /**
     * [NO_SESSION_MS] after [sent]: a run whose command [seq] has shown no session yet ends, with
     * the engine's operation and result in [last].
     */
    fun noSession(seq: Long, last: Snapshot?) {
        if (!active) return
        synchronized(filesLock) {
            close(
                synchronized(lock) {
                    val current = run?.takeIf { it.seq == seq && !it.sessionSeen } ?: return
                    val sentAt = current.sentAt ?: return
                    if (now() - sentAt < NO_SESSION_MS) return
                    val words = describe(last?.operation?.takeIf { it.job == Job.OFFICIAL_LAUNCHER })
                    val operation = if (words != null && words == current.operation) {
                        "the operation still reads $words, as before the press: the session may have started and ended between two snapshots"
                    } else {
                        "the operation reads ${words ?: "none"}"
                    }
                    finish(current, "no session of the engine seen within ${NO_SESSION_MS / 1000} s; $operation${result(current, last?.result)}")
                },
            )
        }
    }

    /**
     * [OPERATION_END_MS] after the session's end: a run that still waits for its operation's end
     * ends now, also when the engine sends no other snapshot (the session service calls it then).
     */
    fun settle() {
        if (!active) return
        synchronized(filesLock) {
            close(
                synchronized(lock) {
                    val current = run ?: return
                    val since = current.sessionEndedAt ?: return
                    if (now() - since < OPERATION_END_MS) return
                    finish(current, "the engine's session ended; its operation still read ${current.operation ?: "nothing"} ${OPERATION_END_MS / 1000} s later")
                },
            )
        }
    }

    /**
     * The app's other processes now ([list], without those the session service started itself):
     * the first look lists them, each later one the processes that started, changed their name or
     * ended since.
     */
    fun processes(list: List<UidProcess>) {
        if (!active) return
        synchronized(lock) {
            look(run ?: return, list)
        }
    }

    /** Whether the X screen shows a picture ([picture]) or one color only; a line when that changed. */
    fun screen(picture: Boolean) {
        if (!active) return
        synchronized(lock) {
            val current = run ?: return
            if (current.picture == picture) return
            current.picture = picture
            if (picture && current.firstPicture == null) current.firstPicture = now() - current.pressedAt
            event(current, if (picture) "the X screen shows a picture" else "the X screen shows one color only (nothing drawn)")
        }
    }

    /**
     * One line of the engine's output, while a run is open: the first [ENGINE_HEAD] are written at
     * once (with Detailed logging, those of the first [DETAILED_HEAD_BYTES]), the last [ENGINE_TAIL]
     * after them at the run's end (where a crash's lines are), the rest only counted; Wine's
     * `+loaddll` trace lines (a development engine's) are left out.
     * Takes [lock] only.
     */
    fun engineLine(line: String) {
        if (!active) return
        synchronized(lock) {
            val current = run ?: return
            if (line.contains(LOADDLL_TRACE)) {
                current.engineTraces++
                return
            }
            val text = line.trimEnd()
            val limit = if (current.detailed) DETAILED_LINE_LIMIT else LINE_LIMIT
            val entry = "${elapsed(current)} [engine] ${if (text.length > limit) text.take(limit) + "…" else text}"
            val head = if (current.detailed) current.engineBytes < DETAILED_HEAD_BYTES else current.engineLines < ENGINE_HEAD
            if (head) {
                current.engineLines++
                // As the log has it: UTF-8, with the wall-clock stamp [RotatingLog.line] puts first.
                current.engineBytes += entry.encodeToByteArray().size + STAMP_BYTES
                write(entry)
                return
            }
            if (current.engineTail.size == ENGINE_TAIL) {
                current.engineTail.removeFirst()
                current.engineDropped++
            }
            current.engineTail.addLast(entry)
        }
    }

    /**
     * Share logs or Save logs while a run is open (a launcher window that never shows, say): the
     * engine lines kept for the end so far are written now, so the zip has them; the run goes on.
     */
    fun checkpoint() {
        if (!active) return
        synchronized(lock) {
            val current = run ?: return
            if (current.engineTail.isEmpty()) return
            write("${elapsed(current)} the run goes on; its last ${current.engineTail.size} engine lines so far (each with its own time since the press):")
            current.engineTail.forEach { write(it) }
            current.engineTail.clear()
        }
    }

    /** The run ends for [reason] (the engine stopped, the app's session ended, the press was not sent). */
    fun end(reason: String) {
        if (!active) return
        synchronized(filesLock) {
            close(synchronized(lock) { finish(run ?: return, reason) })
        }
    }

    /**
     * The session of [current] no longer runs: the last look at the processes, at once. The run
     * ends now when the job's operation has [ended] (or the snapshot has none); else it waits for
     * the operation's end (the engine checks the game's files after the launcher), at most
     * [OPERATION_END_MS], at the snapshots that follow. The reason for the end, or null.
     */
    private fun sessionEnd(current: Run, ended: Boolean, processes: () -> List<UidProcess>): String? {
        val since = current.sessionEndedAt
        if (since == null) {
            current.sessionEndedAt = now()
            look(current, processes())
            if (ended) return "the engine's session ended"
            write("${elapsed(current)} the engine's session ended; its operation goes on")
            return null
        }
        return if (ended || now() - since >= OPERATION_END_MS) "the engine's session ended" else null
    }

    /**
     * The engine's result's words for the end of [current]: always when its operation [ended] in
     * this run, else only when they are not those from before the press.
     */
    private fun result(current: Run, result: ResultCard?, ended: Boolean = false): String =
        result?.takeIf { ended || it != current.resultBefore }?.let { "; its result: ${it.title}: ${it.message}" } ?: ""

    private fun look(current: Run, list: List<UidProcess>) {
        val at = now()
        val before = current.processes
        if (before == null) {
            current.processes = list.associate { it.pid to (it.name to at) }
            list.forEach { count(current, it.name) }
            event(current, "processes at the first look: ${list.joinToString { "${it.pid} ${it.name}" }.ifEmpty { "none" }}")
            return
        }
        val seen = list.associate { it.pid to it.name }
        val next = HashMap<Int, Pair<String, Long>>()
        for ((pid, entry) in before) {
            val (name, since) = entry
            val now = seen[pid]
            when {
                now == null -> event(current, "ended: $pid $name, after ${seconds(at - since)}")
                now == name -> next[pid] = entry
                else -> {
                    // A program that runs another (Wine names a process after the Windows program it
                    // starts); a pid used again within a look reads the same.
                    next[pid] = now to since
                    count(current, now)
                    event(current, "renamed: $pid $name -> $now")
                }
            }
        }
        for (process in list) {
            if (next.containsKey(process.pid)) continue
            next[process.pid] = process.name to at
            count(current, process.name)
            event(current, "started: ${process.pid} ${process.name}")
        }
        current.processes = next
    }

    private fun count(current: Run, name: String) {
        current.starts[name] = (current.starts[name] ?: 0) + 1
    }

    private fun event(current: Run, text: String) {
        if (current.eventLines >= EVENT_LINES) {
            current.eventsDropped++
            return
        }
        current.eventLines++
        write("${elapsed(current)} $text")
    }

    /** Under [lock]: the end's own lines; the run is closed. [close] then writes the file changes. */
    private fun finish(current: Run, reason: String): Ending {
        write("${elapsed(current)} the run ended: $reason")
        if (current.processes != null) {
            val starts = current.starts.entries.joinToString { (name, count) -> "$name $count" }
            write("processes seen in the session, with how many times each started: ${starts.ifEmpty { "none" }}")
            if (current.starts.keys.none { it.equals(LauncherFiles.LAUNCHER, ignoreCase = true) }) {
                write("${LauncherFiles.LAUNCHER} was not seen running (a look each second)")
            }
        }
        if (current.picture != null) {
            write(current.firstPicture?.let { "the X screen first showed a picture at ${seconds(it)}" } ?: "the X screen never showed a picture")
        }
        if (current.engineTail.isNotEmpty()) {
            if (current.engineDropped > 0) write("${current.engineDropped} more engine lines are only in engine.log")
            write("the last ${current.engineTail.size} engine lines (each with its own time since the press):")
            current.engineTail.forEach { write(it) }
        }
        if (current.engineTraces > 0) write("${current.engineTraces} Wine loaddll trace lines are only in engine.log")
        if (current.eventsDropped > 0) write("${current.eventsDropped} more process and screen lines were left out")
        run = null
        active = false
        return Ending(current.project, current.files)
    }

    /** Under [filesLock], after [lock]: what changed in the launcher's files, and the run's last line. */
    private fun close(ending: Ending?) {
        if (ending == null) return
        LauncherFiles.changes(ending.project, ending.files, LauncherFiles.read(ending.project)).forEach { write(it) }
        write("=== end of the run ===")
    }

    private fun elapsed(current: Run): String = "+${seconds(now() - current.pressedAt)}"

    companion object {
        /** The log's name in `files/logs`. */
        const val NAME = "official-launcher"

        /**
         * The log's size before it rotates, and the files kept: at most about 1 MB in all, room
         * for at least three runs that reach their limits.
         */
        const val LIMIT = 512L * 1024
        const val KEEP = 2

        /** The engine lines a run writes as they come, and the last ones it writes at its end. */
        const val ENGINE_HEAD = 200
        const val ENGINE_TAIL = 200

        /**
         * With Detailed logging, the bytes of engine lines a run writes as they come (with their
         * stamps, [STAMP_BYTES]): at most half of [LIMIT], so a run's start and these lines, and
         * the last [ENGINE_TAIL] after them, rotate at most once, into the file kept.
         */
        const val DETAILED_HEAD_BYTES = LIMIT / 2

        /** What [RotatingLog.line] puts before each line: `2026-10-25 21:27:52.668+09:00 `, and its line end. */
        const val STAMP_BYTES = 32

        /** Wine's `+loaddll` debug channel, which a development engine turns on: one line for each DLL loaded. */
        const val LOADDLL_TRACE = ":trace:loaddll:"

        /** How long a run waits after its session's end for the engine's operation to end ([snapshot]). */
        const val OPERATION_END_MS = 30_000L

        /** The process and screen lines a run keeps. */
        const val EVENT_LINES = 200

        /** With no session seen this long after the command was sent, the run ends ([noSession]). */
        const val NO_SESSION_MS = 60_000L

        /** The longest engine line kept; a longer one is cut. */
        const val LINE_LIMIT = 500

        /** The same with Detailed logging, where the engine passes on lines up to 1000 characters. */
        const val DETAILED_LINE_LIMIT = 1100

        /** The sample grid of [picture]: this many columns and rows. */
        const val COLUMNS = 32
        const val ROWS = 18

        private val ENDED = setOf(OperationStatus.SUCCEEDED, OperationStatus.FAILED, OperationStatus.INDETERMINATE, OperationStatus.CANCELLED)

        /** [operation]'s phase and status, and its failure code when it has one; null without one. */
        fun describe(operation: Operation?): String? = operation?.let {
            "${it.phase.name.lowercase()} ${it.status.name.lowercase()}${it.failure?.let { failure -> ", failure $failure" } ?: ""}"
        }

        /**
         * Whether a [width] x [height] X screen shows a picture: two or more points of a [COLUMNS] x
         * [ROWS] grid over it ([pixel], 0xXXRRGGBB) have a color other than the grid's most common
         * one. One point does not count: the X server draws its pointer into the screen. One color
         * is the empty X screen (black, or what was last drawn and cleared); a window of the
         * launcher, or a dialog, covers two points or more.
         */
        fun picture(width: Int, height: Int, pixel: (Int, Int) -> Int): Boolean {
            if (width <= 0 || height <= 0) return false
            val colors = HashMap<Int, Int>()
            for (row in 0 until ROWS) {
                val y = ((row * 2 + 1) * height / (ROWS * 2)).coerceIn(0, height - 1)
                for (column in 0 until COLUMNS) {
                    val x = ((column * 2 + 1) * width / (COLUMNS * 2)).coerceIn(0, width - 1)
                    val color = pixel(x, y) and 0xFFFFFF
                    colors[color] = (colors[color] ?: 0) + 1
                }
            }
            return COLUMNS * ROWS - (colors.values.maxOrNull() ?: 0) >= 2
        }

        private fun seconds(ms: Long): String = String.format(Locale.ROOT, "%.1f s", ms / 1000.0)
    }
}
