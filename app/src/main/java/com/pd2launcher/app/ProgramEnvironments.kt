package com.pd2launcher.app

/**
 * What each Wine program of the game really got: one line in the service log when the service
 * first sees a process of the app's user ID named as one of [PROGRAMS] ([next]: its looks while
 * the game runs and while the official launcher's session runs), from that process's
 * `/proc/<pid>/environ` ([read]), with only the names that set the graphics, Wine or Box64
 * ([allowed]): no other value reaches the log, so no key, account or folder of the player can.
 * The app's own folders are written by their short names ([folders], [ProcessStarter.shortened]),
 * and a value longer than [VALUE_LIMIT] characters is cut. A process that cannot be read gets a
 * line that says why. Thread-safe: the worker's memory look and the launcher's watch both use it.
 */
class ProgramEnvironments(private val read: (Int) -> ByteArray, private val folders: List<Pair<String, String>>) {
    /** The processes already written, while they run. */
    private val seen = HashSet<UidProcess>()

    /** The lines for the [processes] of the app's user ID seen now: one for each program not seen before. */
    @Synchronized
    fun next(processes: List<UidProcess>): List<String> {
        val programs = processes.filter { process -> PROGRAMS.any { process.name.equals(it, ignoreCase = true) } }
        seen.retainAll(programs.toSet())
        return programs.filter(seen::add).map { process ->
            val environ = try {
                read(process.pid)
            } catch (error: Exception) {
                return@map "game environment of ${process.name} (pid ${process.pid}): not readable ($error)"
            }
            line(process, environ, folders)
        }
    }

    companion object {
        /** The programs whose environment is written: the game and the official launcher. */
        val PROGRAMS = listOf(LauncherGame.PROCESS, LauncherHang.PROCESS)

        /** The longest value written; a longer one is cut there. */
        const val VALUE_LIMIT = 300

        private val GRAPHICS = listOf("MESA_", "GALLIUM_", "ZINK_", "TU_", "FD_", "VK_", "LIBGL_", "__GLX_", "__EGL_", "PAN_", "RADV_", "HYBRIS_", "PD2_HYBRIS_")
        private val WINE = listOf("WINEDEBUG", "WINEDLLOVERRIDES")
        private const val BOX64 = "BOX64_"

        /** Whether the variable [name] may go into the line: a graphics, Wine or Box64 name. */
        fun allowed(name: String): Boolean = name in WINE || name.startsWith(BOX64) || GRAPHICS.any { name.startsWith(it) }

        /**
         * The line for [process] with the environment [environ] (`/proc/<pid>/environ`: entries
         * `NAME=value`, each ended by a NUL byte): the allowed names, graphics first, then Wine,
         * then Box64, each group by name.
         */
        fun line(process: UidProcess, environ: ByteArray, folders: List<Pair<String, String>>): String {
            val entries = String(environ, Charsets.UTF_8).split('\u0000').mapNotNull { entry ->
                val equals = entry.indexOf('=')
                if (equals <= 0) return@mapNotNull null
                val name = entry.substring(0, equals)
                if (!allowed(name)) return@mapNotNull null
                val value = ProcessStarter.shortened(entry.substring(equals + 1), folders)
                name to if (value.length > VALUE_LIMIT) value.take(VALUE_LIMIT) + "... (cut)" else value
            }.sortedWith(compareBy({ group(it.first) }, { it.first }))
            val text = if (entries.isEmpty()) "no graphics, Wine or Box64 value" else entries.joinToString(" ") { (name, value) -> "$name=$value" }
            return "game environment of ${process.name} (pid ${process.pid}): $text"
        }

        private fun group(name: String): Int = when {
            name.startsWith(BOX64) -> 2
            name in WINE -> 1
            else -> 0
        }
    }
}
