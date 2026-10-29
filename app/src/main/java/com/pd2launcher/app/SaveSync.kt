package com.pd2launcher.app

import java.io.BufferedOutputStream
import java.io.File
import java.io.FileInputStream
import java.io.FileOutputStream
import java.io.IOException
import java.io.InputStream
import java.io.OutputStream
import java.nio.file.Files
import java.nio.file.LinkOption
import java.nio.file.StandardCopyOption
import java.nio.file.attribute.PosixFilePermissions
import java.security.DigestInputStream
import java.security.MessageDigest
import java.util.Locale
import java.util.UUID
import java.util.concurrent.Callable
import java.util.concurrent.ExecutionException
import java.util.concurrent.Executors

/**
 * One side of a save sync ([SaveSync]): a folder of regular files, each named by its path
 * below the folder's top with `/` between the parts (`Hero.d2s`, `ProjectD2/Hero.key`). [FileTree]
 * is a folder of the app's own storage (the player's Save; both sides in the tests); the folder
 * the player picked is reached through Android's document provider ([DocumentTree]). Another way
 * to the same files (a built-in Syncthing, say) is one more implementation: the plan
 * ([SaveSync.plan]) knows only the paths, hashes and times a tree gives.
 */
interface SaveTree {
    /** One file of [files]: its path and its time (milliseconds since 1970; 0 when the tree does not know it). */
    data class Entry(val path: String, val time: Long)

    /**
     * The files a sync takes: each name on the way [SaveSync.taken], at most [SaveSync.MAX_DEPTH]
     * folders deep, links not followed. Throws when the tree cannot be read.
     */
    fun files(): List<Entry>

    /** The content of the file [path], or null when there is no such file now. Called from several threads at a time ([SaveSync.scan]). */
    fun open(path: String): InputStream?

    /**
     * Writes the file [path] from [content], its folders made as needed: first a temporary file
     * beside it ([SaveSync.TEMPORARY]), which then takes the name, so a stop part way leaves the
     * old file or the new one whole (and a temporary file for [clean]). [time]: the new file's
     * time, where the tree can set it. [over]: the content hash the file there has now (null: no
     * file); when another app changed it meanwhile, the write throws [SaveSync.ChangedMeanwhile]
     * and leaves it. When [content] throws, [path] stays as it was.
     */
    fun write(path: String, time: Long, over: String?, content: (OutputStream) -> Unit)

    /**
     * Removes the file [path], whose content hash is [hash]; nothing when it is not there. When
     * another app changed it meanwhile, it stays and this throws [SaveSync.ChangedMeanwhile].
     */
    fun delete(path: String, hash: String)

    /** Removes what a [write] left when it stopped part way (the [SaveSync.TEMPORARY] files). */
    fun clean()
}

/**
 * Save sync (issue #8): the player's Save ([SessionPaths.userSave], the app side) and a
 * folder the player picked (the folder side), kept the same in both directions while the game,
 * a launch, the official PD2 launcher and every job are not running. Every file of the Save is
 * synced: each character as one group (the files that share a name up to the first dot with a
 * `.d2s`, as [SaveKeeper] groups them), and every other file on its own (the shared stash, the
 * files of the folders inside Save such as PD2's `ProjectD2`). A sync leaves out hidden files and
 * folders, links, and Syncthing's own files ([taken]).
 *
 * A record ([Record]) holds the content hash (SHA-256) of each file at the last sync (below
 * `Backups`, 64 zeros in place of a hash, [UNREAD], or the old hash from an earlier build: there
 * the record only says that the backup went to the folder); file times are never compared to see
 * a change (Android's document picker cannot set them, and FAT keeps
 * them in 2-second steps). For each path of a group: same content on both sides, nothing; else
 * the side whose content is still the record's did not change it, and the other side did; a path
 * changed on both sides is a conflict. A group then goes as one ([plan]):
 * - changed on one side only: that side's files go to the other side; a file it no longer has is
 *   deleted on the other side after a copy goes to a backup (owner, 2026-10-27: "Delete it too,
 *   with a backup");
 * - changed on both sides: when one side has none of the group's files, the other side's stay
 *   (a change is never lost to a delete); else the one saved last stays ([SaveKeeper.savedAt],
 *   by its own time; on a tie the app's) and the other side's files all go to a backup first;
 * - no record yet (the first sync with a folder): every path counts as changed where it is, so
 *   the sync is a merge: what one side has goes to the other, nothing is deleted, and two
 *   different copies keep the one saved last, the other in a backup, as [SaveKeeper] does.
 * The backups go to one `backup-<time>` folder of [SessionPaths.saveBackups] for the sync,
 * laid out as the Save is ([SaveKeeper.backupFolder]). That is the Save's own `Backups` folder
 * (owner, 2026-10-27), so a second pass of the same sync takes a new backup to the folder side too.
 *
 * `Backups` is synced by its own rules ([backupGroup]; owner, 2026-10-27 and 2026-10-29): never
 * as saves, never backed up again, and from the app to the folder only, one time. Each backup
 * folder in it (`Backups/backup-<time>`) is one group, its files each decided on their own: a
 * file of the app that the record does not name goes to the folder, unless the folder has a file
 * there (which stays as it is), and then the record names it. After that, the sync does not touch
 * the path again on either side: a change or a delete on one side stays on that side. Nothing
 * goes from the folder's `Backups` into the app, and no backup file is read to compare it. Backup
 * folders are not counted with the characters and files that came in or went out.
 *
 * Each write goes through a temporary file ([SaveTree.write]), and a character's own `.d2s` is
 * written last: a stop part way leaves whole files, a group either side of it still has its
 * `.d2s` as before, and the next sync finishes the group by the same rules. The record is
 * written after each group that changed something; before a write or a delete, the file there
 * must still have the content the plan saw, so a file another app changed meanwhile is left for
 * the next sync.
 */
object SaveSync {
    /** How deep a folder inside the Save is synced, as [SaveKeeper] keeps them. */
    const val MAX_DEPTH = 16

    /** The start of the name of [SaveTree.write]'s temporary files: hidden, so no sync takes one. */
    const val TEMPORARY = ".pd2sync-"

    /**
     * Whether a sync takes a file or folder of this [name]: not hidden (a dot first: Syncthing's
     * `.stfolder`, `.stignore`, `.stversions` and `.syncthing.*.tmp`, and [TEMPORARY] files), not
     * a Syncthing conflict copy (`*.sync-conflict-*`) or Syncthing's temporary file on Windows
     * (`~syncthing~*.tmp`), and not a name no folder of both sides can hold (empty, `..`, or with
     * `/`, `\` or a control character).
     */
    fun taken(name: String): Boolean =
        name.isNotEmpty() && !name.startsWith(".") && !name.contains(".sync-conflict-") &&
            !(name.startsWith("~syncthing~") && name.endsWith(".tmp")) &&
            name.none { it == '/' || it == '\\' || it.code < 0x20 || it.code == 0x7f }

    enum class Side {
        APP, FOLDER;

        val other: Side get() = if (this == APP) FOLDER else APP
    }

    /**
     * A file of one side as the plan sees it: its content's SHA-256 (hex) and its time. Below
     * `Backups` the hash is [UNREAD], never a real hash, so no code may compare the hashes of
     * backups.
     */
    data class FileState(val hash: String, val time: Long)

    /** Why a group goes as it goes, for the logs. */
    enum class Why { SAME, NEW, CHANGED, DELETED, CHANGED_AND_DELETED, BOTH_CHANGED, BACKUPS }

    /** One write of a plan, in its group's order. */
    sealed interface Step {
        val path: String

        /** A copy of [path] of [side] (content [hash], time [time]) into the sync's backup folder, before it is replaced or deleted. */
        data class Backup(override val path: String, val side: Side, val hash: String, val time: Long) : Step

        /** [path] of [side], which has the content [hash] (checked first), is removed. */
        data class Delete(override val path: String, val side: Side, val hash: String) : Step

        /**
         * [path] of [from] (content [hash], time [time]) goes to the other side, which has the
         * content [over] there now (null: no file; checked first).
         */
        data class Copy(override val path: String, val from: Side, val hash: String, val time: Long, val over: String?) : Step
    }

    /**
     * A character ([character]: its name, [name]) or one other file ([name]: its path), or a
     * backup folder ([backups]: `Backups/<its name>`), with its [paths] on either side or in the
     * record; [to]: the side its [steps] change (null: none);
     * [after]: the content of each of its paths on both sides once the steps are done, which
     * the record then holds.
     */
    data class Group(
        val name: String,
        val character: Boolean,
        val paths: List<String>,
        val why: Why,
        val to: Side?,
        val steps: List<Step>,
        val after: Map<String, String>,
        val backups: Boolean = false,
    )

    data class Plan(val groups: List<Group>) {
        val changes: List<Group> get() = groups.filter { it.steps.isNotEmpty() }
    }

    /**
     * The content hash of each file at the last sync with the picked [folder] (its document
     * tree's URI, or a folder's path in the tests), by path. A path below `Backups` holds [UNREAD]
     * (64 zeros) in place of a hash, or its old hash from an earlier build: the record only says
     * that the backup went to the folder ([backupGroup]). A record of another folder is none: the
     * first sync with a folder is a merge.
     */
    data class Record(val folder: String, val hashes: Map<String, String>)

    /**
     * What [sync] did: [outcome], whether it was a merge ([first]: the first sync with the folder,
     * or [emptied]: one side had no file at all), and how many paths both sides have after it
     * (the backups not counted: the record does not say that both sides have one).
     */
    data class Result(val outcome: Outcome, val first: Boolean, val emptied: Boolean, val files: Int)

    /**
     * One sync of [app] and [folder] (whose record names it [folderId]): what an earlier sync
     * left part way goes ([SaveTree.clean]), both sides are read and hashed ([scan]; the backups
     * are only listed), the record in [recordFile] is read ([readRecord]; none: the first sync),
     * and the [plan] is done ([apply]); the record is written as [apply] goes. A side with no file
     * at all while the record has some is a merge too ([Result.emptied]): a folder made again, or
     * another SD card at the same place, is far more likely than the player deleting every
     * character, so nothing is deleted then. When the sync made a backup (in the app's
     * `Save/Backups`), a second pass takes it to the folder side; the outcome counts both.
     */
    fun sync(
        app: SaveTree,
        folder: SaveTree,
        folderId: String,
        recordFile: File,
        backups: File,
        now: Long,
        detail: (String) -> Unit = {},
    ): Result {
        val first = pass(app, folder, folderId, recordFile, backups, now, detail)
        if (first.outcome.backup == null) return first
        val again = pass(app, folder, folderId, recordFile, backups, now, detail, onlyBackups = true)
        val one = first.outcome
        val second = again.outcome
        return first.copy(
            outcome = Outcome(
                one.toApp + second.toApp, one.toFolder + second.toFolder, one.backedUp + second.backedUp, one.backup,
                one.failed + second.failed, second.record, one.backupFolders + second.backupFolders,
            ),
            files = again.files,
        )
    }

    private fun pass(
        app: SaveTree,
        folder: SaveTree,
        folderId: String,
        recordFile: File,
        backups: File,
        now: Long,
        detail: (String) -> Unit,
        onlyBackups: Boolean = false,
    ): Result {
        app.clean()
        folder.clean()
        val appFiles = scan(app)
        val folderFiles = scan(folder)
        val record = readRecord(recordFile, folderId)
        val emptied = !record?.hashes.isNullOrEmpty() && (appFiles.isEmpty() || folderFiles.isEmpty())
        val hashes = record?.hashes.takeUnless { emptied }
        val plan = plan(appFiles, folderFiles, hashes).let { if (onlyBackups) Plan(it.groups.filter(Group::backups)) else it }
        val outcome = apply(plan, app, folder, hashes.orEmpty(), backups, now, { writeRecord(recordFile, Record(folderId, it)) }, detail)
        // The merge's record replaces the old one, also when both sides are empty.
        if (emptied) writeRecord(recordFile, Record(folderId, outcome.record))
        return Result(outcome, record == null, emptied, outcome.record.keys.count { !it.startsWith(BACKUPS) })
    }

    /**
     * The files of [tree] with their hashes ([SaveTree.files], then each read, [READERS] at a
     * time: each read through a document provider waits mostly for the provider); a file gone
     * meanwhile is left out. A file below `Backups` is not read: its hash is [UNREAD] ([backupGroup]).
     */
    fun scan(tree: SaveTree): Map<String, FileState> {
        val entries = tree.files().filter { backupsName(it.path) }
        val readers = Executors.newFixedThreadPool(READERS)
        try {
            val hashes = readers.invokeAll(
                entries.map { entry -> Callable { if (entry.path.startsWith(BACKUPS)) UNREAD else tree.open(entry.path)?.use(::sha256) } },
            )
            val files = sortedMapOf<String, FileState>()
            for ((entry, hash) in entries.zip(hashes)) {
                val value = try {
                    hash.get()
                } catch (error: ExecutionException) {
                    throw error.cause ?: error
                } ?: continue
                files[entry.path] = FileState(value, entry.time)
            }
            return files
        } finally {
            readers.shutdown()
        }
    }

    /**
     * Whether a sync takes [path] by the name of its top: one named `backups` in any case is the
     * backups' folder only as `Backups/...`; another case of it (a second backups folder on a
     * case-sensitive side, the same folder on a case-insensitive one) and a file by that name
     * (which would keep every backup out of the Save) are left out.
     */
    private fun backupsName(path: String): Boolean {
        val top = path.substringBefore('/')
        return !top.equals(SaveKeeper.BACKUPS, ignoreCase = true) || (top == SaveKeeper.BACKUPS && '/' in path)
    }

    /** How many files [scan] reads at a time. */
    private const val READERS = 4

    /** The plan for the files of both sides, [app] and [folder], against the last sync's [record] (null: none, a merge). */
    fun plan(app: Map<String, FileState>, folder: Map<String, FileState>, record: Map<String, String>?): Plan {
        val paths = (app.keys + folder.keys + record.orEmpty().keys).toSortedSet()
        // A character: the files at the top that share a name up to the first dot with a .d2s,
        // on either side or in the record (one side or the record may have only some of them).
        val characters = paths.filter { '/' !in it }.groupBy(SaveKeeper::characterName)
            .filterValues { files -> files.any(SaveKeeper::isCharacterFile) }.keys
        val groups = paths.groupBy { path ->
            when {
                // A backup folder (or a file at the top of Backups) is one group.
                path.startsWith(BACKUPS) -> GroupKey(BACKUPS + path.removePrefix(BACKUPS).substringBefore('/'), character = false, backups = true)
                '/' !in path && SaveKeeper.characterName(path) in characters -> GroupKey(SaveKeeper.characterName(path), character = true)
                else -> GroupKey(path, character = false)
            }
        }
        return Plan(
            groups.entries.sortedBy { it.key.name }.map { (key, members) ->
                if (key.backups) backupGroup(key.name, members, app, folder, record) else group(key.name, key.character, members, app, folder, record)
            },
        )
    }

    /** The paths below the Save's backups' folder ([SaveKeeper.BACKUPS]), which [backupGroup] syncs. */
    private const val BACKUPS = SaveKeeper.BACKUPS + "/"

    /**
     * The hash of a file below `Backups` in [scan], in a copy's step and in the record: the sync
     * never reads a backup to compare it ([backupGroup]), and a backup path in the record means
     * only that it is done.
     */
    private val UNREAD = "0".repeat(64)

    private data class GroupKey(val name: String, val character: Boolean, val backups: Boolean = false)

    /**
     * A backup folder ([name], `Backups/<its name>`; owner, 2026-10-29): each of its [paths] on
     * its own, never backed up, from the app to the folder one time. A file of the app that the
     * [record] does not name goes to the folder, unless the folder has a file there (which stays
     * as it is, whatever its content); then the record names it. A path that the record names
     * stays as it is on both sides (changed, deleted or back again), and the record forgets it
     * only when neither side has it. A file that only the folder has never goes into the app and
     * is not recorded. Without a record, the same.
     */
    private fun backupGroup(
        name: String,
        paths: List<String>,
        app: Map<String, FileState>,
        folder: Map<String, FileState>,
        record: Map<String, String>?,
    ): Group {
        val steps = mutableListOf<Step>()
        val after = mutableMapOf<String, String>()
        for (path in paths) {
            val was = record?.get(path)
            val file = app[path]
            when {
                was != null -> if (file != null || path in folder) after[path] = was
                file == null -> {}
                path in folder -> after[path] = UNREAD
                else -> {
                    steps += Step.Copy(path, Side.APP, UNREAD, file.time, null)
                    after[path] = UNREAD
                }
            }
        }
        val why = if (steps.isEmpty()) Why.SAME else Why.BACKUPS
        return Group(name, false, paths, why, Side.FOLDER.takeIf { steps.isNotEmpty() }, steps, after, backups = true)
    }

    private fun group(
        name: String,
        character: Boolean,
        paths: List<String>,
        app: Map<String, FileState>,
        folder: Map<String, FileState>,
        record: Map<String, String>?,
    ): Group {
        val a = paths.mapNotNull { path -> app[path]?.let { path to it } }.toMap()
        val f = paths.mapNotNull { path -> folder[path]?.let { path to it } }.toMap()
        val aHashes = a.mapValues { it.value.hash }
        val fHashes = f.mapValues { it.value.hash }
        if (aHashes == fHashes) return Group(name, character, paths, Why.SAME, null, emptyList(), aHashes)
        var appChanged = false
        var folderChanged = false
        var both = false
        for (path in paths) {
            val x = aHashes[path]
            val y = fHashes[path]
            if (x == y) continue
            // Without a record every path counts as changed where it is: a path on one side only
            // goes to the other, two contents of a path are a conflict.
            val was = record?.get(path)
            when {
                y == was -> appChanged = true
                x == was -> folderChanged = true
                else -> both = true
            }
        }
        val conflict = both || (appChanged && folderChanged)
        val to = when {
            !conflict -> if (appChanged) Side.FOLDER else Side.APP
            // A change on one side and none of the group's files on the other: the change stays.
            a.isEmpty() -> Side.APP
            f.isEmpty() -> Side.FOLDER
            // The one saved last stays; on a tie the app's.
            SaveKeeper.bySavedAt.compare(savedAt(name, a), savedAt(name, f)) >= 0 -> Side.FOLDER
            else -> Side.APP
        }
        val source = if (to == Side.APP) f else a
        val target = if (to == Side.APP) a else f
        val backedUp = conflict && source.isNotEmpty() && target.isNotEmpty()
        val steps = mutableListOf<Step>()
        // The copy that does not stay goes to a backup as a whole first.
        if (backedUp) for ((path, state) in target) steps += Step.Backup(path, to, state.hash, state.time)
        for ((path, state) in target) {
            if (path in source) continue
            if (!backedUp) steps += Step.Backup(path, to, state.hash, state.time)
            steps += Step.Delete(path, to, state.hash)
        }
        // The character's own .d2s last: until it is written, the target keeps its own saved-at.
        for (path in source.keys.sortedWith(compareBy({ character && SaveKeeper.isOwnSave(name, it) }, { it }))) {
            val state = source.getValue(path)
            if (target[path]?.hash != state.hash) steps += Step.Copy(path, to.other, state.hash, state.time, target[path]?.hash)
        }
        val why = when {
            conflict && backedUp -> Why.BOTH_CHANGED
            conflict -> Why.CHANGED_AND_DELETED
            source.isEmpty() -> Why.DELETED
            target.isEmpty() -> Why.NEW
            else -> Why.CHANGED
        }
        return Group(name, character, paths, why, to, steps, source.mapValues { it.value.hash })
    }

    private fun savedAt(name: String, files: Map<String, FileState>): Pair<Boolean, Long> =
        SaveKeeper.savedAt(name, files.map { (path, state) -> path to state.time })

    /**
     * What [apply] did: groups to the app and to the folder (characters and other files), files
     * put in [backup], the groups that failed (with why), and the backup folders with files copied
     * to the folder ([backupFolders]).
     */
    data class Outcome(
        val toApp: Int,
        val toFolder: Int,
        val backedUp: Int,
        val backup: File?,
        val failed: List<String>,
        val record: Map<String, String>,
        val backupFolders: Int = 0,
    )

    /** A file changed after the plan read it: its step is not done, and the next sync looks again. */
    class ChangedMeanwhile(path: String) : IOException("$path changed during the sync")

    /**
     * Does the [plan]'s steps, group by group, on [app] and [folder]; the backups go to one
     * `backup-<now>` folder in [backups]. A group stops at its first failure (the rest of it
     * waits for the next sync) and the others go on. [record]: the last sync's hashes, which
     * [saveRecord] gets again after each group that changed something, and once at the end when
     * a group that changed nothing (the same content on both sides) changed them. [detail] gets
     * a line for each file.
     */
    fun apply(
        plan: Plan,
        app: SaveTree,
        folder: SaveTree,
        record: Map<String, String>,
        backups: File,
        now: Long,
        saveRecord: (Map<String, String>) -> Unit,
        detail: (String) -> Unit = {},
    ): Outcome {
        val tree = { side: Side -> if (side == Side.APP) app else folder }
        val hashes = record.toSortedMap()
        val backupPaths = plan.groups.flatMap { group -> group.steps.filterIsInstance<Step.Backup>().map { it.path } }
        val backup = backupPaths.takeIf { it.isNotEmpty() }?.let { SaveKeeper.backupFolder(backups, now, it) }
        var toApp = 0
        var toFolder = 0
        var backedUp = 0
        var backupFolders = 0
        val failed = mutableListOf<String>()
        var unsaved = false
        for (group in plan.groups) {
            if (group.steps.isEmpty()) {
                unsaved = remember(hashes, group) || unsaved
                continue
            }
            try {
                for (step in group.steps) {
                    when (step) {
                        is Step.Backup -> {
                            backUp(tree(step.side), step, backup!!)
                            backedUp++
                            detail("save sync: ${step.path}: the ${side(step.side)}'s copy is in ${backup.name} (${group.why.text})")
                        }
                        is Step.Delete -> {
                            val target = tree(step.side)
                            if (hash(target, step.path) != step.hash) throw ChangedMeanwhile(step.path)
                            target.delete(step.path, step.hash)
                            detail("save sync: ${step.path}: deleted in the ${side(step.side)} (${group.why.text})")
                        }
                        is Step.Copy -> {
                            val target = tree(step.from.other)
                            // Where the plan saw no file, a file there now is not read (a backup of the folder never is).
                            if (step.over == null) {
                                target.open(step.path)?.use { throw ChangedMeanwhile(step.path) }
                            } else if (hash(target, step.path) != step.over) {
                                throw ChangedMeanwhile(step.path)
                            }
                            target.write(step.path, step.time, step.over) { out -> copyChecked(tree(step.from), step.path, step.hash, out) }
                            detail("save sync: ${step.path}: from the ${side(step.from)} to the ${side(step.from.other)} (${group.why.text})")
                        }
                    }
                }
            } catch (error: Exception) {
                failed += "${group.name}: $error"
                detail("save sync: ${group.name}: not synced: $error")
                continue
            }
            when {
                group.backups -> backupFolders++
                group.to == Side.APP -> toApp++
                else -> toFolder++
            }
            remember(hashes, group)
            saveRecord(hashes)
        }
        if (unsaved) saveRecord(hashes)
        return Outcome(toApp, toFolder, backedUp, backup?.takeIf { backedUp > 0 }, failed, hashes, backupFolders)
    }

    /** [hashes] takes [group]'s content after it ([Group.after]); whether that changed them. */
    private fun remember(hashes: MutableMap<String, String>, group: Group): Boolean {
        var changed = false
        for (path in group.paths) {
            val hash = group.after[path]
            changed = changed || hashes[path] != hash
            if (hash != null) hashes[path] = hash else hashes.remove(path)
        }
        return changed
    }

    private val Why.text: String
        get() = when (this) {
            Why.SAME -> "the same"
            Why.NEW -> "new"
            Why.CHANGED -> "changed"
            Why.DELETED -> "deleted on the other side"
            Why.CHANGED_AND_DELETED -> "changed on one side, deleted on the other; the change stays"
            Why.BOTH_CHANGED -> "changed on both sides; the one saved last stays"
            Why.BACKUPS -> "a new backup: to the folder one time, no backup of it"
        }

    private fun side(side: Side) = if (side == Side.APP) "app" else "folder"

    /** A copy of [step]'s file into [backup] at its path, with its time; it must still have the content the plan saw. */
    private fun backUp(tree: SaveTree, step: Step.Backup, backup: File) {
        val target = File(backup, step.path)
        SaveKeeper.ownerOnlyDirs(target.parentFile!!)
        try {
            FileOutputStream(target).use { out ->
                copyChecked(tree, step.path, step.hash, out)
                // On the disk before the copy it keeps is replaced or deleted.
                out.fd.sync()
            }
        } catch (error: Exception) {
            target.delete()
            throw error
        }
        if (step.time > 0) target.setLastModified(step.time)
    }

    /**
     * [path] of [tree] into [out]; throws [ChangedMeanwhile] when it is gone or its content is not
     * [hash] any more ([UNREAD], a backup: not compared).
     */
    private fun copyChecked(tree: SaveTree, path: String, hash: String, out: OutputStream) {
        val digest = MessageDigest.getInstance("SHA-256")
        val input = tree.open(path) ?: throw ChangedMeanwhile(path)
        DigestInputStream(input, digest).use { it.copyTo(out) }
        if (hash != UNREAD && hex(digest.digest()) != hash) throw ChangedMeanwhile(path)
    }

    /** The content hash of [path] of [tree] now; null when there is no such file. */
    private fun hash(tree: SaveTree, path: String): String? = tree.open(path)?.use(::sha256)

    fun sha256(input: InputStream): String {
        val digest = MessageDigest.getInstance("SHA-256")
        val buffer = ByteArray(64 * 1024)
        while (true) {
            val read = input.read(buffer)
            if (read < 0) break
            digest.update(buffer, 0, read)
        }
        return hex(digest.digest())
    }

    private fun hex(bytes: ByteArray): String = bytes.joinToString("") { "%02x".format(Locale.ROOT, it) }

    /** The first line of a record file, with its format's version. */
    private const val RECORD_HEADER = "pd2-launcher save sync record 1"
    private val RECORD_LINE = Regex("^([0-9a-f]{64}) (.+)$")

    /**
     * The record in [file] when it is of [folder]; null when there is none, it is of another
     * folder, or it does not read. Its lines: [RECORD_HEADER], `folder <folder>`, then `<hash> <path>`.
     */
    fun readRecord(file: File, folder: String): Record? {
        val lines = try {
            if (!file.isFile) return null
            file.readLines()
        } catch (_: IOException) {
            return null
        }
        if (lines.getOrNull(0) != RECORD_HEADER || lines.getOrNull(1) != "folder $folder") return null
        val hashes = sortedMapOf<String, String>()
        for (line in lines.drop(2)) {
            val (hash, path) = RECORD_LINE.matchEntire(line)?.destructured ?: return null
            hashes[path] = hash
        }
        return Record(folder, hashes)
    }

    /** [record] into [file] through a temporary file beside it, owner-only. */
    fun writeRecord(file: File, record: Record) {
        file.parentFile?.let(SaveKeeper::ownerOnlyDirs)
        val text = buildString {
            append(RECORD_HEADER).append('\n')
            append("folder ").append(record.folder).append('\n')
            for ((path, hash) in record.hashes.toSortedMap()) append(hash).append(' ').append(path).append('\n')
        }
        // One temporary name for the record: a stop part way leaves no more than one behind.
        val temporary = File(file.parentFile, ".${file.name}.tmp")
        temporary.delete()
        FileTree.writeAtomically(file, 0, temporary, content = { it.write(text.toByteArray()) })
    }
}

/**
 * A folder of the app's own storage as a [SaveTree]: the player's Save, and both sides in the
 * tests. A missing [top] is an empty tree, made at the first write; folders are made owner-only
 * (the app's umask is 0); a write keeps the given time. Links are never followed: a path through
 * a link, or through a file, fails.
 */
class FileTree(private val top: File) : SaveTree {
    override fun files(): List<SaveTree.Entry> {
        val path = top.toPath()
        if (!Files.exists(path, LinkOption.NOFOLLOW_LINKS)) return emptyList()
        if (!Files.isDirectory(path, LinkOption.NOFOLLOW_LINKS)) throw IOException("$top is not a folder")
        val entries = mutableListOf<SaveTree.Entry>()
        walk(top, "", 0) { file, relative ->
            if (Files.isRegularFile(file.toPath(), LinkOption.NOFOLLOW_LINKS)) entries += SaveTree.Entry(relative, file.lastModified())
        }
        return entries
    }

    override fun open(path: String): InputStream? {
        val file = resolve(path) ?: return null
        return if (Files.isRegularFile(file.toPath(), LinkOption.NOFOLLOW_LINKS)) FileInputStream(file) else null
    }

    override fun write(path: String, time: Long, over: String?, content: (OutputStream) -> Unit) {
        val parts = path.split('/')
        var folder = top
        for (part in listOf("") + parts.dropLast(1)) {
            if (part.isNotEmpty()) folder = File(folder, part)
            val at = folder.toPath()
            if (Files.isSymbolicLink(at) || (Files.exists(at, LinkOption.NOFOLLOW_LINKS) && !Files.isDirectory(at, LinkOption.NOFOLLOW_LINKS))) {
                throw IOException("${folder.path} is not a folder")
            }
            SaveKeeper.ownerOnlyDirs(folder)
        }
        val target = File(folder, parts.last())
        writeAtomically(target, time, content = content) {
            // Last look before the rename replaces it.
            if (hashOf(target) != over) throw SaveSync.ChangedMeanwhile(path)
        }
    }

    override fun delete(path: String, hash: String) {
        val file = resolve(path) ?: return
        if (!Files.exists(file.toPath(), LinkOption.NOFOLLOW_LINKS)) return
        if (hashOf(file) != hash) throw SaveSync.ChangedMeanwhile(path)
        Files.delete(file.toPath())
    }

    /** The content hash of the regular file [file]; null when there is none. */
    private fun hashOf(file: File): String? =
        if (Files.isRegularFile(file.toPath(), LinkOption.NOFOLLOW_LINKS)) FileInputStream(file).use(SaveSync::sha256) else null

    override fun clean() {
        if (!Files.isDirectory(top.toPath(), LinkOption.NOFOLLOW_LINKS)) return
        walk(top, "", 0, hidden = true) { file, _ ->
            if (file.name.startsWith(SaveSync.TEMPORARY) && Files.isRegularFile(file.toPath(), LinkOption.NOFOLLOW_LINKS)) file.delete()
        }
    }

    /** [path] below [top] when every folder on the way is a real folder, else null. */
    private fun resolve(path: String): File? {
        var file = top
        for (part in path.split('/')) {
            if (!Files.isDirectory(file.toPath(), LinkOption.NOFOLLOW_LINKS)) return null
            file = File(file, part)
        }
        return file
    }

    /**
     * Every entry below [folder] at [depth] that a sync takes ([SaveSync.taken]; with [hidden], the
     * hidden files too, for [clean]), with its path; into the folders, at most [SaveSync.MAX_DEPTH].
     */
    private fun walk(folder: File, prefix: String, depth: Int, hidden: Boolean = false, each: (File, String) -> Unit) {
        val children = folder.listFiles() ?: throw IOException("cannot read ${folder.path}")
        for (child in children.sortedBy { it.name }) {
            val isFolder = Files.isDirectory(child.toPath(), LinkOption.NOFOLLOW_LINKS)
            if (!SaveSync.taken(child.name) && !(hidden && !isFolder && child.name.startsWith(SaveSync.TEMPORARY))) continue
            if (isFolder) {
                if (depth < SaveSync.MAX_DEPTH) walk(child, "$prefix${child.name}/", depth + 1, hidden, each)
            } else {
                each(child, prefix + child.name)
            }
        }
    }

    companion object {
        /**
         * Writes [file] from [content] through an owner-only [temporary] file beside it, which takes
         * the name once its data is on the disk: a stop part way leaves the old file or the new one.
         * [check] runs just before the rename; when it throws, [file] stays as it was.
         */
        fun writeAtomically(
            file: File,
            time: Long,
            temporary: File = File(file.parentFile, "${SaveSync.TEMPORARY}${UUID.randomUUID()}.tmp"),
            content: (OutputStream) -> Unit,
            check: () -> Unit = {},
        ) {
            Files.createFile(temporary.toPath(), PosixFilePermissions.asFileAttribute(PosixFilePermissions.fromString("rw-------")))
            try {
                FileOutputStream(temporary).use { out ->
                    BufferedOutputStream(out).let { buffered ->
                        content(buffered)
                        buffered.flush()
                    }
                    out.fd.sync()
                }
                if (time > 0) temporary.setLastModified(time)
                check()
                Files.move(temporary.toPath(), file.toPath(), StandardCopyOption.ATOMIC_MOVE, StandardCopyOption.REPLACE_EXISTING)
            } catch (error: Throwable) {
                temporary.delete()
                throw error
            }
        }
    }
}
