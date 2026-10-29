package com.pd2launcher.app

import android.content.ContentResolver
import android.content.Context
import android.net.Uri
import android.provider.DocumentsContract
import android.util.Log
import com.pd2launcher.app.control.Command
import com.pd2launcher.app.control.ExistingProblem
import com.pd2launcher.app.control.Installer
import com.pd2launcher.app.control.Job
import com.pd2launcher.app.control.Snapshot
import java.io.File
import java.io.FileOutputStream
import java.io.IOException
import java.io.InputStream
import java.nio.file.Files
import java.nio.file.attribute.PosixFilePermissions
import java.util.concurrent.Executors
import java.util.concurrent.atomic.AtomicLong
import kotlin.concurrent.thread
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

/**
 * Copies what the player picked with the Storage Access Framework into the app's folder, where
 * the engine can read it: a Diablo II folder into its own [SessionPaths.importedGame]. Each copy
 * replaces the previous ones, all but the one the installation uses in place ([replacedCopies]);
 * the engine gets the folder once the copy is complete. The picked files are only read. A copy
 * that fails, a copy that Close stops ([cancel]), or a folder that is refused, leaves nothing,
 * even when the engine's refusal comes after [VERDICT_TIMEOUT_MS]; a copy that the end of the
 * app's process cut short goes at the next session start ([removePartialCopies]).
 */
object SourceImport {
    private const val TAG = "pd2-source"
    /** Where the last folder the engine got came from ([pickedFolder]): the notice after an import names it. */
    private const val PREFERENCES = "source"
    private const val PICKED_FOLDER = "picked_folder"
    /** The provider the last copied folder came from ([pickedFrom]): the notice's words depend on it. */
    private const val PICKED_FROM = "picked_from"
    /** Android's own provider for shared storage: its document ids are `<volume>:<path>`. */
    const val SHARED_STORAGE = "com.android.externalstorage.documents"
    /** Android's Downloads provider: the folders it gives are on the device's shared storage too. */
    const val DOWNLOADS = "com.android.providers.downloads.documents"
    /** How long a copied folder waits for the engine's check of it. */
    private const val VERDICT_TIMEOUT_MS = 120_000L
    /** How long a copy waits for the Download it stopped to end ([awaitNoJob]). */
    private const val NO_JOB_TIMEOUT_MS = 60_000L
    /** The engine's refusal of a folder while a job runs: the session sends it again later. */
    const val BUSY = "installer-media-locked-while-installing"
    /** The deepest a picked folder may nest, and the most files and folders it may hold ([walk]). */
    const val MAX_DEPTH = 32
    const val MAX_ENTRIES = 200_000
    /**
     * In a copy's token folder from its first file until the copy is complete and has the engine's
     * word ([SourceCopy.Copied]): a token folder that still holds it at a session start is a copy
     * cut short ([partialCopies]).
     */
    const val PARTIAL = ".copying"
    private val worker = Executors.newSingleThreadExecutor { Thread(it, "pd2-source") }
    /** Counts the folder copies started; a copy's late verdict counts only while no newer one started. */
    private val generation = AtomicLong()
    /** The generation of the copy that Close stopped ([cancel]); its thread stops at its next check. */
    private val cancelled = AtomicLong()
    /** Close runs: no copy starts until the next session start ([allowCopies]). With [lock]. */
    private var closed = false
    /** Orders a copy's start ([copyFolder]) and Close's [cancel]. */
    private val lock = Any()
    /** Where the copy that Close stopped writes its end: the session's log ([cancel]). */
    @Volatile private var closeLog: ((String) -> Unit)? = null
    /** The token folder of the copy running now, which [removePartialCopies] keeps. */
    @Volatile private var running: File? = null
    private val _copy = MutableStateFlow<SourceCopy>(SourceCopy.Idle)
    val copy: StateFlow<SourceCopy> = _copy.asStateFlow()

    /** Close stopped the folder copy ([cancel]): its thread ends, and what it copied goes. */
    class CopyCancelled : IOException("the folder copy was stopped")

    /**
     * Close: the folder copy running now stops at its next check (a block it reads, a folder it
     * lists, the wait for the engine's word on it), what it copied goes, and the copy then shows as
     * never started ([SourceCopy.Idle]), so the player can import again later; [log] (the session's
     * log) gets how it ended. No copy starts after it until the next session start
     * ([allowCopies]). Returns whether a copy ran.
     */
    fun cancel(log: (String) -> Unit): Boolean = synchronized(lock) {
        closed = true
        if (_copy.value !is SourceCopy.Copying) return false
        closeLog = log
        cancelled.set(generation.get())
        true
    }

    /** A session starts: copies may start again after a Close ([cancel]). */
    fun allowCopies() {
        synchronized(lock) {
            closed = false
            closeLog = null
        }
    }

    /**
     * How a copy that ended with [error] shows: as never started ([SourceCopy.Idle]) when Close
     * stopped it ([CopyCancelled], or [stopped]: a read that Close's end cut off can fail another
     * way), so the player can import again; else as failed.
     */
    fun endOf(error: Throwable, stopped: Boolean): SourceCopy =
        if (error is CopyCancelled || stopped) SourceCopy.Idle else SourceCopy.Failed(error.message ?: error.javaClass.simpleName)

    /**
     * The copied Diablo II folder the player chose, from its hand-over to the engine until another
     * source is chosen ([chosenAfter]) or the engine refuses it; in this process's memory only.
     * The engine keeps the choice in memory until its Download, so the session gives it to an
     * engine started since ([SessionService]).
     */
    @Volatile
    var chosen: File? = null
        private set

    /** The copy whose acceptance sent the removal of the downloads' files ([acceptedFirst]). */
    @Volatile private var removedFor: File? = null

    /**
     * Whether the engine's [installer] state accepts the [chosen] copy for the first time: the
     * import really starts (the owner's rule of 2026-10-28), so the caller sends the removal of
     * what the Launcher's own downloads made, once for each copy, whichever look sees the
     * acceptance first (the copy's wait, a late one, or one after the folder went again).
     */
    @Synchronized
    fun acceptedFirst(installer: Installer?): Boolean {
        val folder = chosen ?: return false
        if (!removes(installer, folder, removedFor)) return false
        removedFor = folder
        return true
    }

    /** Whether the engine's [installer] state accepts the copy [folder], whose removal was not sent yet ([removedFor]). */
    fun removes(installer: Installer?, folder: File, removedFor: File?): Boolean =
        removedFor != folder && verdictOf(installer, folder) == Verdict.Accepted

    /** [chosen] after the player's [command] ([stillChosen]). */
    fun chosenAfter(command: Command) {
        val folder = chosen ?: return
        if (!stillChosen(command, folder)) chosen = null
    }

    /**
     * Whether the copied [folder] is still the player's choice after [command]: not after Start
     * over, Blizzard's download or another folder.
     */
    fun stillChosen(command: Command, folder: File): Boolean =
        command !is Command.StartOver && !UpgradeImport.declines(command, folder)

    /**
     * Whether the session gives the [chosen] folder to the engine again: no source was sent to the
     * engine in its start ([sent]), the copy is still there ([chosenThere]), and the engine, its
     * check done with nothing running, still needs Diablo II and has not got the folder
     * ([UpgradeImport.selects]).
     */
    fun resends(snapshot: Snapshot, chosen: File?, chosenThere: Boolean, sent: Boolean): Boolean =
        chosen != null && !sent && UpgradeImport.selects(snapshot, chosen, chosenThere, declined = false)

    /**
     * One picked entry: its path below the picked folder, its size, its last change, and how to
     * read it ([open]; null for a folder). A folder is listed too, so an empty one is copied as well.
     */
    class Entry(val relative: String, val size: Long, val modified: Long = 0, val open: (() -> InputStream)?)

    fun copyFolder(context: Context, tree: Uri) {
        val resolver = context.contentResolver
        val app = context.applicationContext
        // Only a newer folder copy ends an older one's wait for its verdict. Counted before the
        // copy shows as running, and with Close's stop in order, so a stop names this copy, or this
        // copy does not start while Close runs.
        val copy = synchronized(lock) {
            if (closed) {
                Log.i(TAG, "folder copy not started: the app closes")
                return
            }
            if (_copy.value is SourceCopy.Copying) return
            generation.incrementAndGet().also { _copy.value = SourceCopy.Copying(0, 0) }
        }
        val stopped = { cancelled.get() == copy }
        val stop = { if (stopped()) throw CopyCancelled() }
        worker.execute {
            val paths = SessionPaths(DeviceCheck.dataDir(app))
            var target: File? = null
            var copied: File? = null
            _copy.value = try {
                val walked = walk(DocumentsContract.getTreeDocumentId(tree)) { parent ->
                    stop()
                    children(resolver, tree, parent)
                }
                val into = paths.importedGame("copy-${System.currentTimeMillis()}")
                val picks = walked.map { (relative, entry) ->
                    val open = if (entry.folder) null else opener(resolver, DocumentsContract.buildDocumentUriUsingTree(tree, entry.id), relative)
                    Entry(ExistingFolder.canonical(relative), if (entry.folder) 0 else entry.size, if (entry.folder) 0 else entry.modified, open)
                }
                val check = ExistingFolder.check(picks.filter { it.open != null }.associate { it.relative to it.size })
                // The folder is copied once and used in place (a Launcher copy checks its own space).
                val total = picks.sumOf { it.size }
                val free = paths.files.usableSpace
                when {
                    check is ExistingFolder.Check.Missing -> {
                        Log.i(TAG, "folder refused before the copy, these files are missing: ${check.files.joinToString(", ")}")
                        SourceCopy.Missing(check.files)
                    }
                    total > free -> SourceCopy.NoSpace(total, free)
                    else -> {
                        // This copy's token folder: all that a failed, stopped or refused copy removes.
                        val replaced = into.parentFile!!
                        stop()
                        // Download runs (the player came back from the key page): the engine takes
                        // no folder while a job runs. Download stops safely, and the copy waits
                        // until no job runs. Nothing is removed yet: the downloads' files go only
                        // once the engine accepts the folder ([acceptedFirst]), so a refused folder
                        // or a failed copy keeps them.
                        beforeCopy(activeJob())?.let { command ->
                            Log.i(TAG, "Download runs: it stops before the copy")
                            SessionService.command(command)
                            awaitNoJob(NO_JOB_TIMEOUT_MS, { !stopped() }, ::activeJob)
                            stop()
                        }
                        // A previous copy used in place may still hold the player's characters: they
                        // move into SessionPaths.userSave, so removing the copies takes none.
                        SaveKeeper.moveImported(paths) { Log.i(TAG, it) }
                        target = replaced
                        copied = into
                        // The engine's word, and while it has none (starting or restarting) the
                        // Launcher's record: without either, the copy in use would go too.
                        val inUse = listOfNotNull(gameFolder(), GameFilesProvider.recordedGameFolder(paths))
                        // Not for a copy that Close stopped: the earlier copies stay.
                        for (earlier in replacedCopies(paths.imports, inUse)) {
                            stop()
                            UserlandInstaller.deleteTree(earlier.toPath())
                        }
                        stop()
                        // Named running before its mark is made: a session start that finds the
                        // mark then keeps this copy (removePartialCopies).
                        running = replaced
                        copyEntries(into, picks, total, stopped) { done -> _copy.value = SourceCopy.Copying(done, total) }
                        Log.i(TAG, "copied ${picks.size} entries (${total shr 20} MB)")
                        stop()
                        SessionService.command(DiabloSource.selectCommand(into.path))
                        // After the command is queued: a look of the session that sees the choice
                        // then comes after it, and does not send it twice.
                        chosen = into
                        check as ExistingFolder.Check.Usable
                        val current = { generation.get() == copy && !stopped() }
                        val verdict = awaitVerdict(into, VERDICT_TIMEOUT_MS, current, ::installer)
                        stop()
                        when (verdict) {
                            is Verdict.Refused -> {
                                Log.i(TAG, "the engine refused the copied folder: ${verdict.problem}")
                                if (chosen == into) chosen = null
                                discard(replaced)
                                SourceCopy.Refused(verdict.problem)
                            }
                            else -> {
                                // The engine took the copy, or has not said yet: the copy is complete
                                // and stays, and the import's notice names where it came from.
                                markComplete(replaced)
                                val picked = folderName(tree.authority, DocumentsContract.getTreeDocumentId(tree), displayName(resolver, tree))
                                app.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE).edit()
                                    .putString(PICKED_FOLDER, picked).putString(PICKED_FROM, tree.authority).apply()
                                if (verdict == null) watchLateVerdict(replaced, into, copy)
                                // A folder picked and accepted: the import really starts (the owner's
                                // rule of 2026-10-28), so what the Launcher's own downloads made goes,
                                // unless the session's look at the acceptance sent it already. The
                                // import's own Download then fetches nothing.
                                if (verdict == Verdict.Accepted && acceptedFirst(installer())) {
                                    SessionService.command(Command.DiscardDownloads)
                                }
                                SourceCopy.Copied(check.projectDiablo)
                            }
                        }
                    }
                }
            } catch (error: Throwable) {
                // Any failure, an Error too (a provider's folders deep enough to exhaust the stack or
                // the memory): on this executor thread it would otherwise end the app.
                val end = endOf(error, stopped())
                val log = closeLog.takeIf { end == SourceCopy.Idle }
                if (log != null) log("the folder copy stopped for Close") else Log.w(TAG, "folder copy failed: $error")
                // Nothing half copied stays behind, and the copy is no longer the player's choice.
                if (copied != null && chosen == copied) chosen = null
                target?.let { token ->
                    try {
                        discard(token)
                        log?.invoke("what the stopped folder copy had copied is gone")
                    } catch (failure: Exception) {
                        Log.w(TAG, "the copy's folder: $failure")
                        log?.invoke("the stopped folder copy was not removed ($failure); the next session start removes it")
                    }
                }
                // Stopped by Close: as if no copy had started, so the player can import again later.
                end
            } finally {
                running = null
            }
        }
    }

    /** Reads the picked file [uri] ([relative] below the picked folder). */
    private fun opener(resolver: ContentResolver, uri: Uri, relative: String): () -> InputStream =
        { resolver.openInputStream(uri) ?: throw IOException("cannot open $relative") }

    /**
     * Removes a copy's [token] folder: what it holds first, then the token folder with its [PARTIAL]
     * mark, so a removal that stops part way leaves the mark for the next session start.
     */
    fun discard(token: File) {
        token.listFiles()?.filter { it.name != PARTIAL }?.forEach { UserlandInstaller.deleteTree(it.toPath()) }
        UserlandInstaller.deleteTree(token.toPath())
    }

    /** The copy in [token] is complete and has the engine's word: its [PARTIAL] mark goes. */
    fun markComplete(token: File) {
        Files.deleteIfExists(File(token, PARTIAL).toPath())
    }

    /**
     * At a session start: removes every copy that the end of the app's process cut short
     * ([partialCopies]), so no half-copied folder is ever used and its space comes back. The copy
     * running now in this process stays.
     */
    fun removePartialCopies(paths: SessionPaths, log: (String) -> Unit) {
        val inUse = listOfNotNull(gameFolder(), GameFilesProvider.recordedGameFolder(paths))
        for (token in partialCopies(paths.imports, inUse) { running }) {
            log("a folder copy that the app's end cut short goes: ${token.name}")
            try {
                discard(token)
            } catch (error: IOException) {
                log("the folder copy ${token.name} was not removed: $error")
            }
        }
    }

    /**
     * The token folders below [imports] that still hold the [PARTIAL] mark: copies cut short. Not
     * the copy running now ([running], read after the folders are listed, as the copy names itself
     * before it makes its mark), and never one that holds a folder of [inUse] or the older game
     * folder a failed update keeps ([UpgradeImport.TOKEN]).
     */
    fun partialCopies(imports: File, inUse: Collection<String>, running: () -> File?): List<File> {
        val marked = imports.listFiles()?.filter { it.name != UpgradeImport.TOKEN && File(it, PARTIAL).exists() } ?: return emptyList()
        val now = running()
        val kept = inUse.mapNotNull { File(it).parentFile }.toSet()
        return marked.filter { it != now && it !in kept }
    }

    private fun installer(): Installer? = SessionService.status.value.snapshot?.installer

    private fun activeJob(): Job? = SessionService.status.value.snapshot?.activeJob

    /**
     * What goes to the engine before a picked folder is copied while [activeJob] runs: Download's
     * safe stop, as the engine takes no folder while a job runs. Never the removal of the
     * downloads' files, which waits for the engine's acceptance of the folder ([acceptedFirst]).
     */
    fun beforeCopy(activeJob: Job?): Command? = if (activeJob == Job.DOWNLOAD) Command.RequestSafeStop else null

    /**
     * Waits up to [timeoutMs] until [activeJob] says no job runs, polled every [pollMs]; false on
     * the timeout or once [current] says the copy stopped.
     */
    fun awaitNoJob(timeoutMs: Long, current: () -> Boolean, activeJob: () -> Job?, pollMs: Long = 200): Boolean {
        val start = System.nanoTime()
        while (current()) {
            if (activeJob() == null) return true
            if ((System.nanoTime() - start) / 1_000_000 >= timeoutMs) return false
            Thread.sleep(pollMs)
        }
        return false
    }

    /** Where the last copied folder is on the device, as the player finds it ("Download/Diablo II"); null: not known. */
    fun pickedFolder(context: Context): String? =
        context.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE).getString(PICKED_FOLDER, null)

    /** The documents provider the last copied folder came from ([SHARED_STORAGE], say); null: not known. */
    fun pickedFrom(context: Context): String? =
        context.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE).getString(PICKED_FROM, null)

    /**
     * A picked folder as the player finds it: its path on shared storage ("primary:Download/Diablo
     * II" is "Download/Diablo II"), else its [displayName] from another provider; null: neither.
     */
    fun folderName(authority: String?, treeDocumentId: String, displayName: String?): String? =
        treeDocumentId.substringAfter(':', "").trim('/').takeIf { authority == SHARED_STORAGE && it.isNotEmpty() }
            ?: displayName?.takeIf { it.isNotBlank() }

    private fun displayName(resolver: ContentResolver, tree: Uri): String? = runCatching {
        val folder = DocumentsContract.buildDocumentUriUsingTree(tree, DocumentsContract.getTreeDocumentId(tree))
        resolver.query(folder, arrayOf(DocumentsContract.Document.COLUMN_DISPLAY_NAME), null, null, null)?.use {
            if (it.moveToFirst()) it.getString(0) else null
        }
    }.getOrNull()

    /** The folder the installation uses in place, as the engine last named it (null: none). */
    private fun gameFolder(): String? = SessionService.status.value.snapshot?.gameFolder?.path

    /**
     * The previous folder copies below [imports] a new copy replaces: every token folder but those
     * holding a folder of [inUse], the folder the installation uses in place as the engine and
     * the Launcher's record name it. Removing that one would leave the installation without its
     * game (the engine's `game-folder-unavailable`) until Start over; it goes with the next copy
     * after Start over. The older game folder a failed update keeps ([UpgradeImport]) stays too:
     * the engine may refuse the new copy; it goes once the installation is Ready.
     */
    fun replacedCopies(imports: File, inUse: Collection<String>): List<File> {
        val kept = inUse.mapNotNull { File(it).parentFile }.toSet()
        return imports.listFiles()?.filter { it !in kept && it.name != UpgradeImport.TOKEN } ?: emptyList()
    }

    /**
     * No verdict in time: the copy stays (the engine's own word is on screen), and a refusal that
     * comes later, while no newer copy started, still removes [copy] (the token folder holding
     * [folder]).
     */
    private fun watchLateVerdict(copy: File, folder: File, started: Long) {
        val current = { generation.get() == started }
        thread(name = "pd2-source-verdict", isDaemon = true) {
            val verdict = awaitVerdict(folder, Long.MAX_VALUE, current, ::installer)
            if (verdict !is Verdict.Refused) return@thread
            worker.execute {
                if (!current()) return@execute
                Log.i(TAG, "the engine refused the copied folder late: ${verdict.problem}")
                if (chosen == folder) chosen = null
                UserlandInstaller.deleteTree(copy.toPath())
                _copy.value = SourceCopy.Refused(verdict.problem)
            }
        }
    }

    /**
     * Copies [entries] ([total] bytes in all) into [target], a game folder in its own token folder,
     * which gets the [PARTIAL] mark first; it keeps it until the copy has the engine's word
     * ([markComplete]), so a copy that the app's end cuts short keeps it. [progress] gets the
     * bytes done so far. [stopped] is asked before each entry and after each block: once it says
     * so, the copy ends with [CopyCancelled], and the caller removes what is there ([discard]).
     */
    fun copyEntries(target: File, entries: List<Entry>, total: Long, stopped: () -> Boolean, progress: (Long) -> Unit) {
        UserlandInstaller.deleteTree(target.toPath())
        val token = target.parentFile ?: throw IOException("no token folder for $target")
        ownerOnlyDirs(token)
        val mark = File(token, PARTIAL)
        if (!mark.exists() && !mark.createNewFile()) throw IOException("cannot mark the copy as running")
        Files.setPosixFilePermissions(mark.toPath(), PosixFilePermissions.fromString("rw-------"))
        ownerOnlyDirs(target)
        val buffer = ByteArray(1 shl 18)
        var done = 0L
        for (entry in entries) {
            if (stopped()) throw CopyCancelled()
            val out = File(target, entry.relative)
            val open = entry.open
            if (open == null) {
                ownerOnlyDirs(out)
                continue
            }
            out.parentFile?.let(::ownerOnlyDirs)
            open().use { input ->
                FileOutputStream(out).use { file ->
                    while (true) {
                        val n = input.read(buffer)
                        if (n < 0) break
                        file.write(buffer, 0, n)
                        done += n
                        progress(done)
                        if (stopped()) throw CopyCancelled()
                    }
                    file.fd.sync()
                }
            }
            if (entry.size > 0 && out.length() != entry.size) throw IOException("${entry.relative} changed while it was copied")
            // The app's umask is 0: the Launcher's own files are private to it.
            Files.setPosixFilePermissions(out.toPath(), PosixFilePermissions.fromString("rw-------"))
            if (entry.modified > 0) out.setLastModified(entry.modified)
        }
    }

    /** [dir] and any missing parents, each owner-only (the app's umask is 0). */
    private fun ownerOnlyDirs(dir: File) {
        if (dir.isDirectory) return
        dir.parentFile?.let(::ownerOnlyDirs)
        if (!dir.mkdir() && !dir.isDirectory) throw IOException("cannot create ${dir.name}")
        Files.setPosixFilePermissions(dir.toPath(), PosixFilePermissions.fromString("rwx------"))
    }

    /** One entry a provider lists in a folder: its document ID, display name, kind, size and last change. */
    data class Listed(val id: String, val name: String?, val folder: Boolean, val size: Long = 0, val modified: Long = 0)

    /** An entry below the picked folder: its path there and what the provider listed. */
    data class Walked(val relative: String, val listed: Listed)

    /**
     * Every entry below folder [root], each folder before what it holds, from [list] (a folder's
     * entries). The provider is another app's code: one whose folders hold each other (a folder
     * listed twice), nest deeper than [MAX_DEPTH] or hold more than [MAX_ENTRIES] entries in all
     * is refused with an [IOException] instead of being walked until the stack or the memory runs out.
     */
    fun walk(root: String, list: (String) -> List<Listed>): List<Walked> {
        val out = ArrayList<Walked>()
        val visited = hashSetOf(root)
        fun descend(parent: String, prefix: String, depth: Int) {
            for (entry in list(parent)) {
                val relative = prefix + plainName(entry.name)
                if (out.size >= MAX_ENTRIES) throw IOException("the folder holds more than $MAX_ENTRIES files and folders")
                out += Walked(relative, entry)
                if (!entry.folder) continue
                if (depth > MAX_DEPTH) throw IOException("folders nested more than $MAX_DEPTH deep: $relative")
                if (!visited.add(entry.id)) throw IOException("a folder holds itself: $relative")
                descend(entry.id, "$relative/", depth + 1)
            }
        }
        descend(root, "", 1)
        return out
    }

    /** What the picked [tree] lists in folder [parent]; at most one more than [MAX_ENTRIES], which [walk] refuses. */
    private fun children(resolver: ContentResolver, tree: Uri, parent: String): List<Listed> {
        val children = DocumentsContract.buildChildDocumentsUriUsingTree(tree, parent)
        val columns = arrayOf(
            DocumentsContract.Document.COLUMN_DOCUMENT_ID,
            DocumentsContract.Document.COLUMN_DISPLAY_NAME,
            DocumentsContract.Document.COLUMN_MIME_TYPE,
            DocumentsContract.Document.COLUMN_SIZE,
            DocumentsContract.Document.COLUMN_LAST_MODIFIED,
        )
        val cursor = resolver.query(children, columns, null, null, null) ?: throw IOException("cannot list the folder")
        return cursor.use {
            buildList {
                while (size <= MAX_ENTRIES && it.moveToNext()) {
                    add(Listed(it.getString(0), it.getString(1), it.getString(2) == DocumentsContract.Document.MIME_TYPE_DIR, it.getLong(3), it.getLong(4)))
                }
            }
        }
    }

    /** The engine's word on a copied folder. */
    sealed interface Verdict {
        data object Accepted : Verdict

        data class Refused(val problem: ExistingProblem) : Verdict
    }

    /**
     * The engine's word on [folder] in its [installer] state, or null while it names another
     * folder or none: each copy has its own path ([SessionPaths.importedGame]), so only the
     * engine's check of this copy can name it.
     */
    fun verdictOf(installer: Installer?, folder: File): Verdict? {
        if (installer !is Installer.Existing || installer.path.trimEnd('/') != folder.path.trimEnd('/')) return null
        return installer.problem?.let { Verdict.Refused(it) } ?: Verdict.Accepted
    }

    /**
     * Waits up to [timeoutMs] for the engine's word on [folder] in [installer]'s state, polled
     * every [pollMs]; null on the timeout or once [current] says a newer copy started.
     */
    fun awaitVerdict(
        folder: File,
        timeoutMs: Long,
        current: () -> Boolean,
        installer: () -> Installer?,
        pollMs: Long = 200,
    ): Verdict? {
        val start = System.nanoTime()
        while (current()) {
            verdictOf(installer(), folder)?.let { return it }
            if ((System.nanoTime() - start) / 1_000_000 >= timeoutMs) return null
            Thread.sleep(pollMs)
        }
        return null
    }

    /** A provider's display name as one path component; anything else is refused. */
    fun plainName(name: String?): String {
        if (name.isNullOrEmpty() || name == "." || name == ".." || '/' in name || '\u0000' in name) {
            throw IOException("unusable file name: $name")
        }
        return name
    }
}
