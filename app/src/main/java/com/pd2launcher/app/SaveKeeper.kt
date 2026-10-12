package com.pd2launcher.app

import java.io.File
import java.io.IOException
import java.nio.file.Files
import java.nio.file.LinkOption
import java.nio.file.StandardCopyOption
import java.nio.file.attribute.PosixFilePermissions
import java.util.Locale

/**
 * Keeps the player's characters in one folder of their own, [SessionPaths.userSave]
 * (`files/userdata/Save`), apart from any Diablo II folder. The game keeps characters in
 * `<InstallPath>\Save` (the engine writes no `Save Path`), so the app makes the `Save` of the
 * game folder in use, the Managed prefix's ([SessionPaths.managedGame]) or a folder copy's below
 * [SessionPaths.imports], a symbolic link to it ([link]) before a launch; Wine follows it like a
 * folder. Start over removes the Managed prefix without following links (the engine's
 * `remove_owned_tree`), so it takes only the link. A folder copy gets no link before Start over
 * or a new folder copy ([moveImported]): the engine refuses an existing folder holding any link
 * (its `tree_bytes`), and after Start over the same copy can be imported again; its Save only has
 * its saves moved out and goes. The launch, after the import, links it again.
 *
 * A `Save` that is still a real folder (one the game or an installer made) has its saves moved
 * into the folder first. `Save` in another case (a picked folder's `save` or
 * `SAVE`, which the copy keeps) is the game's `Save` to Wine too: its saves are moved the same
 * way and it goes, so a game folder is left with the link only and a folder copy with no `Save`
 * at all ([spellings]). A character is the files at the top of a Save that share a name up to
 * the first dot with a `.d2s` (`Hero.d2s`, `Hero.key`, `Hero.ma0`, `Hero.map`). One already there
 * with the same content is not kept twice; one with other content is never overwritten: the one
 * saved last ([savedAt]) stays, the one already there on a tie, and the other moves into a
 * `backup-<time>` folder in [SessionPaths.saveBackups]. Every other saved file is kept on its own
 * by the same rule, dated by its own time: the shared stash, and the files of the folders inside
 * a Save (PD2's `ProjectD2`, the online characters' hotkeys and automaps), which are kept in the
 * same folders of the player's folder, every spelling of Save merged into them; the other copy
 * goes to a backup laid out as the player's folder is. What is left (hidden files and folders,
 * links, which are never followed, a file or folder the player's folder holds as another kind)
 * moves into a backup as the Save held it. Every move is a rename of one file (or of what is left
 * of a folder), so a stop part way leaves each file in the game folder, the player's folder or a
 * backup, and the next keep goes on from there. A folder more than [MAX_DEPTH] deep fails the keep
 * before anything moves.
 */
object SaveKeeper {
    /** How deep a folder inside a Save is kept, as on the desktop. */
    private const val MAX_DEPTH = 16

    /**
     * Makes [save] a link to [userSave] once its saves, and those of every other spelling of it
     * in the game folder ([spellings]), are in [userSave] ([moveOut]); a link elsewhere is
     * replaced (it holds nothing itself). Without the game folder ([save]'s parent) only the
     * saves are moved and no link is made.
     */
    fun link(save: File, userSave: File, backups: File, now: Long = System.currentTimeMillis(), log: (String) -> Unit = {}) {
        for (other in spellings(save.parentFile)) {
            if (other.name != save.name) moveOut(other, userSave, backups, now, log)
        }
        val path = save.toPath()
        val target = userSave.toPath()
        if (Files.isSymbolicLink(path) && Files.readSymbolicLink(path) == target) return
        moveOut(save, userSave, backups, now, log)
        if (save.parentFile?.isDirectory != true) return
        Files.createSymbolicLink(path, target)
        log("${save.path} links to ${userSave.path}")
    }

    /**
     * Moves the saves of [save] into [userSave] and removes [save]: a link (to anywhere) is
     * removed without being followed, what else a Save folder held moves to a backup folder.
     * Makes no link.
     */
    fun moveOut(save: File, userSave: File, backups: File, now: Long = System.currentTimeMillis(), log: (String) -> Unit = {}) {
        val path = save.toPath()
        ownerOnlyDirs(userSave)
        if (Files.isSymbolicLink(path)) {
            Files.delete(path)
        } else if (Files.isDirectory(path, LinkOption.NOFOLLOW_LINKS)) {
            // A folder too deep fails the keep before anything moves.
            checkDepth(save, 0)
            val moved = Moved()
            mergeFolder(save, userSave, "", backups, now, log, moved)
            if (moved.characters + moved.files > 0) {
                log("moved ${moved.characters} characters and ${moved.files} other saved files from ${save.path} into ${userSave.path}")
            }
            val left = save.list()?.size ?: 0
            if (left == 0) {
                Files.delete(path)
            } else {
                // Whatever else was in it (hidden files, links) is kept, not removed.
                val backup = backupFolder(backups, now, emptyList())
                ownerOnlyDirs(backup)
                Files.move(path, File(backup, save.name).toPath())
                log("the rest of ${save.path} ($left entries) is in ${backup.name}")
            }
        } else if (Files.exists(path, LinkOption.NOFOLLOW_LINKS)) {
            val backup = backupFolder(backups, now, listOf(save.name))
            ownerOnlyDirs(backup)
            Files.move(path, File(backup, save.name).toPath())
        }
    }

    /**
     * Before Start over: the Managed prefix's Save [link]s to the player's folder (Start over
     * removes the prefix) and every folder copy's has its saves moved there ([moveImported]).
     */
    fun keepAll(paths: SessionPaths, now: Long = System.currentTimeMillis(), log: (String) -> Unit = {}) {
        val managed = File(paths.managedGame, "Save")
        if (managed.parentFile?.isDirectory == true) link(managed, paths.userSave, paths.saveBackups, now, log)
        moveImported(paths, now, log)
    }

    /**
     * [moveOut] for the Save of every folder copy below [SessionPaths.imports], in every spelling
     * ([importedSaves]): no link is left in a copy, which the engine may validate again as an
     * existing folder, and no save for the copy's removal to take.
     */
    fun moveImported(paths: SessionPaths, now: Long = System.currentTimeMillis(), log: (String) -> Unit = {}) {
        for (save in importedSaves(paths)) moveOut(save, paths.userSave, paths.saveBackups, now, log)
    }

    /** The Save folder of the game folder a snapshot names ([gameFolder]; null: the Managed prefix's). */
    fun liveSave(paths: SessionPaths, gameFolder: String?): File = File(gameFolder?.let(::File) ?: paths.managedGame, "Save")

    /** The Saves of the folder copies below [SessionPaths.imports], every spelling of each ([spellings]). */
    fun importedSaves(paths: SessionPaths): List<File> =
        paths.imports.listFiles()?.sortedBy { it.name }?.flatMap { spellings(File(it, "Diablo II")) } ?: emptyList()

    /**
     * The entries of [gameFolder] named `Save` in any (ASCII) case, by name: Wine finds each as the
     * game's `Save`, and a picked folder is copied with the name it had. None without the folder.
     */
    private fun spellings(gameFolder: File?): List<File> =
        gameFolder?.list()?.filter { it.lowercase(Locale.ROOT) == "save" }?.sorted()?.map { File(gameFolder, it) } ?: emptyList()

    /** What a keep moved out of one Save: characters, and other saved files each on its own. */
    private class Moved {
        var characters = 0
        var files = 0
    }

    /** Fails when a folder in [folder] (at [depth] below a Save's top) is deeper than [MAX_DEPTH]; links are not followed. */
    private fun checkDepth(folder: File, depth: Int) {
        for (child in folder.listFiles() ?: emptyArray()) {
            if (!Files.isDirectory(child.toPath(), LinkOption.NOFOLLOW_LINKS)) continue
            if (depth >= MAX_DEPTH) throw IOException("a folder in ${folder.path} is too deep to keep")
            checkDepth(child, depth + 1)
        }
    }

    /**
     * Moves the saves of [from] into [into], the player's folder's [folder] (`""` for its top,
     * else ending in `/`): at the top the characters ([merge]), and every other regular file on
     * its own ([mergeFile]); every folder the same way, removed once it is empty. Hidden entries,
     * links and special files stay in [from], for the rest of the Save to go to a backup.
     */
    private fun mergeFolder(from: File, into: File, folder: String, backups: File, now: Long, log: (String) -> Unit, moved: Moved) {
        ownerOnlyDirs(into)
        val files = regularFiles(from)
        if (folder.isEmpty()) {
            for ((name, group) in files.groupBy { it.name.substringBefore('.') }.toSortedMap()) {
                val existing = characters(into)[name]
                if (isCharacter(group) || existing != null && isCharacter(existing)) {
                    merge(name, group, into, backups, now, log)
                    moved.characters++
                } else {
                    // Files of no character (the shared stash): each on its own.
                    for (file in group) mergeFile(file, into, folder, backups, now, log)
                    moved.files += group.size
                }
            }
        } else {
            for (file in files.sortedBy { it.name }) mergeFile(file, into, folder, backups, now, log)
            moved.files += files.size
        }
        val folders = (from.listFiles() ?: emptyArray())
            .filter { !it.name.startsWith(".") && Files.isDirectory(it.toPath(), LinkOption.NOFOLLOW_LINKS) }
            .sortedBy { it.name }
        for (child in folders) {
            val target = File(into, child.name).toPath()
            // The player's folder holds a file or a link by that name: it stays, and this folder goes with the rest.
            if (Files.exists(target, LinkOption.NOFOLLOW_LINKS) && !Files.isDirectory(target, LinkOption.NOFOLLOW_LINKS)) continue
            mergeFolder(child, target.toFile(), "$folder${child.name}/", backups, now, log, moved)
            if (child.list()?.isEmpty() == true) Files.delete(child.toPath())
        }
    }

    /**
     * Moves the character [name] ([files]) into [into]: as it is when [into] has none, dropped
     * when [into] has the same content, else the one saved last ([savedAt]; on a tie the one in
     * [into]) stays in [into] and the other goes to a backup folder.
     */
    private fun merge(name: String, files: List<File>, into: File, backups: File, now: Long, log: (String) -> Unit) {
        val existing = characters(into)[name]
        if (existing != null) {
            if (sameContent(files, existing)) {
                files.forEach { Files.delete(it.toPath()) }
                return
            }
            val backup = backupFolder(backups, now, (files + existing).map { it.name })
            if (bySavedAt.compare(savedAt(name, existing), savedAt(name, files)) >= 0) {
                move(files, backup)
                log("character $name: the one saved last stays, the other is in ${backup.name}")
                return
            }
            move(existing, backup)
            log("character $name: the one saved earlier moved to ${backup.name}")
        }
        move(files, into)
    }

    /**
     * Moves the saved file [file] into [into], the player's folder's [folder]: as it is when
     * [into] has none, dropped when [into] has the same content, else the one saved last (by its
     * own time; on a tie the one in [into]) stays in [into] and the other goes to a backup folder,
     * at the same place there. When [into] holds a folder or a link by that name, [file] stays
     * where it is, for the rest of the Save to go to a backup.
     */
    private fun mergeFile(file: File, into: File, folder: String, backups: File, now: Long, log: (String) -> Unit) {
        val target = File(into, file.name)
        val targetPath = target.toPath()
        if (!Files.exists(targetPath, LinkOption.NOFOLLOW_LINKS)) {
            move(listOf(file), into)
            return
        }
        if (!Files.isRegularFile(targetPath, LinkOption.NOFOLLOW_LINKS)) return
        if (sameContent(listOf(file), listOf(target))) {
            Files.delete(file.toPath())
            return
        }
        val backup = backupFolder(backups, now, listOf(folder + file.name))
        if (target.lastModified() >= file.lastModified()) {
            move(listOf(file), File(backup, folder))
            log("saved file $folder${file.name}: the one saved last stays, the other is in ${backup.name}")
            return
        }
        move(listOf(target), File(backup, folder))
        log("saved file $folder${file.name}: the one saved earlier moved to ${backup.name}")
        move(listOf(file), into)
    }

    private fun move(files: List<File>, into: File) {
        ownerOnlyDirs(into)
        for (file in files) Files.move(file.toPath(), File(into, file.name).toPath(), StandardCopyOption.REPLACE_EXISTING)
    }

    /**
     * `backup-<now>`, or the first `backup-<now>-<n>` that holds none of the paths [names] ([holds]);
     * for none, the first that does not exist yet.
     */
    private fun backupFolder(backups: File, now: Long, names: List<String>): File =
        generateSequence(0) { it + 1 }
            .map { n -> File(backups, if (n == 0) "backup-$now" else "backup-$now-$n") }
            .first { folder ->
                val path = folder.toPath()
                when {
                    !Files.exists(path, LinkOption.NOFOLLOW_LINKS) -> true
                    names.isEmpty() || !Files.isDirectory(path, LinkOption.NOFOLLOW_LINKS) -> false
                    else -> names.none { holds(folder, it) }
                }
            }

    /**
     * Whether the path [relative] names anything below [folder], no link followed: a path below a
     * file or a link counts as taken (a previous keep's rest of a Save may hold one).
     */
    private fun holds(folder: File, relative: String): Boolean {
        val parts = relative.split('/').filter { it.isNotEmpty() }
        var current = folder.toPath()
        for ((index, part) in parts.withIndex()) {
            current = current.resolve(part)
            if (!Files.exists(current, LinkOption.NOFOLLOW_LINKS)) return false
            if (index < parts.lastIndex && !Files.isDirectory(current, LinkOption.NOFOLLOW_LINKS)) return true
        }
        return true
    }

    /** The regular files of [folder], not hidden, links not followed; empty when it is missing. */
    private fun regularFiles(folder: File): List<File> =
        (folder.listFiles() ?: emptyArray())
            .filter { Files.isRegularFile(it.toPath(), LinkOption.NOFOLLOW_LINKS) && !it.name.startsWith(".") }

    /** The regular files of [folder] by character name; empty when it is missing. */
    private fun characters(folder: File): Map<String, List<File>> =
        regularFiles(folder).groupBy { it.name.substringBefore('.') }.toSortedMap()

    /** Whether [files] are a character's: one of them is a `.d2s`. */
    private fun isCharacter(files: List<File>): Boolean = files.any { it.name.lowercase(Locale.ROOT).endsWith(".d2s") }

    /**
     * When the character [name] ([files]) was saved last: by its own `<name>.d2s`, which the game
     * writes at every save (a copy with one comes before a copy without; never `Hero.bak.d2s`),
     * or, for a copy without one, by the latest of its files.
     */
    private fun savedAt(name: String, files: List<File>): Pair<Boolean, Long> {
        // Every file of the character is `name` or starts `name.`: this is `name.d2s`, any case of `.d2s`.
        val own = files.firstOrNull { it.name.length == name.length + 4 && it.name.takeLast(4).lowercase(Locale.ROOT) == ".d2s" }
        return if (own != null) true to own.lastModified() else false to files.maxOf { it.lastModified() }
    }

    private val bySavedAt = compareBy<Pair<Boolean, Long>>({ it.first }, { it.second })

    private fun sameContent(a: List<File>, b: List<File>): Boolean {
        val byName = b.associateBy { it.name }
        return a.size == b.size &&
            a.all { file ->
                val other = byName[file.name]
                other != null && file.length() == other.length() && file.readBytes().contentEquals(other.readBytes())
            }
    }

    /** [dir] and any missing parents, each owner-only (the app's umask is 0). */
    private fun ownerOnlyDirs(dir: File) {
        if (dir.isDirectory) return
        dir.parentFile?.let(::ownerOnlyDirs)
        if (!dir.mkdir() && !dir.isDirectory) throw IOException("cannot create ${dir.path}")
        Files.setPosixFilePermissions(dir.toPath(), PosixFilePermissions.fromString("rwx------"))
    }
}
