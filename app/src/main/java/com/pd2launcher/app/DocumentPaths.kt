package com.pd2launcher.app

import java.io.File
import java.io.FileNotFoundException

/**
 * The document IDs of the Files app's "PD2 Launcher" root ([GameFilesProvider]) and the files
 * they stand for: [ROOT] is the Diablo II folder in use ([game]; none when there is no game
 * folder), and a file's ID is [ROOT], a slash and its path below that folder
 * (`game/Save/Hero.d2s`). These are the real files, not copies.
 *
 * Every ID resolves only below the game folder or, through a link, below [player] (the game
 * folder's `Save` links into `files/userdata/Save`, [SaveKeeper]): `.`, `..` and empty names are
 * refused, and so is any path whose real location (links followed) is elsewhere. The Launcher's
 * own records (`.pd2launcher` names) are neither shown nor reachable.
 */
class DocumentPaths(private val game: File?, player: File) {
    private val gameReal = game?.canonicalFile
    private val playerReal = player.canonicalFile

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
        /** The Launcher works on the game folder (install, update, repair). */
        LAUNCHER_BUSY,
    }

    /**
     * Whether a change to [documentId] is refused: [gameRunning], the game runs; [busy], a
     * Launcher job runs. [whole]: the ID itself is removed, renamed or moved (not only written
     * or created in).
     */
    fun refusal(documentId: String, gameRunning: Boolean, busy: Boolean, whole: Boolean = false): Refusal? {
        file(documentId)
        return when {
            whole && documentId == ROOT -> Refusal.FIXED
            gameRunning -> Refusal.GAME_RUNNING
            busy -> Refusal.LAUNCHER_BUSY
            else -> null
        }
    }

    private fun allowed(real: File): Boolean = (gameReal != null && below(real, gameReal)) || below(real, playerReal)

    companion object {
        const val ROOT = "game"

        /** A name in one folder: not empty, `.` or `..`, no slash or NUL, not the Launcher's records. */
        fun plainName(name: String): Boolean =
            name.isNotEmpty() && name != "." && name != ".." && '/' !in name && '\u0000' !in name && !name.startsWith(".pd2launcher")

        private fun below(file: File, folder: File): Boolean =
            file.path == folder.path || file.path.startsWith(folder.path + File.separator)
    }
}
