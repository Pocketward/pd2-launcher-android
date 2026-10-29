package com.pd2launcher.app

import java.io.File
import java.io.FileOutputStream
import java.io.IOException
import java.nio.channels.FileChannel
import java.nio.file.FileVisitResult
import java.nio.file.Files
import java.nio.file.LinkOption
import java.nio.file.Path
import java.nio.file.SimpleFileVisitor
import java.nio.file.StandardCopyOption
import java.nio.file.StandardOpenOption
import java.nio.file.attribute.BasicFileAttributes
import java.nio.file.attribute.PosixFilePermissions
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive

/**
 * The update in place of a prefix that an older runtime or another template made: an older app's,
 * whose runtime was Wine-GE under FEX (Wine-GE's x86-64 system folders, which the ARM64 Wine cannot
 * use), or one of another generation of this runtime's template ([GENERATION]: another Wine, other
 * registry values). That prefix in the managed root [root] becomes a copy of
 * the new runtime's prefix [template], with what the player keeps moved over from the old one: the
 * managed game folder (`C:\Program Files (x86)\Diablo II`, with its link to the characters),
 * Blizzard's download that has not finished (`C:\pd2launcher-downloads`, which the downloader
 * continues), the Launcher's records of the game folder and an imported game, the `G:` drive of a
 * game folder used in place, and the game's registry keys (Blizzard Entertainment's: the install
 * path the Launcher checks, and the game's own settings). Then the old prefix goes: its links go as
 * links, never what they lead to. No Wine prefix update runs on the phone: most files of the
 * prefix are links into the runtime's Wine, and `wineboot -u` would write through them.
 *
 * A prefix made from the current template carries `.update-timestamp` "disable" and the
 * template's generation ([ours]); any other one moves. Each step can stop and run again: the
 * new prefix is made beside the old one
 * ([MIGRATING]), the old one is renamed out of the way ([OLD]) and the new one into place, then
 * the player's files move over (each a rename), and the old one goes last. A later run finishes
 * what a stop left. When a step fails, [setAside] keeps the old game folder for Import instead.
 * Before the old prefix goes, any characters left in it move below the player's [userSave]
 * ([recoverCharacters]). What is written, copied or renamed reaches the disk ([syncDirectory],
 * each file synced) before anything that depends on it: the old prefix goes only after that.
 * [failForTest] (debug builds only) makes [migrate] fail after the old prefix is renamed, and
 * [failLateForTest] after the new prefix took its place.
 */
class LegacyPrefix(
    private val root: File,
    private val template: File,
    private val userSave: File,
    private val log: (String) -> Unit,
    private val failForTest: Boolean = false,
    private val failLateForTest: Boolean = false,
    /** Makes a folder's entries durable (fsync on the folder); the JVM tests pass their own. */
    private val syncDirectory: (Path) -> Unit = ::fsyncDirectory,
) {
    private val prefix = File(root, "prefix")
    private val old = File(root, OLD)
    private val migrating = File(root, MIGRATING)
    private val partial = File(root, PARTIAL)

    /**
     * The update in place (see the class). [kept] is where [setAside] puts the older game folder:
     * found there with no game left in either prefix, a set-aside that a stop left unfinished is
     * not taken for an update that passed. [migrate] fails, and [setAside] finishes it.
     */
    fun migrate(kept: File? = null) {
        // An older prefix [setAside] kept only waits for Ready ([removeOld]): nothing in it moves.
        if (File(old, SET_ASIDE).exists()) return
        if (!old.exists()) {
            if (!prefix.isDirectory || ours(prefix, template)) return
            val made = if (stamp(prefix) == "disable") "template generation ${generation(prefix)}" else "an older runtime's"
            log("update in place: the prefix ($made) becomes a copy of the template (generation ${generation(template)})")
            makeNew(prefix)
            Files.move(prefix.toPath(), old.toPath(), StandardCopyOption.ATOMIC_MOVE)
            syncDirectory(root.toPath())
        }
        if (failForTest) throw IOException("the update in place failed on purpose (debug builds' test switch)")
        if (!prefix.exists()) {
            // A copy a stop left from another template generation is made again.
            if (!migrating.isDirectory || !ours(migrating, template)) makeNew(old)
            Files.move(migrating.toPath(), prefix.toPath(), StandardCopyOption.ATOMIC_MOVE)
            syncDirectory(root.toPath())
        }
        if (failLateForTest) {
            throw IOException("the update in place failed on purpose after the new prefix took its place (debug builds' test switch)")
        }
        if (kept != null && Files.isDirectory(kept.toPath(), LinkOption.NOFOLLOW_LINKS) && !holdsGame(old) && !holdsGame(prefix)) {
            throw IOException("the older game folder already waits for Import at $kept")
        }
        // Until Ready: should the engine reject the moved game, Repair offers Import of it first.
        // Before the game moves, so a stop part way leaves the mark too.
        if (holdsGame(old) || holdsGame(prefix)) writeDurably(File(root, UPDATED), "${System.currentTimeMillis() / 1000}\n")
        moveOver(GAME)
        moveOver(DOWNLOADS)
        old.listFiles()?.filter { it.name.startsWith(".pd2launcher-") && it.name.endsWith(".json") }?.forEach {
            moveOver(it.name)
        }
        // The G: drive of a game folder used in place, only with its record.
        if (driveToCopy()) {
            val link = File(prefix, "dosdevices/g:").toPath()
            Files.createSymbolicLink(link, Files.readSymbolicLink(File(old, "dosdevices/g:").toPath()))
            syncDirectory(link.parent)
        }
        syncDirectory(root.toPath())
        try {
            removeOld()
        } catch (error: Exception) {
            // The update is done; what is left of the older prefix goes at Ready (UpgradeImport.cleanUp).
            log("update in place: the older prefix was not removed ($error); it goes at Ready")
        }
        log("update in place: the game, its records and its registry keys are in the new prefix")
        if (stale()) {
            log("update in place: the prefix in place is a copy of an older template; it moves once more")
            migrate(kept)
        }
    }

    /**
     * Whether the prefix in place must move once more ([migrate]): two template generation changes
     * around one stopped or failed move can leave a copy of an older template in place (a stop
     * after the second rename, or a set-aside that waits for Ready, then a newer runtime). Only
     * once the older prefix is gone, and only with a template whose copies are [ours] (else the
     * move would run again and again). [migrate] checks it at its end; the app checks it at each
     * start, before the engine starts ([AppRuntime.updatePrefix]), for a copy left in place when
     * the older prefix went at Ready ([removeOld]).
     */
    fun stale(): Boolean = !moved() && prefix.isDirectory && !ours(prefix, template) && ours(template, template)

    /**
     * Whether a move that a stop left part way waits to be finished ([migrate] runs again): the
     * older prefix is out of the way ([OLD]), no [setAside] kept it, and the move is not done: no
     * prefix in place, or the game folder, Blizzard's download, a record or the G: drive still to
     * move over. (An older prefix that only failed to go is no move to finish: it goes at Ready.)
     * The app checks it at each start, before the engine starts ([AppRuntime.updatePrefix]): a
     * move at a start (not at an unpack, which runs again whole after a stop) is finished there.
     */
    fun unfinished(): Boolean {
        if (!moved() || File(old, SET_ASIDE).exists()) return false
        if (!prefix.isDirectory) return true
        if (Files.exists(File(old, GAME).toPath(), LinkOption.NOFOLLOW_LINKS) ||
            Files.exists(File(old, DOWNLOADS).toPath(), LinkOption.NOFOLLOW_LINKS)
        ) {
            return true
        }
        return old.listFiles()?.any { it.name.startsWith(".pd2launcher-") && it.name.endsWith(".json") } == true || driveToCopy()
    }

    /**
     * Whether the older prefix's G: drive (a game folder used in place, with its record) is still
     * to be made in the new prefix: only where the new prefix has no G: at all, so a drive the
     * engine set since stays, and is no move left to finish.
     */
    private fun driveToCopy(): Boolean =
        File(prefix, POINTER).isFile && Files.isSymbolicLink(File(old, "dosdevices/g:").toPath()) &&
            !Files.exists(File(prefix, "dosdevices/g:").toPath(), LinkOption.NOFOLLOW_LINKS)

    /** Whether a set-aside ([setAside]) that a stop left part way waits to be finished: its mark is there. */
    fun settingAside(): Boolean = File(root, SETTING_ASIDE).exists()

    /** Whether the prefix in place, with no move begun, is one of an older runtime (no "disable" stamp: Wine-GE under FEX). */
    fun olderRuntimeInPlace(): Boolean = !moved() && prefix.isDirectory && stamp(prefix) != "disable"

    /** Whether the move renamed the older prefix out of the way ([OLD]): its later steps can be finished or set aside. */
    fun moved(): Boolean = Files.exists(old.toPath(), LinkOption.NOFOLLOW_LINKS)

    /** After a move failed before it renamed anything ([moved] false): the new prefix it began goes, the prefix in place stays. */
    fun discardNew() {
        UserlandInstaller.deleteTree(partial.toPath())
        UserlandInstaller.deleteTree(migrating.toPath())
        syncDirectory(root.toPath())
    }

    /**
     * After [migrate] failed: the older installation's game folder goes to [kept] by a rename (no
     * copy), for Import to take it, with a copy of the player's characters in place of its `Save`
     * link ([unlinkSave]). That folder is the one the older
     * installation's record names (an imported copy below the same folder as [kept]'s), else the
     * managed one. The older prefix waits as [OLD] until the installation is Ready ([removeOld]). A
     * new prefix the update made stays without the records and the registry keys of the game
     * folder that moved ([forgetGame]), so Blizzard's download it holds continues if the player
     * downloads instead; the engine sets the prefix up again either way. The game folder for
     * Import; null when the older installation had none here. Each step can stop and run again: a
     * mark ([SETTING_ASIDE]) comes first and goes last, and while it is there the next start
     * finishes the set-aside ([settingAside], [AppRuntime.updatePrefix]) before anything else.
     */
    fun setAside(kept: File): File? {
        val mark = File(root, SETTING_ASIDE)
        if (!mark.exists()) writeDurably(mark, "${System.currentTimeMillis() / 1000}\n")
        val recorded = recordedGame()
        val managed = recorded == null
        val game = recorded ?: listOf(File(prefix, GAME), File(old, GAME))
            .firstOrNull { Files.isDirectory(it.toPath(), LinkOption.NOFOLLOW_LINKS) }
        // By their canonical paths: the engine may name the data folder by another link
        // (/data/data against /data/user/0).
        val imports = kept.parentFile?.parentFile?.canonicalFile
        if (game != null && game.canonicalFile != kept.canonicalFile) {
            if (managed || game.canonicalFile.parentFile?.parentFile == imports) {
                moveDurably(game, kept)
                // An imported copy's own token folder, empty now.
                if (!managed) runCatching { Files.delete(game.parentFile!!.toPath()) }
            } else {
                log("update in place failed: the older game folder ${game.path} is outside the app; Import can pick it")
            }
        }
        val found = kept.takeIf { Files.isDirectory(it.toPath(), LinkOption.NOFOLLOW_LINKS) }
        if (found != null) unlinkSave(found)
        if (prefix.isDirectory) {
            when {
                ours(prefix, template) -> forgetRecords()
                !old.exists() -> {
                    Files.move(prefix.toPath(), old.toPath(), StandardCopyOption.ATOMIC_MOVE)
                    syncDirectory(root.toPath())
                }
                // A copy of another template generation in place while the older prefix waits (a
                // stop, then a newer runtime): it forgets the game as well, so the engine offers
                // Import, not a damaged installation.
                stamp(prefix) == "disable" -> forgetRecords()
            }
        }
        UserlandInstaller.deleteTree(partial.toPath())
        UserlandInstaller.deleteTree(migrating.toPath())
        syncDirectory(root.toPath())
        // From here on [migrate] leaves the older prefix.
        if (old.isDirectory) {
            File(old, SET_ASIDE).createNewFile()
            syncDirectory(old.toPath())
        }
        Files.deleteIfExists(mark.toPath())
        syncDirectory(root.toPath())
        log("update in place failed: the older game folder waits for Import at ${found ?: "none"}")
        return found
    }

    /**
     * The older prefix goes ([migrate]'s end, and once the installation is Ready again after
     * [setAside]): any characters left in it first ([recoverCharacters]); a failure of that keeps
     * it. Never while it still holds the managed game folder and no [setAside] kept it (a move not
     * finished yet, [unfinished]): it stays, and the next start finishes the move. One a set-aside
     * kept goes whole. Whether it went.
     */
    fun removeOld(): Boolean {
        if (!moved()) return true
        if (!File(old, SET_ASIDE).exists() && Files.isDirectory(File(old, GAME).toPath(), LinkOption.NOFOLLOW_LINKS)) {
            log("the older prefix still holds the game folder; it stays until the move is finished")
            return false
        }
        recoverCharacters()
        UserlandInstaller.deleteTree(old.toPath())
        return true
    }

    /** The prefix in place without the game folder's records and the game's registry keys. */
    private fun forgetRecords() {
        for (record in listOf(POINTER, IMPORT_RECEIPT, "dosdevices/g:")) {
            Files.deleteIfExists(File(prefix, record).toPath())
        }
        File(prefix, "dosdevices").takeIf { it.isDirectory }?.let { syncDirectory(it.toPath()) }
        syncDirectory(prefix.toPath())
        forgetGame()
    }

    /**
     * After an update in place whose moved game the engine rejects ([UPDATED], Repair's first
     * choice): the prefix forgets the game ([forgetGame]), and the managed game folder goes from
     * it to [kept] by a rename, with a copy of the player's characters in place of its `Save` link
     * ([unlinkSave]), for Import to take it. A mark ([TAKING_OUT]) comes first and goes only with
     * [tookOut], once the app recorded the Import: should a stop come between, the next start
     * finds it ([takingOut]) and finishes. The folder; null when the prefix holds none.
     */
    fun takeOut(kept: File): File? {
        val mark = File(root, TAKING_OUT)
        if (!mark.exists()) writeDurably(mark, "${System.currentTimeMillis() / 1000}\n")
        forgetGame()
        val game = File(prefix, GAME)
        if (Files.isDirectory(game.toPath(), LinkOption.NOFOLLOW_LINKS) && !kept.exists()) moveDurably(game, kept)
        if (!Files.isDirectory(kept.toPath(), LinkOption.NOFOLLOW_LINKS)) return null
        unlinkSave(kept)
        log("the engine rejected the updated game: it waits for Import at $kept")
        return kept
    }

    /** Whether a [takeOut] is to be finished: a stop came before its [tookOut]. */
    fun takingOut(): Boolean = File(root, TAKING_OUT).exists()

    /** [takeOut]'s end, once the app recorded that Install imports the folder: its mark goes. */
    fun tookOut() {
        Files.deleteIfExists(File(root, TAKING_OUT).toPath())
        syncDirectory(root.toPath())
    }

    /**
     * The `Save` of the game folder [game] for Import, a link to the player's [userSave]: Import
     * takes no folder with links, so a `Save` folder with a copy of what the player's folder holds
     * takes the link's place, and the game finds its characters at once, as in any imported copy
     * (the engine names a game folder used in place only from its next start, so the first
     * launches link nothing). The player's folder stays as it is; the next link merges the copy
     * back ([SaveKeeper.link]: the same file is not kept twice, and of two others the one saved
     * last stays). Links and other special files are not copied. The copy is made beside [game]'s
     * `Save` ([SAVE_COPY]) and renamed into its place once the link is gone, so a stop part way
     * is finished by the next run.
     */
    private fun unlinkSave(game: File) {
        val save = File(game, "Save").toPath()
        val copy = File(game, SAVE_COPY).toPath()
        if (Files.isSymbolicLink(save)) {
            UserlandInstaller.deleteTree(copy)
            if (Files.isDirectory(userSave.toPath(), LinkOption.NOFOLLOW_LINKS)) {
                copySaves(userSave.toPath(), copy)
                syncTree(copy)
            }
            Files.delete(save)
            syncDirectory(game.toPath())
        }
        if (Files.isDirectory(copy, LinkOption.NOFOLLOW_LINKS) && !Files.exists(save, LinkOption.NOFOLLOW_LINKS)) {
            Files.move(copy, save, StandardCopyOption.ATOMIC_MOVE)
            syncDirectory(game.toPath())
        }
        UserlandInstaller.deleteTree(copy)
    }

    /**
     * The new prefix without the game's registry keys (Blizzard Entertainment's, which [makeNew]
     * moved over), once the game folder is to leave it: keys that name a game folder no longer
     * there are a damaged installation to the engine, not a missing one, so it would not start
     * the Import. Import writes the keys of the folder it takes. Each hive is replaced whole
     * ([writeDurably]).
     */
    private fun forgetGame() {
        for (hive in listOf("system.reg", "user.reg")) {
            val file = File(prefix, hive)
            if (!file.isFile) continue
            val text = file.readText()
            val without = withoutKeys(text)
            if (without != text) writeDurably(file, without)
        }
    }

    /**
     * Characters left in the older prefix before it goes (a registry `Save Path` inside it, the
     * Saved Games folder): every regular file of a folder that holds a `.d2s` (the characters'
     * own files, a shared stash too) moves into a new folder below the player's [userSave],
     * `recovered-<time>/<n>` for the n-th folder they came from, never over a file. The `.d2s`
     * files go last, so a stop part way leaves a folder the next run finds again. Links are never
     * followed. The moves reach the disk before this returns. The files moved.
     */
    fun recoverCharacters(time: Long = System.currentTimeMillis()): List<File> {
        if (!Files.isDirectory(old.toPath(), LinkOption.NOFOLLOW_LINKS)) return emptyList()
        val folders = ArrayList<Path>()
        characterFolders(old.toPath(), folders)
        val moved = ArrayList<File>()
        val recovered = File(userSave, "recovered-$time")
        for ((index, folder) in folders.sorted().withIndex()) {
            val entries = folder.toFile().listFiles()
                ?.filter { Files.isRegularFile(it.toPath(), LinkOption.NOFOLLOW_LINKS) } ?: continue
            val target = File(recovered, "${index + 1}")
            // A file leaves a folder Wine made read-only only once the folder is writable.
            if (!Files.isWritable(folder)) Files.setPosixFilePermissions(folder, PosixFilePermissions.fromString("rwx------"))
            ownerOnly(target)
            for (file in entries.sortedBy { it.name.lowercase().endsWith(".d2s") }) {
                // No replace: a file already there stays, and this one is not moved.
                Files.move(file.toPath(), File(target, file.name).toPath())
                moved += File(target, file.name)
            }
            syncDirectory(target.toPath())
        }
        if (moved.isNotEmpty()) {
            syncDirectory(recovered.toPath())
            syncDirectory(userSave.toPath())
            userSave.parentFile?.let { syncDirectory(it.toPath()) }
            log("older prefix: ${moved.size} files of characters kept in $recovered")
        }
        return moved
    }

    /** The folders below [dir] (itself included) that hold a `.d2s`, into [into]; links are not followed. */
    private fun characterFolders(dir: Path, into: MutableList<Path>) {
        // A folder Wine made read-only, or one without search permission, could hold characters too.
        if (!Files.isReadable(dir) || !Files.isExecutable(dir)) {
            Files.setPosixFilePermissions(dir, PosixFilePermissions.fromString("rwx------"))
        }
        var holds = false
        Files.newDirectoryStream(dir).use { entries ->
            for (entry in entries) {
                if (Files.isDirectory(entry, LinkOption.NOFOLLOW_LINKS)) {
                    characterFolders(entry, into)
                } else if (Files.isRegularFile(entry, LinkOption.NOFOLLOW_LINKS) && entry.fileName.toString().lowercase().endsWith(".d2s")) {
                    holds = true
                }
            }
        }
        if (holds) into.add(dir)
    }

    /**
     * The game folder the older installation's record names (the engine's game folder pointer:
     * a folder used in place, such as an imported copy); null: no record (the managed folder), or
     * no such folder.
     */
    private fun recordedGame(): File? = listOf(prefix, old)
        .map { File(it, POINTER) }
        .firstNotNullOfOrNull { recordedPath(it) }
        ?.let(::File)
        ?.takeIf { Files.isDirectory(it.toPath(), LinkOption.NOFOLLOW_LINKS) }

    /** Whether the prefix [dir] holds the game: the managed game folder, or the record of one used in place. */
    private fun holdsGame(dir: File): Boolean =
        Files.isDirectory(File(dir, GAME).toPath(), LinkOption.NOFOLLOW_LINKS) || File(dir, POINTER).isFile

    /** [dir] and its missing parents, owner-only (the app process's umask is 0). */
    private fun ownerOnly(dir: File) {
        if (!dir.isDirectory) {
            Files.createDirectories(dir.toPath(), PosixFilePermissions.asFileAttribute(PosixFilePermissions.fromString("rwx------")))
        }
    }

    /** [from] renamed to [to] (its missing parent folders made owner-only), both names on the disk. */
    private fun moveDurably(from: File, to: File) {
        val parent = to.parentFile!!
        val made = generateSequence(parent) { it.parentFile }.takeWhile { !it.exists() }.toList()
        ownerOnly(parent)
        Files.move(from.toPath(), to.toPath(), StandardCopyOption.ATOMIC_MOVE)
        for (dir in listOf(parent) + made.map { it.parentFile!! }) syncDirectory(dir.toPath())
        syncDirectory(from.parentFile!!.toPath())
    }

    /** [file] with [text], whole or not at all: a synced temporary file renamed over it, its folder synced. */
    private fun writeDurably(file: File, text: String) {
        val next = File(file.parentFile, "${file.name}.pd2-new")
        FileOutputStream(next).use {
            it.write(text.toByteArray())
            it.fd.sync()
        }
        Files.move(next.toPath(), file.toPath(), StandardCopyOption.ATOMIC_MOVE, StandardCopyOption.REPLACE_EXISTING)
        syncDirectory(file.parentFile!!.toPath())
    }

    /** Every regular file below [dir] synced (links are not followed), then every folder, deepest first. */
    private fun syncTree(dir: Path) {
        val folders = ArrayList<Path>()
        Files.walkFileTree(dir, object : SimpleFileVisitor<Path>() {
            override fun preVisitDirectory(folder: Path, attrs: BasicFileAttributes): FileVisitResult {
                // add, not +=: a Path is Iterable, so += is ambiguous.
                folders.add(folder)
                return FileVisitResult.CONTINUE
            }

            override fun visitFile(file: Path, attrs: BasicFileAttributes): FileVisitResult {
                if (attrs.isRegularFile) FileChannel.open(file, StandardOpenOption.READ).use { it.force(true) }
                return FileVisitResult.CONTINUE
            }
        })
        for (folder in folders.asReversed()) syncDirectory(folder)
    }

    /**
     * [MIGRATING]: a copy of the template with the game's registry keys of [from], every file and
     * folder of it on the disk before it can take the older prefix's place. It is made as
     * [PARTIAL] and gets its name only when whole, so a stop part way never leaves a copy that
     * [ours] would take (as the engine makes a prefix, `.prefix.partial`).
     */
    private fun makeNew(from: File) {
        if (!File(template, "system.reg").isFile) throw CorruptUserland("no prefix template at $template")
        UserlandInstaller.deleteTree(partial.toPath())
        UserlandInstaller.deleteTree(migrating.toPath())
        copyTree(template.toPath(), partial.toPath())
        for (hive in listOf("system.reg", "user.reg")) {
            val source = File(from, hive)
            if (!source.isFile) continue
            val target = File(partial, hive)
            target.writeText(mergeKeys(target.readText(), source.readText()))
        }
        // GLX for OpenGL, which the engine sets only in a prefix it makes itself.
        val user = File(partial, "user.reg")
        if (user.isFile) user.writeText(withGlx(user.readText(), System.currentTimeMillis() / 1000))
        syncTree(partial.toPath())
        Files.move(partial.toPath(), migrating.toPath(), StandardCopyOption.ATOMIC_MOVE)
        syncDirectory(root.toPath())
    }

    /** The old prefix's [relative] path into the new prefix, in place of the template's; both names on the disk. */
    private fun moveOver(relative: String) {
        val from = File(old, relative)
        if (!Files.exists(from.toPath(), LinkOption.NOFOLLOW_LINKS)) return
        val to = File(prefix, relative)
        to.parentFile?.mkdirs()
        UserlandInstaller.deleteTree(to.toPath())
        Files.move(from.toPath(), to.toPath(), StandardCopyOption.ATOMIC_MOVE)
        syncDirectory(to.parentFile!!.toPath())
        syncDirectory(from.parentFile!!.toPath())
    }

    companion object {
        const val OLD = ".prefix.old"
        const val MIGRATING = ".prefix.migrating"
        /** [MIGRATING] while it is made ([makeNew]); never moved into place. */
        const val PARTIAL = ".prefix.migrating.partial"
        /** In [OLD]: [setAside] kept it after a failed update; it waits for Ready, and [migrate] leaves it. */
        const val SET_ASIDE = ".pd2-set-aside"
        private const val GAME = "drive_c/Program Files (x86)/Diablo II"
        /** Where Blizzard's downloader writes (the engine's `WRAPPER_DOWNLOADS`). */
        private const val DOWNLOADS = "drive_c/pd2launcher-downloads"
        /** The engine's record of a game folder used in place (`game_folder.rs` `POINTER_NAME`). */
        const val POINTER = ".pd2launcher-game-folder.json"
        /** In the managed root: a [takeOut] that is not finished yet ([takingOut], [tookOut]). */
        const val TAKING_OUT = ".pd2-taking-out"
        /** In the managed root: a [setAside] that is not finished yet ([settingAside]). */
        const val SETTING_ASIDE = ".pd2-setting-aside"
        /** The engine's record of an imported game (`game_folder.rs` `IMPORT_RECEIPT_NAME`). */
        private const val IMPORT_RECEIPT = ".pd2launcher-imported-game.json"
        /**
         * In the managed root: an update in place moved the game over and the installation was not
         * Ready since ([UpgradeImport.offersImport]); it goes at Ready ([UpgradeImport.cleanUp]).
         */
        const val UPDATED = ".pd2-updated-in-place"
        /** In a game folder for Import: the copy of the player's characters on its way to `Save` ([unlinkSave]). */
        private const val SAVE_COPY = ".pd2-save-copy"
        private const val KEYS = "blizzard entertainment"
        private const val X11_DRIVER = "[software\\\\wine\\\\x11 driver]"
        private const val GLX = "\"UseEGL\"=\"N\""

        /**
         * The registry file [userReg] with Wine's X11 driver on GLX: `"UseEGL"="N"` in its
         * `[Software\\Wine\\X11 Driver]` key (after the key's `#` lines, in place of another
         * `UseEGL` value), the key added with the time [time] where it is missing. Wine 11 takes
         * EGL by default, which drew about a quarter fewer frames on a Mali at 120 Hz.
         */
        fun withGlx(userReg: String, time: Long): String {
            val lines = userReg.split('\n').toMutableList()
            val header = lines.indexOfFirst { it.lowercase().startsWith(X11_DRIVER) }
            if (header < 0) return userReg.trimEnd('\n') + "\n\n[Software\\\\Wine\\\\X11 Driver] $time\n$GLX\n"
            var end = header + 1
            while (end < lines.size && !lines[end].startsWith("[")) end++
            val key = lines.subList(header + 1, end)
            key.removeAll { it.lowercase().startsWith("\"useegl\"=") }
            key.add(key.indexOfFirst { !it.startsWith("#") }.takeIf { it >= 0 } ?: key.size, GLX)
            return lines.joinToString("\n")
        }

        /**
         * In a template, and so in each prefix made from it: the template's generation, a number
         * (runtime/prefix.sh writes config.sh's PREFIX_GENERATION). It is raised by hand when
         * existing prefixes must get a change of the template (another Wine, other registry values).
         */
        const val GENERATION = ".pd2-prefix-generation"

        /**
         * Whether [prefix] was made from the current [template]: its `.update-timestamp` is
         * "disable", as in every copy of a template (Wine never updates it by itself), and its
         * [generation] is the template's. A stamp or a mark that cannot be read is no match, so
         * such a prefix moves, once: the new one has the template's.
         */
        fun ours(prefix: File, template: File): Boolean =
            stamp(prefix) == "disable" && generation(prefix) == generation(template)

        /** The text of [prefix]'s `.update-timestamp`, trimmed; null when it is not there or cannot be read. */
        private fun stamp(prefix: File): String? =
            runCatching { File(prefix, ".update-timestamp").takeIf { it.isFile && it.length() < 64 }?.readText() }
                .getOrNull()?.trim()

        /**
         * The template generation of [dir], a template or a prefix made from one: the whole number in
         * its [GENERATION]; 1 when there is no such file (the templates made before the mark); null
         * when it holds no whole number, or is a folder, a file of 64 bytes or more, or a file that
         * cannot be read.
         */
        fun generation(dir: File): Int? {
            val mark = File(dir, GENERATION)
            if (!mark.exists()) return 1
            return runCatching { mark.takeIf { it.isFile && it.length() < 64 }?.readText() }.getOrNull()?.trim()?.toIntOrNull()
        }

        /** The folder the engine's game folder record [record] ([POINTER]) names; null: none, or it cannot be read. */
        fun recordedPath(record: File): String? = record.takeIf { it.isFile && it.length() < 64 * 1024 }?.let {
            runCatching { Json.parseToJsonElement(it.readText()).jsonObject["path"]?.jsonPrimitive?.contentOrNull }.getOrNull()
        }

        /**
         * The registry file [target] with its Blizzard Entertainment keys (and their subkeys)
         * replaced by [source]'s. A key is a `[path] time` line and the lines up to the next key.
         */
        fun mergeKeys(target: String, source: String): String {
            val kept = blocks(target).filterNot(::gameKey)
            val moved = blocks(source).filter(::gameKey)
            if (moved.isEmpty()) return target
            val head = kept.joinToString("").trimEnd('\n') + "\n\n"
            return head + moved.joinToString("") { it.trimEnd('\n') + "\n\n" }.trimEnd('\n') + "\n"
        }

        /** The registry file [registry] without its Blizzard Entertainment keys (and their subkeys). */
        fun withoutKeys(registry: String): String {
            val all = blocks(registry)
            val kept = all.filterNot(::gameKey)
            if (kept.size == all.size) return registry
            return kept.joinToString("").trimEnd('\n') + "\n"
        }

        /** A registry file's text as its keys, each with the lines up to the next; the head first. */
        private fun blocks(text: String): List<String> {
            val out = ArrayList<String>()
            val current = StringBuilder()
            for (line in text.split('\n')) {
                if (line.startsWith("[") && current.isNotEmpty()) {
                    out += current.toString()
                    current.clear()
                }
                current.append(line).append('\n')
            }
            if (current.isNotEmpty()) out += current.toString().removeSuffix("\n")
            return out
        }

        private fun gameKey(block: String) = block.startsWith("[") &&
            block.substringBefore(']').lowercase().contains(KEYS)

        /**
         * [from]'s folders (owner-only) and regular files copied to [to] (which must not exist),
         * each file with its times, so a copied character is not taken for one saved since; links
         * are not followed and, as other special files, not copied. The backups' folder at the top
         * ([SaveKeeper.BACKUPS]) is not copied: it stays in the player's folder, and the next link
         * would only bring the copies back as second backups.
         */
        fun copySaves(from: Path, to: Path) {
            val ownerOnly = PosixFilePermissions.asFileAttribute(PosixFilePermissions.fromString("rwx------"))
            Files.walkFileTree(from, object : SimpleFileVisitor<Path>() {
                override fun preVisitDirectory(dir: Path, attrs: BasicFileAttributes): FileVisitResult {
                    if (dir.parent == from && dir.fileName.toString() == SaveKeeper.BACKUPS) return FileVisitResult.SKIP_SUBTREE
                    Files.createDirectory(to.resolve(from.relativize(dir).toString()), ownerOnly)
                    return FileVisitResult.CONTINUE
                }

                override fun visitFile(file: Path, attrs: BasicFileAttributes): FileVisitResult {
                    if (attrs.isRegularFile) {
                        Files.copy(file, to.resolve(from.relativize(file).toString()), StandardCopyOption.COPY_ATTRIBUTES, LinkOption.NOFOLLOW_LINKS)
                    }
                    return FileVisitResult.CONTINUE
                }
            })
        }

        /** [from] copied to [to] (which must not exist): folders, files, and links as they are. */
        fun copyTree(from: Path, to: Path) {
            Files.walkFileTree(from, object : SimpleFileVisitor<Path>() {
                override fun preVisitDirectory(dir: Path, attrs: BasicFileAttributes): FileVisitResult {
                    Files.createDirectory(to.resolve(from.relativize(dir).toString()))
                    return FileVisitResult.CONTINUE
                }

                override fun visitFile(file: Path, attrs: BasicFileAttributes): FileVisitResult {
                    val target = to.resolve(from.relativize(file).toString())
                    if (attrs.isSymbolicLink) {
                        Files.createSymbolicLink(target, Files.readSymbolicLink(file))
                    } else {
                        Files.copy(file, target, LinkOption.NOFOLLOW_LINKS)
                        if (Files.isExecutable(file)) target.toFile().setExecutable(true, true)
                    }
                    return FileVisitResult.CONTINUE
                }
            })
        }
    }
}
