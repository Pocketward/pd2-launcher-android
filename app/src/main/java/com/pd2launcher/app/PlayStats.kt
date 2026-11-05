package com.pd2launcher.app

import androidx.annotation.PluralsRes
import androidx.annotation.StringRes
import java.io.ByteArrayOutputStream
import java.io.IOException
import java.net.HttpURLConnection
import java.net.URL
import java.text.NumberFormat
import java.util.Locale
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.delay
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.runInterruptible
import kotlinx.coroutines.withTimeoutOrNull
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.longOrNull

/**
 * Play's stats line (owner, 2026-11-03): the PD2 server's status and the players in game, from
 * PD2's public API, as the official website reads them. No login, no key, no game traffic.
 * [REALM_URL] gives the status (`ok` online, `down` down, any other value slow); [META_URL] gives
 * `in_game.count`. Any error is [Realm.UNKNOWN], never [Realm.DOWN]: our own failure says nothing
 * of the server. The app asks only while Play shows ([Schedule]) and keeps the last answer in
 * memory for this run, never on disk.
 */
object PlayStats {
    const val REALM_URL = "https://api.projectdiablo2.com/game/realm-status"
    const val META_URL = "https://api.projectdiablo2.com/system/meta"

    /** Each request's connect and read time: the line is a small extra, so it gives up soon. */
    private const val TIMEOUT_MS = 5_000

    /** The most of an answer that is read: both are about 100 bytes. */
    private const val ANSWER_LIMIT = 16 shl 10

    /** The most time for both requests of one fetch ([fetchWithin]). */
    const val FETCH_LIMIT_MS = 10_000L

    /** How the app names itself to PD2's API: the app's one form, as the app update's. */
    val userAgent = AppUpdateHttp.userAgent

    private val json = Json { ignoreUnknownKeys = true }

    /** The PD2 server's status. */
    enum class Realm(val word: String) {
        ONLINE("ok"),
        SLOW("slow"),
        DOWN("down"),
        /** No answer, or one that does not read as a status: not known. */
        UNKNOWN("unknown"),
    }

    /** The status and the players in game ([players] null: not known). */
    data class Stats(val realm: Realm, val players: Long?) {
        /** The count that shows: only while the server is online or slow, and only when known. */
        val shownPlayers: Long? get() = players.takeIf { realm == Realm.ONLINE || realm == Realm.SLOW }

        companion object {
            val UNKNOWN = Stats(Realm.UNKNOWN, null)
        }
    }

    /** The server answered with [code], not 2xx. */
    class Status(val code: Int) : IOException("HTTP $code")

    /** One fetch: the [stats], and one reason for each request that failed (never an answer's body). */
    data class Result(val stats: Stats, val failures: List<String>)

    /** The status in realm-status's answer [body]. */
    fun realm(body: String): Realm {
        val status = (field(body, "status") as? JsonPrimitive)?.takeIf { it.isString }?.content ?: return Realm.UNKNOWN
        return when (status) {
            "ok" -> Realm.ONLINE
            "down" -> Realm.DOWN
            else -> Realm.SLOW
        }
    }

    /** `in_game.count` in meta's answer [body]: a whole number of 0 or more, else null. */
    fun players(body: String): Long? {
        val count = ((field(body, "in_game") as? JsonObject)?.get("count") as? JsonPrimitive)?.takeUnless { it.isString }
        return count?.longOrNull?.takeIf { it >= 0 }
    }

    private fun field(body: String, name: String): JsonElement? =
        (runCatching { json.parseToJsonElement(body) }.getOrNull() as? JsonObject)?.get(name)

    /**
     * Both requests through [get] (the answer's body; an error throws), each on the IO threads:
     * the stats, and why a request failed. A cancel (Play left, or [fetchWithin]'s limit)
     * interrupts the request under way and sends no other, also when the request ends after the
     * cancel (a DNS lookup does not stop for an interrupt). An interrupt without a cancel is a
     * failure of the request, as a request's own timeout is.
     */
    suspend fun fetch(get: (String) -> String): Result {
        val failures = mutableListOf<String>()
        suspend fun ask(name: String, url: String): String? = try {
            runInterruptible(Dispatchers.IO) { get(url) }
        } catch (error: Exception) {
            // Cancelled: the fetch ends here. An interrupt or a cancel error without a cancel of
            // the fetch is a failure of this request, as any other error, never a crash.
            currentCoroutineContext().ensureActive()
            failures += "$name: ${reason(error)}"
            null
        }
        val realm = ask("realm-status", REALM_URL)?.let { body ->
            realm(body).also { if (it == Realm.UNKNOWN) failures += "realm-status: no status in the answer" }
        } ?: Realm.UNKNOWN
        currentCoroutineContext().ensureActive()
        val players = ask("meta", META_URL)?.let { body ->
            players(body).also { if (it == null) failures += "meta: no in_game count in the answer" }
        }
        return Result(Stats(realm, players), failures)
    }

    /** A failure in a few words: the status, or the error's kind and message, never an answer's body. */
    private fun reason(error: Exception): String = when (error) {
        is Status -> "HTTP ${error.code}"
        // runInterruptible turns an interrupt into a cancel error, also without a cancel.
        is CancellationException, is InterruptedException -> "interrupted"
        else -> listOfNotNull(error.javaClass.simpleName, error.message).joinToString(": ")
    }

    /**
     * Both requests through [get] ([fetch]) on the IO threads, within [timeoutMs] in all; no answer
     * by then is unknown.
     */
    suspend fun fetchWithin(timeoutMs: Long, get: (String) -> String): Result =
        withTimeoutOrNull(timeoutMs) { fetch(get) }
            ?: Result(Stats.UNKNOWN, listOf("no answer in $timeoutMs ms"))

    /**
     * The refresh loop while Play is visible, until it is cancelled: the first fetch at once (a
     * visit) unless [schedule]'s last was less than 1 s ago, then one each 60 s ([Schedule.wait]).
     * [now] is the clock, [sleep] the wait; each fetch's [Result] goes to [onResult].
     */
    suspend fun run(
        schedule: Schedule,
        now: () -> Long,
        get: (String) -> String,
        onResult: suspend (Result) -> Unit,
        sleep: suspend (Long) -> Unit = { delay(it) },
        timeoutMs: Long = FETCH_LIMIT_MS,
    ) {
        var visit = true
        while (true) {
            sleep(schedule.wait(now(), visit))
            visit = false
            schedule.fetched(now())
            onResult(fetchWithin(timeoutMs, get))
        }
    }

    /**
     * What the line shows: the [last] answer of this app run; before the first, unknown while the
     * app may ask ([active]), else no line (the app has not asked yet, during "Getting ready…").
     */
    fun shown(last: Stats?, active: Boolean): Stats? = last ?: Stats.UNKNOWN.takeIf { active }

    /** The log line when the status changes; null when it stays (the count alone changes all the time). */
    fun changeLine(before: Stats?, after: Stats): String? {
        val was = (before ?: Stats.UNKNOWN).realm
        return if (was == after.realm) null else "play stats: realm ${was.word} -> ${after.realm.word}"
    }

    /** The count with the thousands separator of [locale]. */
    fun count(players: Long, locale: Locale): String = NumberFormat.getIntegerInstance(locale).format(players)

    /** The status's words. */
    fun words(realm: Realm): Int = when (realm) {
        Realm.ONLINE -> R.string.play_stats_online
        Realm.SLOW -> R.string.play_stats_slow
        Realm.DOWN -> R.string.play_stats_down
        Realm.UNKNOWN -> R.string.play_stats_unknown
    }

    /**
     * The line for TalkBack, as one sentence: the status's [words] alone, or with a count the
     * [plural] ("Server online, 3,613 players in game"; "1 player") for [quantity], with [count].
     */
    data class Said(@param:StringRes val words: Int, @param:PluralsRes val plural: Int? = null, val quantity: Int = 0, val count: String? = null)

    /** The line for TalkBack ([Said]). */
    fun spoken(stats: Stats, locale: Locale): Said {
        val players = stats.shownPlayers ?: return Said(words(stats.realm))
        return Said(
            words(stats.realm),
            if (stats.realm == Realm.SLOW) R.plurals.play_stats_said_slow else R.plurals.play_stats_said_online,
            players.coerceAtMost(Int.MAX_VALUE.toLong()).toInt(),
            count(players, locale),
        )
    }

    /**
     * When to fetch (owner, 2026-11-03): "every time the user visits the play page. no more than once
     * a second", and every 60 s while Play stays visible. Kept for the app's run, so a return to Play
     * within 1 s of the last fetch sends nothing.
     */
    class Schedule(private val minGapMs: Long = MIN_GAP_MS, private val periodMs: Long = PERIOD_MS) {
        private var last: Long? = null

        /**
         * How long to wait at [now] before the next fetch: on a [visit] none, unless the last fetch
         * was less than [minGapMs] ago; then, as while Play stays visible, until [periodMs] after the last fetch.
         */
        fun wait(now: Long, visit: Boolean): Long {
            val last = last ?: return 0
            if (visit && now - last >= minGapMs) return 0
            return (last + periodMs - now).coerceAtLeast(0)
        }

        /** [wait] while Play is [visible]; null (no fetch) while it is not. */
        fun next(visible: Boolean, now: Long, visit: Boolean): Long? = if (visible) wait(now, visit) else null

        /** A fetch started at [at]. */
        fun fetched(at: Long) {
            last = at
        }
    }

    const val MIN_GAP_MS = 1_000L
    const val PERIOD_MS = 60_000L

    /** The fetches of this app run ([Schedule]). */
    val schedule = Schedule()

    /** The last answer of this app run; null until the first. Never on disk. */
    val current = MutableStateFlow<Stats?>(null)

    /** One GET of [url]: the body of a 2xx answer, at most [ANSWER_LIMIT] bytes; anything else throws. */
    fun get(url: String): String {
        val connection = (URL(url).openConnection() as HttpURLConnection).apply {
            connectTimeout = TIMEOUT_MS
            readTimeout = TIMEOUT_MS
            useCaches = false
            setRequestProperty("User-Agent", userAgent)
            setRequestProperty("Accept", "application/json")
        }
        try {
            val code = connection.responseCode
            if (code !in 200..299) throw Status(code)
            return connection.inputStream.use { input ->
                val out = ByteArrayOutputStream()
                val buffer = ByteArray(4096)
                while (true) {
                    val n = input.read(buffer)
                    if (n < 0) break
                    out.write(buffer, 0, n)
                    if (out.size() > ANSWER_LIMIT) throw IOException("answer longer than $ANSWER_LIMIT bytes")
                }
                out.toString(Charsets.UTF_8.name())
            }
        } finally {
            connection.disconnect()
        }
    }
}
