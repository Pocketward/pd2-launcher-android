package com.pd2launcher.app

import java.io.ByteArrayOutputStream
import java.io.File
import java.io.FileOutputStream
import java.io.IOException
import java.io.InputStream
import java.net.HttpURLConnection
import java.net.URL
import kotlinx.serialization.builtins.ListSerializer
import kotlinx.serialization.json.Json

/**
 * The app update's requests ([AppUpdater]): the check against the releases feed, small text files
 * (a `.sha256`) and the APK's download, resumed where it stopped. Plain HTTPS through
 * [HttpURLConnection], which follows GitHub's redirects to its file servers.
 */
object AppUpdateHttp {
    private const val TIMEOUT_MS = 20_000
    private const val HTTP_RANGE_NOT_SATISFIABLE = 416
    /** The most a check reads of the feed's answer: ten releases with long notes stay well below it. */
    const val FEED_LIMIT = 4 shl 20
    /** The download's progress is reported each time this much more came in. */
    private const val PROGRESS_STEP = 512L shl 10
    /**
     * About how long a small answer's body (the feed, a `.sha256`) may take from the request's
     * start. It is checked after each read of the body, so the answer ends at the first read after
     * this time. The connection and the headers have only [TIMEOUT_MS] for each read.
     */
    const val ANSWER_TIME_MS = 60_000L
    /** The download must get at least [STALL_BYTES] in each [STALL_TIME_MS], checked after each read. */
    const val STALL_TIME_MS = 60_000L
    const val STALL_BYTES = 64L shl 10

    /** How the app names itself to GitHub: its own name and version, not the phone's model or build. */
    val userAgent = "PD2LauncherAndroid/${BuildConfig.VERSION_NAME}"

    private val json = Json { ignoreUnknownKeys = true }

    /** The server answered [code]: not the release or the file asked for. */
    class Status(val code: Int, what: String) : IOException("HTTP $code for $what")

    /**
     * The server's file is not the size the release names: another file, or no file at all (a
     * captive portal's page). Nothing of it is written.
     */
    class OtherFile(message: String) : IOException(message)

    /** The feed's answer to a check. */
    sealed interface Answer {
        /** The releases (GitHub's list, newest first), and the ETag to send with the next check. */
        data class Found(val releases: List<Release>, val etag: String?) : Answer

        /** The ETag sent still names the same list (HTTP 304). */
        data object NotModified : Answer

        /** No published release (HTTP 404). */
        data object NoRelease : Answer

        /** GitHub refused the check: no request before [until] ([AppUpdate.retryAt]). */
        data class Wait(val until: Long) : Answer
    }

    /**
     * One request to the releases [feed] (GitHub's list of a repository's releases, API version
     * 2022-11-28) with the [etag] of the last answer. Any other answer, an answer that does not
     * read as a list of releases, and one whose body is still coming about [timeLimitMs] after the
     * request's start (checked after each read; [ANSWER_TIME_MS]), is an error.
     */
    fun ask(feed: String, etag: String?, now: Long, timeLimitMs: Long = ANSWER_TIME_MS): Answer {
        val deadline = System.nanoTime() + timeLimitMs * 1_000_000
        val connection = open(feed)
        connection.setRequestProperty("Accept", "application/vnd.github+json")
        connection.setRequestProperty("X-GitHub-Api-Version", "2022-11-28")
        if (etag != null) connection.setRequestProperty("If-None-Match", etag)
        try {
            val code = connection.responseCode
            AppUpdate.retryAt(code, connection::getHeaderField, now)?.let { return Answer.Wait(it) }
            return when (code) {
                HttpURLConnection.HTTP_OK -> Answer.Found(
                    json.decodeFromString(ListSerializer(Release.serializer()), connection.inputStream.use { read(it, FEED_LIMIT, deadline) }),
                    connection.getHeaderField("ETag"),
                )
                HttpURLConnection.HTTP_NOT_MODIFIED -> Answer.NotModified
                HttpURLConnection.HTTP_NOT_FOUND -> Answer.NoRelease
                else -> throw Status(code, "the release feed")
            }
        } finally {
            connection.disconnect()
        }
    }

    /**
     * A small text file (a `.sha256`): at most [limit] bytes, whose body must come within about
     * [timeLimitMs] of the request's start (checked after each read; [ANSWER_TIME_MS]).
     */
    fun text(url: String, limit: Int, timeLimitMs: Long = ANSWER_TIME_MS): String {
        val deadline = System.nanoTime() + timeLimitMs * 1_000_000
        val connection = open(url)
        try {
            val code = connection.responseCode
            if (code != HttpURLConnection.HTTP_OK) throw Status(code, url)
            return connection.inputStream.use { read(it, limit, deadline) }
        } finally {
            connection.disconnect()
        }
    }

    /**
     * The file at [url], of [size] bytes, into [part], after what [part] holds already: a Range
     * request. A server that sends the whole file instead starts it again; one that sends another
     * part deletes [part] (the next try starts again), as does one that has no such part (416). An
     * answer for a file of another size writes nothing ([OtherFile]), and another status neither
     * ([Status]). [answered] hears the server's answer (its status, the host that sent it after
     * the redirects, and the part it sends), for the log; [progress] the bytes in hand. Ends early,
     * keeping what came in, once [stopped]; a download that ends short is an error, as is one that
     * gets less than [STALL_BYTES] in [stallTimeMs] (a slow download goes on; one that trickles
     * does not).
     */
    fun fetch(
        url: String,
        size: Long,
        part: File,
        stopped: () -> Boolean,
        stallTimeMs: Long = STALL_TIME_MS,
        answered: (String) -> Unit = {},
        progress: (Long) -> Unit,
    ) {
        var have = part.length()
        if (have > size) {
            part.delete()
            have = 0
        }
        if (have == size) return
        val connection = open(url)
        // The file's own bytes, never compressed on the way, so the length is the file's.
        connection.setRequestProperty("Accept-Encoding", "identity")
        if (have > 0) connection.setRequestProperty("Range", "bytes=$have-")
        try {
            val code = connection.responseCode
            answered("HTTP $code from ${connection.url.host}${connection.getHeaderField("Content-Range")?.let { " ($it)" }.orEmpty()}")
            val append = when (code) {
                HttpURLConnection.HTTP_OK -> {
                    val length = connection.contentLengthLong
                    if (length != size) throw OtherFile("the server sends $length bytes, not the $size of $url")
                    false
                }
                HttpURLConnection.HTTP_PARTIAL -> {
                    val range = connection.getHeaderField("Content-Range")
                    if (AppUpdate.rangeTotal(range) != size) throw OtherFile("the server's file is not the $size bytes of $url ($range)")
                    if (AppUpdate.rangeStart(range) != have) {
                        part.delete()
                        throw IOException("the server sent another part of the file ($range)")
                    }
                    true
                }
                HTTP_RANGE_NOT_SATISFIABLE -> {
                    part.delete()
                    throw Status(code, url)
                }
                else -> throw Status(code, url)
            }
            var done = if (append) have else 0L
            var shown = done
            progress(done)
            // The current window of the stall rule: when it began, and the bytes in hand then.
            var windowStart = System.nanoTime()
            var windowDone = done
            connection.inputStream.use { input ->
                FileOutputStream(part, append).use { out ->
                    val buffer = ByteArray(64 shl 10)
                    while (!stopped() && done <= size) {
                        val n = input.read(buffer)
                        if (n < 0) break
                        out.write(buffer, 0, n)
                        done += n
                        if (done - shown >= PROGRESS_STEP) {
                            shown = done
                            progress(done)
                        }
                        val now = System.nanoTime()
                        if (now - windowStart >= stallTimeMs * 1_000_000) {
                            if (done - windowDone < STALL_BYTES) {
                                out.fd.sync()
                                throw IOException("the download got ${done - windowDone} bytes in ${stallTimeMs / 1000} s")
                            }
                            windowStart = now
                            windowDone = done
                        }
                    }
                    out.fd.sync()
                }
            }
            progress(done)
            if (!stopped() && done < size) throw IOException("the download ended at $done of $size bytes")
        } finally {
            connection.disconnect()
        }
    }

    private fun open(url: String): HttpURLConnection =
        (URL(url).openConnection() as HttpURLConnection).apply {
            connectTimeout = TIMEOUT_MS
            readTimeout = TIMEOUT_MS
            setRequestProperty("User-Agent", userAgent)
        }

    /**
     * All of [input] as UTF-8 text; longer than [limit] bytes, or still coming at [deadline]
     * ([System.nanoTime]), is an error.
     */
    private fun read(input: InputStream, limit: Int, deadline: Long): String {
        val out = ByteArrayOutputStream()
        val buffer = ByteArray(8192)
        while (true) {
            val n = input.read(buffer)
            if (n < 0) break
            out.write(buffer, 0, n)
            if (out.size() > limit) throw IOException("the answer is longer than $limit bytes")
            if (System.nanoTime() - deadline > 0) throw IOException("the answer took longer than its time limit")
        }
        return out.toString(Charsets.UTF_8.name())
    }
}
