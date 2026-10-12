package com.pd2launcher.app

import java.io.File
import java.io.IOException
import java.util.Locale

/**
 * A game that ended within [WINDOW_MS] of starting. The engine reports any end of the game's
 * process as the game having exited, without its exit status, and FEX logs nothing by default,
 * so such an end leaves no error of its own. The service log then gets what can still be read
 * before the game environment stops: the FEX telemetry the processes wrote ([telemetry]: an exit
 * writes the crash mask 0, a fatal signal adds its bit, and FEX returning before it runs the
 * program writes none) and the app's processes still running (a Wine server or a FEXServer left
 * from before, say).
 */
object EarlyGameEnd {
    const val WINDOW_MS = 10_000L

    /** The FEX processes the launch starts before the game (the FEX probe's) are counted in too. */
    const val LAUNCH_MS = 15_000L

    /** A game session that started at [startedAt] (null: not seen starting) ended at [endedAt] early. */
    fun early(startedAt: Long?, endedAt: Long): Boolean = startedAt != null && endedAt - startedAt < WINDOW_MS

    /**
     * "<program> <age> s ago, crash mask <mask>" for each FEX telemetry file (`<program>.telem`)
     * in [dir] written at or after [since], newest first; times in ms since the epoch.
     */
    fun telemetry(dir: File, since: Long, now: Long): List<String> =
        dir.listFiles { file -> file.name.endsWith(".telem") }.orEmpty()
            .filter { it.lastModified() >= since }
            .sortedByDescending { it.lastModified() }
            .map { file ->
                val mask = try {
                    crashMask(file.readText())?.toString() ?: "none"
                } catch (_: IOException) {
                    "unreadable"
                }
                String.format(Locale.ROOT, "%s %.1f s ago, crash mask %s", file.name.removeSuffix(".telem"), (now - file.lastModified()) / 1000.0, mask)
            }

    /** The `Crash mask` of a FEX telemetry file's [text] (bit n: signal n), null without one. */
    fun crashMask(text: String): Long? =
        text.lineSequence().firstNotNullOfOrNull { line ->
            line.takeIf { it.startsWith(CRASH_MASK) }?.removePrefix(CRASH_MASK)?.trim()?.toLongOrNull()
        }

    private const val CRASH_MASK = "Crash mask:"
}
