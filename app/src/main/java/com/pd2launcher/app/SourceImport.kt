package com.pd2launcher.app

import android.content.ContentResolver
import android.content.Context
import android.net.Uri
import android.provider.DocumentsContract
import android.util.Log
import com.pd2launcher.app.control.ExistingProblem
import com.pd2launcher.app.control.Installer
import java.io.File
import java.io.FileOutputStream
import java.io.IOException
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
 * that fails, or a folder that is refused, leaves nothing, even when the engine's refusal comes
 * after [VERDICT_TIMEOUT_MS].
 */
object SourceImport {
    private const val TAG = "pd2-source"
    /** Where the last folder the engine got came from ([pickedFolder]): the notice after an import names it. */
    private const val PREFERENCES = "source"
    private const val PICKED_FOLDER = "picked_folder"
    /** Android's own provider for shared storage: its document ids are `<volume>:<path>`. */
    private const val SHARED_STORAGE = "com.android.externalstorage.documents"
    /** How long a copied folder waits for the engine's check of it. */
    private const val VERDICT_TIMEOUT_MS = 120_000L
    private val worker = Executors.newSingleThreadExecutor { Thread(it, "pd2-source") }
    /** Counts the folder copies started; a copy's late verdict counts only while no newer one started. */
    private val generation = AtomicLong()
    private val _copy = MutableStateFlow<SourceCopy>(SourceCopy.Idle)
    val copy: StateFlow<SourceCopy> = _copy.asStateFlow()

    /**
     * One picked entry: its path below the picked folder, its size, where to read it, and its
     * last change. A folder is listed too, so an empty one is copied as well.
     */
    private data class Pick(val relative: String, val size: Long, val uri: Uri?, val modified: Long = 0)

    fun copyFolder(context: Context, tree: Uri) {
        val resolver = context.contentResolver
        val app = context.applicationContext
        if (_copy.value is SourceCopy.Copying) return
        _copy.value = SourceCopy.Copying(0, 0)
        // Only a newer folder copy ends an older one's wait for its verdict.
        val copy = generation.incrementAndGet()
        worker.execute {
            val paths = SessionPaths(DeviceCheck.dataDir(app))
            var target: File? = null
            _copy.value = try {
                val listed = ArrayList<Pick>()
                walk(resolver, tree, DocumentsContract.getTreeDocumentId(tree), "", listed)
                val into = paths.importedGame("copy-${System.currentTimeMillis()}")
                val picks = listed.map { it.copy(relative = ExistingFolder.canonical(it.relative)) }
                val check = ExistingFolder.check(picks.filter { it.uri != null }.associate { it.relative to it.size })
                // The folder is copied once and used in place (a Launcher copy checks its own space).
                val total = picks.sumOf { it.size }
                val free = paths.files.usableSpace
                when {
                    check is ExistingFolder.Check.Refused -> {
                        Log.i(TAG, "folder refused before the copy: ${check.problem}")
                        SourceCopy.Refused(check.problem)
                    }
                    total > free -> SourceCopy.NoSpace(total, free)
                    else -> {
                        // This copy's token folder: all that a failed or refused copy removes.
                        val replaced = into.parentFile!!
                        // A previous copy used in place may still hold the player's characters: they
                        // move into SessionPaths.userSave, so removing the copies takes none.
                        SaveKeeper.moveImported(paths) { Log.i(TAG, it) }
                        target = replaced
                        for (earlier in replacedCopies(paths.imports, gameFolder())) {
                            UserlandInstaller.deleteTree(earlier.toPath())
                        }
                        copyAll(app.contentResolver, into, picks, total)
                        Log.i(TAG, "copied ${picks.size} entries (${total shr 20} MB)")
                        val picked = folderName(tree.authority, DocumentsContract.getTreeDocumentId(tree), displayName(resolver, tree))
                        app.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE).edit().putString(PICKED_FOLDER, picked).apply()
                        SessionService.command(DiabloSource.selectCommand(into.path))
                        check as ExistingFolder.Check.Usable
                        val current = { generation.get() == copy }
                        when (val verdict = awaitVerdict(into, VERDICT_TIMEOUT_MS, current, ::installer)) {
                            is Verdict.Refused -> {
                                Log.i(TAG, "the engine refused the copied folder: ${verdict.problem}")
                                UserlandInstaller.deleteTree(replaced.toPath())
                                SourceCopy.Refused(verdict.problem)
                            }
                            Verdict.Accepted -> SourceCopy.Copied(check.projectDiablo)
                            null -> {
                                watchLateVerdict(replaced, into, copy)
                                SourceCopy.Copied(check.projectDiablo)
                            }
                        }
                    }
                }
            } catch (error: Exception) {
                Log.w(TAG, "folder copy failed: $error")
                // Nothing half copied stays behind.
                target?.let { runCatching { UserlandInstaller.deleteTree(it.toPath()) } }
                SourceCopy.Failed(error.message ?: error.javaClass.simpleName)
            }
        }
    }

    private fun installer(): Installer? = SessionService.status.value.snapshot?.installer

    /** Where the last copied folder is on the device, as the player finds it ("Download/Diablo II"); null: not known. */
    fun pickedFolder(context: Context): String? =
        context.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE).getString(PICKED_FOLDER, null)

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
     * The previous folder copies below [imports] a new copy replaces: every token folder but the
     * one holding [inUse], the folder the installation uses in place. Removing that one would
     * leave the installation without its game (the engine's `game-folder-unavailable`) until
     * Start over; it goes with the next copy after Start over.
     */
    fun replacedCopies(imports: File, inUse: String?): List<File> {
        val kept = inUse?.let { File(it).parentFile }
        return imports.listFiles()?.filter { it != kept } ?: emptyList()
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
                UserlandInstaller.deleteTree(copy.toPath())
                _copy.value = SourceCopy.Refused(verdict.problem)
            }
        }
    }

    private fun copyAll(resolver: ContentResolver, target: File, picks: List<Pick>, total: Long) {
        UserlandInstaller.deleteTree(target.toPath())
        target.parentFile?.let(::ownerOnlyDirs)
        ownerOnlyDirs(target)
        val buffer = ByteArray(1 shl 18)
        var done = 0L
        for (pick in picks) {
            val out = File(target, pick.relative)
            val uri = pick.uri
            if (uri == null) {
                ownerOnlyDirs(out)
                continue
            }
            out.parentFile?.let(::ownerOnlyDirs)
            val input = resolver.openInputStream(uri) ?: throw IOException("cannot open ${pick.relative}")
            input.use {
                FileOutputStream(out).use { file ->
                    while (true) {
                        val n = input.read(buffer)
                        if (n < 0) break
                        file.write(buffer, 0, n)
                        done += n
                        _copy.value = SourceCopy.Copying(done, total)
                    }
                    file.fd.sync()
                }
            }
            if (pick.size > 0 && out.length() != pick.size) throw IOException("${pick.relative} changed while it was copied")
            // The app's umask is 0: the Launcher's own files are private to it.
            Files.setPosixFilePermissions(out.toPath(), PosixFilePermissions.fromString("rw-------"))
            if (pick.modified > 0) out.setLastModified(pick.modified)
        }
    }

    /** [dir] and any missing parents, each owner-only (the app's umask is 0). */
    private fun ownerOnlyDirs(dir: File) {
        if (dir.isDirectory) return
        dir.parentFile?.let(::ownerOnlyDirs)
        if (!dir.mkdir() && !dir.isDirectory) throw IOException("cannot create ${dir.name}")
        Files.setPosixFilePermissions(dir.toPath(), PosixFilePermissions.fromString("rwx------"))
    }

    private fun walk(resolver: ContentResolver, tree: Uri, parent: String, prefix: String, out: MutableList<Pick>) {
        val children = DocumentsContract.buildChildDocumentsUriUsingTree(tree, parent)
        val columns = arrayOf(
            DocumentsContract.Document.COLUMN_DOCUMENT_ID,
            DocumentsContract.Document.COLUMN_DISPLAY_NAME,
            DocumentsContract.Document.COLUMN_MIME_TYPE,
            DocumentsContract.Document.COLUMN_SIZE,
            DocumentsContract.Document.COLUMN_LAST_MODIFIED,
        )
        val cursor = resolver.query(children, columns, null, null, null) ?: throw IOException("cannot list the folder")
        cursor.use {
            while (it.moveToNext()) {
                val id = it.getString(0)
                val relative = prefix + plainName(it.getString(1))
                if (it.getString(2) == DocumentsContract.Document.MIME_TYPE_DIR) {
                    out += Pick(relative, 0, null)
                    walk(resolver, tree, id, "$relative/", out)
                } else {
                    out += Pick(relative, it.getLong(3), DocumentsContract.buildDocumentUriUsingTree(tree, id), it.getLong(4))
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
