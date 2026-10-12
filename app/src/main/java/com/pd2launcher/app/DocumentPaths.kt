package com.pd2launcher.app

import com.pd2launcher.app.ui.Play
import java.io.File
import java.io.FileNotFoundException
import java.io.IOException
import java.io.UncheckedIOException
import java.nio.file.Files
import java.nio.file.LinkOption
import java.nio.file.Paths
import java.util.Locale

/**
 * The document IDs of the Files app's "PD2 Launcher" root ([GameFilesProvider]) and the files
 * they stand for: [ROOT] is the Diablo II folder in use ([game]; none when there is no game
 * folder), and a file's ID is [ROOT], a slash and its path below that folder
 * (`game/Save/Hero.d2s`). These are the real files, not copies.
 *
 * Every ID resolves only below the game folder or, through a link, below [player] (the game
 * folder's `Save` links into `files/userdata/Save`, [SaveKeeper]): `.`, `..` and empty names are
 * refused, and so is any path whose real location (links followed) is elsewhere. The Launcher's
 * own records (`.pd2launcher` names) are neither shown nor reachable. Each folder counts only when
 * its canonical path is its own, no link on the way to it; and what a descriptor opened is checked
 * again by the path the kernel gives it ([opened]).
 */
class DocumentPaths(game: File?, player: File?) {
    private val game = game?.takeIf { canonical(it) == it }
    private val player = player?.takeIf { canonical(it) == it }

    /** The file [documentId] stands for; throws [FileNotFoundException] for any other ID. */
    fun file(documentId: String): File {
        val base = game ?: throw FileNotFoundException("there is no game folder")
        if (documentId != ROOT && !documentId.startsWith("$ROOT/")) throw FileNotFoundException("unknown document $documentId")
        val relative = documentId.removePrefix(ROOT).removePrefix("/")
        if (documentId != ROOT && relative.split('/').any { !plainName(it) }) throw FileNotFoundException("not a plain path: $documentId")
        val file = if (relative.isEmpty()) base else File(base, relative)
        if (!allowed(file.canonicalFile)) throw FileNotFoundException("$documentId leads out of the game folder")
        return file
    }

    /** Whether there is a game folder to show. */
    fun available(): Boolean = game?.isDirectory == true

    /**
     * The ID of [name] in folder [parentId], after checking it is a plain name that differs not
     * only in case from another in that folder ([caseClash]; [renaming], the name of the file
     * being renamed there, is not another).
     */
    fun child(parentId: String, name: String, renaming: String? = null): String {
        if (!plainName(name)) throw FileNotFoundException("not a plain file name: $name")
        if (caseClash(parentId, name, renaming)) throw FileNotFoundException("$name differs only in case from a file in $parentId")
        return "$parentId/$name"
    }

    /**
     * Whether folder [parentId] holds a name other than [name] (and [renaming]) that differs from
     * it only in case: Wine takes the two for one file, and the engine refuses a folder with such
     * a pair.
     */
    fun caseClash(parentId: String, name: String, renaming: String? = null): Boolean =
        (file(parentId).list() ?: emptyArray()).any { it != name && it != renaming && it.equals(name, ignoreCase = true) }

    /** The IDs in folder [documentId], by name; links that lead out are left out. */
    fun children(documentId: String): List<String> =
        (file(documentId).list() ?: emptyArray()).sorted().filter(::plainName).map { "$documentId/$it" }
            .filter { id -> runCatching { file(id) }.isSuccess }

    /** [documentId]'s folder; null for [ROOT]. */
    fun parent(documentId: String): String? = if ('/' in documentId) documentId.substringBeforeLast('/') else null

    /** Whether [documentId] is below (or is) [parentId]. */
    fun isChild(parentId: String, documentId: String): Boolean = documentId == parentId || documentId.startsWith("$parentId/")

    /** Why a change cannot be made now. */
    enum class Refusal {
        /** The root folder itself is not removed, renamed or moved. */
        FIXED,
        /** The game runs: it writes its folder and, when it closes, the characters. */
        GAME_RUNNING,
        /** A launch is under way ([gameStarting]): the engine prepares the game's files for it. */
        GAME_STARTING,
        /** The Launcher works on the game folder (install, update, repair). */
        LAUNCHER_BUSY,
        /** A program or library ([executable]): Wine would run it as the app. */
        EXECUTABLE,
    }

    /** What a change does to the document it names. */
    enum class Change {
        /** A file is written, or something is created in a folder. */
        WRITE,
        /** It is removed. */
        DELETE,
        /** It is renamed or moved (the new name is [nameRefusal]'s to check). */
        MOVE,
    }

    /**
     * Whether a [change] to [documentId] is refused: [gameRunning], the game runs; [gameStarting],
     * a launch is under way ([Companion.gameStarting]); [busy], a Launcher job runs. Another app
     * the player lets write here could otherwise put code where Wine loads it, as the app's own
     * user (a DLL beside the game, say): a file [executable] names is never written, renamed or
     * moved, nor a folder holding one renamed or moved ([nameRefusal] keeps new names from being
     * such). Removing one is allowed. The player's own files (characters, loot filters,
     * settings) are none of these.
     */
    fun refusal(
        documentId: String,
        gameRunning: Boolean,
        busy: Boolean,
        change: Change = Change.WRITE,
        gameStarting: Boolean = false,
    ): Refusal? {
        val file = file(documentId)
        return when {
            change != Change.WRITE && documentId == ROOT -> Refusal.FIXED
            gameRunning -> Refusal.GAME_RUNNING
            gameStarting -> Refusal.GAME_STARTING
            busy -> Refusal.LAUNCHER_BUSY
            change == Change.WRITE && !file.isDirectory && executable(file.name) -> Refusal.EXECUTABLE
            change == Change.MOVE && holdsExecutable(file) -> Refusal.EXECUTABLE
            else -> null
        }
    }

    /** Whether [name] may be given to a file or folder here (created, renamed to, imported as). */
    fun nameRefusal(name: String): Refusal? = if (executable(name)) Refusal.EXECUTABLE else null

    /**
     * Whether [file] is named as an executable, or is a folder with one at any depth (links not
     * followed). A folder the walk cannot finish (one that vanishes meanwhile, say) counts as
     * holding one, rather than failing the whole listing.
     */
    private fun holdsExecutable(file: File): Boolean {
        if (executable(file.name)) return true
        if (Files.isSymbolicLink(file.toPath()) || !file.isDirectory) return false
        return try {
            Files.walk(file.toPath(), MAX_DEPTH).use { paths -> paths.anyMatch { executable(it.fileName.toString()) } }
        } catch (_: IOException) {
            true
        } catch (_: UncheckedIOException) {
            true
        }
    }

    /**
     * Whether a descriptor that opened the file [reported] (the path `/proc/self/fd/<n>` names:
     * where it really is, whatever links led there) may be used: below the game folder or the
     * player's, a regular file ([regularFile]; not a FIFO, say) with one name ([links]: a hard link
     * would reach a file elsewhere) and, opened for [write], not named as a program or library
     * ([executable]: a link's own name is not the file's).
     */
    fun openedFile(reported: String, regularFile: Boolean, links: Long, write: Boolean): Boolean =
        allowed(File(reported)) && regularFile && links == 1L && !(write && executable(File(reported).name))

    /** Whether a descriptor that opened the folder [reported] (as [openedFile]) may be used: below the game folder or the player's. */
    fun openedFolder(reported: String): Boolean = allowed(File(reported))

    private fun allowed(real: File): Boolean =
        (game != null && below(real, game)) || (player != null && below(real, player))

    companion object {
        const val ROOT = "game"
        private const val MAX_DEPTH = 32

        /**
         * Whether a Play or official PD2 launcher request taken at [requestedAt] (null: none under
         * way) still holds the game folder at [now]: the engine prepares the game's files
         * (`d2gl.json` among them) before the game shows as running, and another app's change
         * then would reach the game. The session service forgets the request once its game or
         * session shows or it fails; at most [Play.LAUNCH_PENDING_MS] after it, as Play's buttons.
         */
        fun gameStarting(requestedAt: Long?, now: Long): Boolean =
            requestedAt != null && now - requestedAt < Play.LAUNCH_PENDING_MS

        /**
         * Programs and libraries Windows and Wine run or load, by upper-case extension: the
         * game's and its renderers' DLLs (`ddraw.dll`, `d2gl.dll`, the DLLs in `ProjectD2`), ASI
         * plugins, installers and scripts.
         */
        private val EXECUTABLE_EXTENSIONS = setOf(
            "EXE", "DLL", "ASI", "COM", "BAT", "CMD", "SCR", "PIF", "CPL", "OCX", "DRV", "SYS", "MSI", "MSP",
        )

        /**
         * Whether [name]'s extension is a program's or library's ([EXECUTABLE_EXTENSIONS]), after
         * the trailing dots and spaces Windows names drop (`ddraw.dll.` is `ddraw.dll`). Compared
         * upper-cased, as Wine matches names: `ı` and `ſ` become `I` and `S` there, so `x.ası`
         * is `x.ASI`.
         */
        fun executable(name: String): Boolean =
            name.trimEnd('.', ' ').substringAfterLast('.', "").uppercase(Locale.ROOT) in EXECUTABLE_EXTENSIONS

        /**
         * [folder] when it is where a game folder can be, else null: the Managed prefix's Diablo II
         * folder ([SessionPaths.managedGame]) or a folder copy's (`files/import/<token>/Diablo II`,
         * [SessionPaths.importedGame]), named exactly so, with every part from the data folder down
         * a real folder, walked without following links. So the root neither lies elsewhere in the
         * app's files (the App userland, the Launcher's records) nor holds them, whatever the
         * engine's word or the Launcher's record of a folder used in place (which code under Wine
         * can write) names. Null too while the folder is missing.
         */
        fun gameFolder(paths: SessionPaths, folder: File): File? {
            val token = folder.parentFile
            val place = folder == paths.managedGame ||
                (folder.name == "Diablo II" && token?.parentFile == paths.imports && plainName(token.name))
            return folder.takeIf { place && realFolders(paths, it) }
        }

        /** [SessionPaths.userData] when every part from the data folder down is a real folder, as [gameFolder] walks it; else null. */
        fun playerFolder(paths: SessionPaths): File? = paths.userData.takeIf { realFolders(paths, it) }

        /** Whether every part of [folder] from the data folder down is a real folder, walked without following links. */
        private fun realFolders(paths: SessionPaths, folder: File): Boolean {
            val data = File(paths.data).toPath()
            var path = data
            for (part in data.relativize(folder.toPath())) {
                path = path.resolve(part)
                if (!Files.isDirectory(path, LinkOption.NOFOLLOW_LINKS)) return false
            }
            return true
        }

        private fun canonical(file: File): File? = try {
            file.canonicalFile
        } catch (_: IOException) {
            null
        }

        /** A name in one folder: not empty, `.` or `..`, no slash or NUL, not the Launcher's records. */
        fun plainName(name: String): Boolean =
            name.isNotEmpty() && name != "." && name != ".." && '/' !in name && '\u0000' !in name && !name.startsWith(".pd2launcher")

        private fun below(file: File, folder: File): Boolean =
            file.path == folder.path || file.path.startsWith(folder.path + File.separator)
    }
}

/**
 * Removes an entry of an open folder and, a folder, all it holds, each folder through its own
 * descriptor ([Folders.open] follows no link): no deeper path is looked up by name, so a folder
 * swapped for a link meanwhile is removed itself, never what it leads to. [listed] gets each
 * folder once it is listed.
 */
internal class PinnedRemoval(private val folders: Folders, private val listed: (String) -> Unit = {}) {
    /** Folders opened below open ones. */
    interface Folders {
        /**
         * `/proc/self/fd/<n>` of [name] in the open folder [folder], opened as a folder without
         * following a link and made owner-only (read, write, search); null when it is no folder
         * (a file, a link).
         */
        fun open(folder: String, name: String): String?

        fun close(descriptor: String)
    }

    /** Removes [name] of the open folder [folder] (`/proc/self/fd/<n>`). */
    fun remove(folder: String, name: String) {
        val inner = folders.open(folder, name)
        if (inner != null) {
            try {
                val entries = File(inner).list() ?: throw IOException("$name cannot be listed")
                listed(inner)
                for (entry in entries) remove(inner, entry)
            } finally {
                folders.close(inner)
            }
        }
        // The entry itself: rmdir or unlink, which follow no link.
        Files.delete(Paths.get(folder, name))
    }
}
