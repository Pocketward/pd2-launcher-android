package com.pd2launcher.app

import java.io.File
import java.io.IOException
import java.io.InputStream
import java.nio.channels.Channels
import java.nio.channels.FileChannel
import java.nio.file.Files
import java.nio.file.LinkOption
import java.nio.file.StandardOpenOption
import java.time.Instant

/**
 * The official PD2 launcher's files in the game folder's `ProjectD2` ([FOLDER]), for the logs
 * ([OfficialLauncherLog], [LogShare]): its programs ([LAUNCHER], [UPDATER]) with their size, time
 * and version, and the logs it writes beside them: [UPDATE_LOG] (UpdateUtility.exe, the launcher's
 * own update), [UPDATE_DEBUG_LOG] (each check for a launcher update) and an error report
 * `pd2launcher_error__<time>.txt` ([ERROR_REPORT]) for each error it did not handle. These hold no
 * CD key, password or account: the launcher has none. Their text is [redact]ed all the same (the
 * query, fragment and user part of a web address, a token or key after its name). Every file is
 * read without following a link, and only a regular file; Windows programs under Wine cannot make
 * the other kinds of file (a FIFO, whose open would wait) that a check before the open lets by.
 */
object LauncherFiles {
    const val FOLDER = "ProjectD2"
    const val LAUNCHER = "PD2Launcher.exe"
    const val UPDATER = "UpdateUtility.exe"
    const val UPDATE_LOG = "update.log"
    const val UPDATE_DEBUG_LOG = "launcher-update-debug.log"
    val ERROR_REPORT = Regex("""pd2launcher_error__[0-9_-]+\.txt""")

    /** The most of a program's start read for its version ([version]): .NET's program host keeps it near the start. */
    const val VERSION_SCAN = 1L shl 20

    /** The most of each file Share logs packs ([shareTexts]): its end. */
    const val SHARE_TAIL = 64 * 1024

    /** The newest error reports Share logs packs. */
    const val SHARE_REPORTS = 3

    /** The most of a log's new part the run's end shows ([changes]): its end. */
    const val NEW_TAIL = 8 * 1024

    /** The most lines of a log's new part the run's end shows. */
    const val NEW_LINES = 40

    /** The longest line taken from the launcher's files; a longer one is cut. */
    const val LINE_LIMIT = 500

    /** The new error reports a run's end summarizes ([changes]): the newest. */
    const val NEW_REPORTS = 5

    private val LOGS = listOf(UPDATE_LOG, UPDATE_DEBUG_LOG)

    /** A program: [size] in bytes, [modified] in milliseconds since the epoch, its [version] (null: not found). */
    data class Program(val size: Long, val modified: Long, val version: String?)

    /**
     * What is in the folder: [programs] by name (null: missing or not a regular file), the size of
     * each of the launcher's logs that is there ([logs]), and its error reports' names, oldest first.
     */
    data class State(val programs: Map<String, Program?>, val logs: Map<String, Long>, val errorReports: List<String>)

    /** The folder's [State]; all missing when [project] is null or not a real folder. */
    fun read(project: File?): State {
        val folder = project?.takeIf { realFolder(it) }
        val programs = listOf(LAUNCHER, UPDATER).associateWith { name -> folder?.let { program(File(it, name)) } }
        val logs = LOGS.mapNotNull { name -> folder?.let { File(it, name) }?.takeIf(::regular)?.let { name to it.length() } }.toMap()
        return State(programs, logs, folder?.let(::errorReports).orEmpty())
    }

    /** The error reports' names in [folder], oldest first (their names start with their time). */
    private fun errorReports(folder: File): List<String> =
        folder.listFiles()?.filter { ERROR_REPORT.matches(it.name) && regular(it) }?.map { it.name }?.sorted().orEmpty()

    /** [state] in the log's words, one line for each program and one for the launcher's own logs. */
    fun describe(state: State): List<String> =
        state.programs.map { (name, program) -> "$name: ${describe(program)}" } + "the launcher's own logs: ${describeLogs(state)}"

    /** A program's size, time and version, or that it is missing. */
    fun describe(program: Program?): String = program?.let {
        "${it.size} bytes, modified ${Instant.ofEpochMilli(it.modified)}, version ${it.version ?: "not found"}"
    } ?: "missing"

    private fun describeLogs(state: State): String {
        val logs = state.logs.map { (name, size) -> "$name $size bytes" }
        val reports = state.errorReports.takeIf { it.isNotEmpty() }?.let {
            listOf("${it.size} error report${if (it.size == 1) "" else "s"} (newest ${it.last()})")
        }.orEmpty()
        return (logs + reports).joinToString(", ").ifEmpty { "none" }
    }

    /**
     * What changed in [project] from [before] to [after], in the log's words: a program that
     * changed (the launcher's update writes a new one), the new lines of its logs (their end: at
     * most [NEW_TAIL] bytes and [NEW_LINES] lines, [redact]ed) and each new error report's summary
     * ([errorSummary]). Empty when nothing changed.
     */
    fun changes(project: File?, before: State, after: State): List<String> {
        val folder = project?.takeIf { realFolder(it) }
        val lines = mutableListOf<String>()
        for ((name, now) in after.programs) {
            val was = before.programs[name]
            if (was != now) lines += "$name changed in the run: ${describe(was)} -> ${describe(now)}"
        }
        if (folder != null) {
            for ((name, size) in after.logs) {
                val from = before.logs[name] ?: 0L
                if (size == from) continue
                // A log written anew (smaller than before) is new from its start.
                val added = tail(File(folder, name), if (size > from) from else 0L, NEW_TAIL)?.let(::textLines).orEmpty()
                if (added.isEmpty()) continue
                lines += "$name, new lines:"
                lines += added.takeLast(NEW_LINES).map { "  $it" }
            }
            val reports = after.errorReports - before.errorReports.toSet()
            if (reports.size > NEW_REPORTS) lines += "${reports.size - NEW_REPORTS} more new error reports, older than these"
            for (report in reports.takeLast(NEW_REPORTS)) {
                val summary = tail(File(folder, report), 0L, SHARE_TAIL)?.let(::errorSummary)
                lines += "new error report $report: ${summary ?: "no exception named in it"}"
            }
        }
        return lines
    }

    /**
     * An error report's summary from its labelled lines: "<class>.<method>: <message>", and the
     * inner exception's message after "inner:"; null when it names no exception. At most
     * [LINE_LIMIT] characters, [redact]ed.
     */
    fun errorSummary(text: String): String? {
        val lines = text.lines().map { it.trim() }
        fun after(label: String): String? = lines.indexOf(label).takeIf { it >= 0 }?.let { lines.getOrNull(it + 1) }?.takeIf { it.isNotEmpty() }
        val message = after("Exception Message:")
        val type = after("Exception Class:")
        val method = after("Exception Method:")
        if (message == null && type == null) return null
        val where = listOfNotNull(type, method).joinToString(".").ifEmpty { "an exception" }
        val inner = after("Inner Exception Message:")?.takeIf { it != message }?.let { "; inner: $it" } ?: ""
        return line("$where: ${message ?: "no message"}$inner")
    }

    /**
     * [text] for the logs: [redact]ed and cut at [LINE_LIMIT] characters (cut once before the
     * redaction too, so a long line costs little).
     */
    fun line(text: String): String {
        val redacted = redact(if (text.length > LINE_LIMIT * 4) text.take(LINE_LIMIT * 4) else text)
        return if (redacted.length > LINE_LIMIT) redacted.take(LINE_LIMIT) + "…" else redacted
    }

    /**
     * [text] with each web address's user part (`…@`), query (`?…`) and fragment (`#…`) left out
     * but for their marks, the value after a name such as `token`, `password`, `api_key` or
     * `Authorization` followed by `=` or `:` left out (`…`), and the user of a Windows profile
     * folder (`C:\Users\<name>`; a folder copied from a PC names the PC's user) as `<user>`, but
     * for the folders that name no one ([SHARED_USERS]). Each pattern takes a bounded number of
     * characters at each place, so a long line costs time in proportion to its length.
     */
    fun redact(text: String): String {
        val noUser = URL_USER.replace(text) { "${it.groupValues[1]}…@" }
        val noQuery = URL_QUERY.replace(noUser) { "${it.groupValues[1]}${it.groupValues[2]}…" }
        val noSecret = SECRET.replace(noQuery) { "${it.groupValues[1]}${it.groupValues[2]}…" }
        return WINDOWS_USER.replace(noSecret) { match ->
            val (folder, name, separator) = match.destructured
            // A name runs to a character no Windows user name holds (`" / \ [ ] : ; | = , + * ? < >`),
            // a quote (an apostrophe between letters, as in O'Brien, is part of it) or the line's
            // end, not to a space: a Windows user name can hold spaces. So a line cut inside a name
            // hides the part that is left too.
            if (name.lowercase() in SHARED_USERS) match.value else "$folder<user>$separator"
        }
    }

    private val WINDOWS_USER = Regex("""(?i)\b([a-z]:[\\/]{1,2}users[\\/]{1,2})((?:[^\\/"'<>|\[\]:;=,+*?\r\n]|(?<=\p{L})'(?=\p{L})){1,64}+)([\\/]?)""")
    private val SHARED_USERS = setOf("public", "default", "default user", "all users")

    private val URL_USER = Regex("""(?i)(\b[a-z][a-z0-9+.-]{0,31}://)[^\s/?#@"'<>]{1,256}+@""")
    private val URL_QUERY = Regex("""(?i)(\b[a-z][a-z0-9+.-]{0,31}://[^\s?#"'<>]{1,2048}+)([?#])[^\s"'<>]*+""")
    private val SECRET = Regex(
        """(?i)\b([a-z_.-]{0,32}(?:authorization|token|password|passwd|secret|api[_-]?key))(\s{0,4}[:=]\s{0,4})(?:(?:bearer|basic)\s+)?[^\s,;"'<>]++""",
    )

    /**
     * The launcher's own logs for Share logs, as `launcher/<name>` texts: [UPDATE_LOG],
     * [UPDATE_DEBUG_LOG] and the [SHARE_REPORTS] newest error reports, each its last [SHARE_TAIL]
     * bytes, line by line [redact]ed. Empty without the folder.
     */
    fun shareTexts(project: File?): List<Pair<String, String>> {
        val folder = project?.takeIf { realFolder(it) } ?: return emptyList()
        val reports = errorReports(folder).takeLast(SHARE_REPORTS)
        return (LOGS + reports).mapNotNull { name ->
            tail(File(folder, name), null, SHARE_TAIL)?.let { text -> "launcher/$name" to text.lines().joinToString("\n") { redact(it) } }
        }
    }

    /**
     * The x86 .NET runtime the launcher runs on: the app's runtime folder ([AppRuntime.DOTNET] in
     * [managedRoot]) with its frameworks and host versions, and whether the prefix's
     * `C:\Program Files (x86)\dotnet` leads to it. One line, without the data folder's path.
     */
    fun dotnet(managedRoot: File): String {
        val runtime = File(managedRoot, AppRuntime.DOTNET)
        val found = if (!realFolder(runtime)) {
            "${AppRuntime.DOTNET} is missing"
        } else {
            val frameworks = File(runtime, "shared").listFiles().orEmpty().filter { it.isDirectory }.sortedBy { it.name }
                .map { framework -> "${framework.name} ${versions(framework)}" }
            val host = "hostfxr ${versions(File(runtime, "host/fxr"))}"
            "${AppRuntime.DOTNET}: ${(frameworks + host).joinToString(", ")}"
        }
        val link = File(managedRoot, PREFIX_DOTNET).toPath()
        val linked = try {
            when {
                !Files.exists(link, LinkOption.NOFOLLOW_LINKS) -> "is missing"
                !Files.isSymbolicLink(link) -> "is not a link"
                !Files.isDirectory(link) -> "leads nowhere (${target(managedRoot, link)})"
                Files.isSameFile(link, runtime.toPath()) -> "leads to it"
                else -> "leads elsewhere (${target(managedRoot, link)})"
            }
        } catch (error: IOException) {
            "cannot be read (${error.javaClass.simpleName})"
        }
        return "$found; C:\\Program Files (x86)\\dotnet $linked"
    }

    /** Where [link] leads: as written when relative, else relative to [managedRoot], else "a path outside the installation". */
    private fun target(managedRoot: File, link: java.nio.file.Path): String {
        val target = Files.readSymbolicLink(link)
        return when {
            !target.isAbsolute -> target.toString()
            target.startsWith(managedRoot.toPath()) -> "the installation's ${managedRoot.toPath().relativize(target)}"
            else -> "a path outside the installation"
        }
    }

    /** The prefix's link to the .NET runtime, in the managed root (runtime/prefix.sh). */
    private const val PREFIX_DOTNET = "prefix/drive_c/Program Files (x86)/dotnet"

    private fun versions(folder: File): String =
        folder.listFiles().orEmpty().filter { it.isDirectory }.map { it.name }.sorted().joinToString("/").ifEmpty { "none" }

    /**
     * The program's version from its version resource: the UTF-16 `ProductVersion` value (for the
     * launcher, its version and the commit it was built from), else `FileVersion`, in its first
     * [VERSION_SCAN] bytes; null when neither is there.
     */
    fun version(file: File): String? {
        val bytes = head(file, VERSION_SCAN) ?: return null
        return versionValue(bytes, "ProductVersion") ?: versionValue(bytes, "FileVersion")
    }

    /**
     * The value after the version resource's key [key] in [bytes]: the key in UTF-16LE with its
     * end, the zero that aligns the value (when there is one), then the value up to its end;
     * printable ASCII only, at most 100 characters. Null when the key is not there or its value is
     * empty (then the next copy of the key is looked at).
     */
    fun versionValue(bytes: ByteArray, key: String): String? {
        val needle = (key + "\u0000").toByteArray(Charsets.UTF_16LE)
        var at = indexOf(bytes, needle)
        while (at >= 0) {
            var index = at + needle.size
            // The alignment to 32 bits: one UTF-16 zero at most; a zero after it is an empty value's end.
            if (index + 1 < bytes.size && bytes[index] == 0.toByte() && bytes[index + 1] == 0.toByte()) index += 2
            val value = StringBuilder()
            while (index + 1 < bytes.size && value.length < 100) {
                val char = (bytes[index].toInt() and 0xff) or ((bytes[index + 1].toInt() and 0xff) shl 8)
                if (char == 0 || char !in 0x20..0x7e) break
                value.append(char.toChar())
                index += 2
            }
            if (value.isNotEmpty()) return value.toString()
            at = indexOf(bytes, needle, at + 1)
        }
        return null
    }

    private fun indexOf(bytes: ByteArray, needle: ByteArray, from: Int = 0): Int {
        outer@ for (start in from..bytes.size - needle.size) {
            for (offset in needle.indices) if (bytes[start + offset] != needle[offset]) continue@outer
            return start
        }
        return -1
    }

    /** PD2Launcher.exe in [project] ([LAUNCHER]), read alone; null as in [read]. */
    fun launcher(project: File?): Program? = project?.takeIf { realFolder(it) }?.let { program(File(it, LAUNCHER)) }

    /** [file]'s size, time and version; null when it is missing or not a regular file. */
    private fun program(file: File): Program? {
        if (!regular(file)) return null
        return Program(file.length(), file.lastModified(), version(file))
    }

    /** The lines of [text], each [line]d, without empty ones. */
    private fun textLines(text: String): List<String> = text.lines().map { it.trimEnd('\r') }.filter { it.isNotBlank() }.map(::line)

    /** At most the first [limit] bytes of [file]; null when it is not a regular file or cannot be read. */
    private fun head(file: File, limit: Long): ByteArray? = open(file)?.use { channel ->
        try {
            val buffer = java.nio.ByteBuffer.allocate(minOf(channel.size(), limit).toInt())
            while (buffer.hasRemaining()) {
                if (channel.read(buffer) < 0) break
            }
            buffer.array().copyOf(buffer.position())
        } catch (_: IOException) {
            null
        }
    }

    /**
     * [file]'s text from byte [from] (null: the end's last [limit] bytes) to its end, at most its
     * last [limit] bytes of that; null when it is not a regular file or cannot be read. Bytes that
     * are not UTF-8 read as replacement characters.
     */
    private fun tail(file: File, from: Long?, limit: Int): String? = open(file)?.use { channel ->
        try {
            val size = channel.size()
            val start = maxOf(from ?: 0L, size - limit, 0L)
            channel.position(start)
            Channels.newInputStream(channel).readAtMost(limit).decodeToString()
        } catch (_: IOException) {
            null
        }
    }

    private fun InputStream.readAtMost(limit: Int): ByteArray {
        val out = java.io.ByteArrayOutputStream()
        val buffer = ByteArray(8192)
        while (out.size() < limit) {
            val n = read(buffer, 0, minOf(buffer.size, limit - out.size()))
            if (n < 0) break
            out.write(buffer, 0, n)
        }
        return out.toByteArray()
    }

    private fun open(file: File): FileChannel? = try {
        if (regular(file)) FileChannel.open(file.toPath(), StandardOpenOption.READ, LinkOption.NOFOLLOW_LINKS) else null
    } catch (_: IOException) {
        null
    }

    private fun regular(file: File): Boolean = Files.isRegularFile(file.toPath(), LinkOption.NOFOLLOW_LINKS)

    private fun realFolder(folder: File): Boolean = Files.isDirectory(folder.toPath(), LinkOption.NOFOLLOW_LINKS)
}
