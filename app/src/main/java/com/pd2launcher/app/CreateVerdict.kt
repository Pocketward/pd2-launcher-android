package com.pd2launcher.app

import java.util.Locale

/**
 * Test builds only: one service-log line for each join of an online game ([line]), from the
 * engine's network lines (Detailed logging), so a report says at once how a create went:
 * "create verdict: game started 59.6 s ago; realm exchange 24100 ms; logon answered no (2 sends,
 * 1 answers); window 15.2 s".
 *  - game started: from the game's start ([line]'s `gameStartedMs`) to the game server connect;
 *  - realm exchange: on the realm socket (port [REALM_PORT]) that closed last before that connect,
 *    the time from its first answer to its last, as the engine's socket line says it (the engine
 *    does not time the create request itself);
 *  - logon answered: whether the game server socket got a second answer ([answered]), from the
 *    engine's own count on that socket's line, not from a sampled traffic figure;
 *  - window: the game server socket's open time, at most [JoinWindow.MAX_MS].
 * The line comes when the engine writes the game server socket's line, at its close: about 15 s
 * after the connect for a join that failed, at the game's end for one that went on. It holds no
 * address, name or key. Not thread-safe: the service uses it on its worker.
 */
class CreateVerdict {
    /** One socket's line: its port, how long it was open, its sends and answers, and its first and last answer (s after the connect). */
    data class Tally(
        val socket: String,
        val port: Int,
        val seconds: Double,
        val sends: Int,
        val answers: Int,
        val firstS: Double?,
        val lastS: Double?,
    )

    private class Join(val gameAgeS: Double?, val realm: Tally?)

    /** The realm socket that closed last. */
    private var realm: Tally? = null

    /** The game server sockets connected and not closed yet, by `pid socket`. */
    private val joins = LinkedHashMap<String, Join>()

    /**
     * The engine's [line] at [nowMs] (elapsed ms), with the game's start [gameStartedMs] (elapsed
     * ms; null: not known): the verdict line when it closes a game server socket that this saw
     * connect, else null.
     */
    fun line(line: String, nowMs: Long, gameStartedMs: Long?): String? {
        JoinWindow.connect(line)?.let { (socket, _) ->
            if (joins.size >= MAX_OPEN) joins.remove(joins.keys.first())
            joins[socket] = Join(gameStartedMs?.let { (nowMs - it) / 1000.0 }, realm)
            return null
        }
        val tally = tally(line) ?: return null
        if (tally.port == REALM_PORT) {
            realm = tally
            return null
        }
        if (tally.port != JoinWindow.GAME_PORT) return null
        val join = joins.remove(tally.socket) ?: return null
        return text(join.gameAgeS, join.realm, tally)
    }

    companion object {
        /** The realm servers' port (Battle.net's realm, as Diablo II's). */
        const val REALM_PORT = 6113

        /** The game server sockets kept at most while open. */
        private const val MAX_OPEN = 8

        /** Whether the game server answered the logon: a second answer on its socket. */
        fun answered(game: Tally): Boolean = game.answers >= 2

        /** The verdict's text ([CreateVerdict]). */
        fun text(gameAgeS: Double?, realm: Tally?, game: Tally): String {
            val started = gameAgeS?.let { String.format(Locale.ROOT, "game started %.1f s ago", it) } ?: "game start unknown"
            val exchange = realm?.let { r ->
                if (r.firstS != null && r.lastS != null) "realm exchange ${Math.round((r.lastS - r.firstS) * 1000)} ms" else "realm exchange none (no answer)"
            } ?: "realm exchange unknown"
            val logon = "logon answered ${if (answered(game)) "yes" else "no"} (${game.sends} sends, ${game.answers} answers)"
            val window = String.format(Locale.ROOT, "window %.1f s", minOf(game.seconds, JoinWindow.MAX_MS / 1000.0))
            return "create verdict: $started; $exchange; $logon; $window"
        }

        /**
         * The engine's socket line ("0020:0024 socket 0x5bc to <address>:4000 closed after 15.19 s:
         * 2 sends, 1 answers (first after 0.32 s, last after 0.32 s)") as a [Tally], its socket as
         * `pid socket` ([JoinWindow.connect]'s form); null for any other line.
         */
        fun tally(line: String): Tally? {
            val match = TALLY.find(line) ?: return null
            val (pid, socket, port, seconds, sends, answers, first, last) = match.destructured
            return Tally(
                "$pid $socket", port.toIntOrNull() ?: return null, seconds.toDoubleOrNull() ?: return null,
                sends.toIntOrNull() ?: return null, answers.toIntOrNull() ?: return null, first.toDoubleOrNull(), last.toDoubleOrNull(),
            )
        }

        // No brace in the pattern: Android's ICU regex refuses a bare '}'.
        internal const val TALLY_PATTERN =
            """ ([0-9a-f]+):[0-9a-f]+ socket (0x[0-9a-f]+) to \S+:([0-9]+) closed after ([0-9.]+) s: ([0-9]+) sends, ([0-9]+) answers""" +
                """(?: \(first after ([0-9.]+) s, last after ([0-9.]+) s\))?"""
        private val TALLY = Regex(TALLY_PATTERN)
    }
}
