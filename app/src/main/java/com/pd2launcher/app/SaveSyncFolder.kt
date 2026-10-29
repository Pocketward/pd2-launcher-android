package com.pd2launcher.app

import android.content.ContentResolver
import android.content.Context
import android.content.Intent
import android.database.Cursor
import android.net.Uri
import android.os.ParcelFileDescriptor
import android.provider.DocumentsContract
import android.provider.DocumentsContract.Document
import android.util.Log
import java.io.BufferedOutputStream
import java.io.File
import java.io.FileNotFoundException
import java.io.IOException
import java.io.InputStream
import java.io.OutputStream
import java.util.UUID
import java.util.concurrent.ConcurrentHashMap
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update

/**
 * The folder the player picked for the save sync ([SaveSync]) as a [SaveTree]: a document
 * tree that Android's folder picker (ACTION_OPEN_DOCUMENT_TREE) gave with a lasting grant, in
 * shared storage or on an SD card, read and written through its document provider. Blocking:
 * call off the main thread.
 *
 * A write ([write]) is the closest a document provider comes to a temporary file and a rename:
 * the new content goes into a hidden temporary document beside the file and is flushed to the
 * disk; the old file is set aside under a hidden name that keeps its own ([setAside]) and must
 * still have the content the plan saw (else another app, Syncthing say, wrote it meanwhile: it gets
 * its name back and the write fails); the new one takes the name; then the old one is removed. A
 * delete sets the file aside the same way first. A stop part way leaves the old file or the new
 * one under its name, or both under hidden names: the next sync ([clean]) gives a set-aside file
 * its name back when nothing holds it and removes the rest, then writes again from the app's
 * copy. When the name cannot be given back exactly (another file holds it, in another case on a
 * FAT card, say), the old file takes its name again and the write fails. A provider that cannot
 * rename gets no writes at all (an overwrite in place could leave half a file).
 */
class DocumentTree(private val resolver: ContentResolver, private val tree: Uri) : SaveTree {
    private val top = DocumentsContract.getTreeDocumentId(tree)
    /** Document IDs of the folders found so far, by path (`""`: the top); [open] runs on several threads. */
    private val folders = ConcurrentHashMap(mapOf("" to top))
    /**
     * Document IDs of the files [files] found and this tree wrote, by path: [open] reads one by
     * its ID (each look-up by name lists the whole folder, about 0.1 s on a slow phone), and
     * looks it up by name again only when the ID no longer opens or the path is new.
     */
    private val documents = ConcurrentHashMap<String, String>()

    private class Child(val id: String, val name: String, val folder: Boolean, val time: Long)

    /** Whether the picked folder is there now, as a folder (an SD card taken out, a folder removed: no). */
    fun available(): Boolean = try {
        resolver.query(document(top), arrayOf(Document.COLUMN_MIME_TYPE), null, null, null)?.use { cursor ->
            cursor.moveToFirst() && cursor.getString(0) == Document.MIME_TYPE_DIR
        } == true
    } catch (error: Exception) {
        Log.w(TAG, "the sync folder is not there: $error")
        false
    }

    override fun files(): List<SaveTree.Entry> {
        folders.clear()
        folders[""] = top
        documents.clear()
        val entries = mutableListOf<SaveTree.Entry>()
        walk(top, "", 0) { child, path ->
            if (!child.folder) {
                documents[path] = child.id
                entries += SaveTree.Entry(path, child.time)
            }
        }
        return entries
    }

    override fun open(path: String): InputStream? {
        documents[path]?.let { id ->
            try {
                return resolver.openInputStream(document(id))
            } catch (_: FileNotFoundException) {
                documents.remove(path)
            }
        }
        val parent = findFolder(path.substringBeforeLast('/', "")) ?: return null
        val child = child(parent, path.substringAfterLast('/'))?.takeIf { !it.folder } ?: return null
        return try {
            resolver.openInputStream(document(child.id)).also { documents[path] = child.id }
        } catch (_: FileNotFoundException) {
            null
        }
    }

    override fun write(path: String, time: Long, over: String?, content: (OutputStream) -> Unit) {
        val parent = makeFolder(path.substringBeforeLast('/', ""))
        val name = path.substringAfterLast('/')
        val temporary = DocumentsContract.createDocument(resolver, document(parent), BINARY, "${SaveSync.TEMPORARY}${UUID.randomUUID()}.tmp")
            ?: throw IOException("cannot make a file in the folder for $path")
        documents.remove(path)
        try {
            // No write over a file in place: a stop part way would leave half of it under its name.
            if (!canRename(temporary)) throw IOException("the picked folder's storage cannot rename a file, so the app cannot write $path safely")
            writeTo(temporary, content)
            documents[path] = DocumentsContract.getDocumentId(place(parent, temporary, name, over, path))
        } catch (error: Throwable) {
            try {
                DocumentsContract.deleteDocument(resolver, temporary)
            } catch (_: Exception) {
            }
            throw error
        }
    }

    override fun delete(path: String, hash: String) {
        documents.remove(path)
        val parent = findFolder(path.substringBeforeLast('/', "")) ?: return
        val name = path.substringAfterLast('/')
        val child = child(parent, name) ?: return
        if (child.folder) throw IOException("$path is a folder in the picked folder")
        val old = setAside(child, name, hash, path)
        if (!DocumentsContract.deleteDocument(resolver, old)) throw IOException("cannot delete $path")
    }

    /**
     * What a write or a delete left when it stopped part way: a temporary document goes; a file
     * set aside ([setAside]) gets its name back when no file holds the name, and goes otherwise
     * (the write after it finished).
     */
    override fun clean() {
        walk(top, "", 0, hidden = true) { child, path ->
            if (!child.folder && child.name.startsWith(SaveSync.TEMPORARY)) {
                try {
                    val original = child.name.takeIf { it.startsWith(SET_ASIDE) }?.removePrefix(SET_ASIDE)?.substringAfter('-')
                    val parent = findFolder(path.substringBeforeLast('/', ""))
                    if (original != null && parent != null && child(parent, original) == null) {
                        rename(document(child.id), original)
                    } else {
                        DocumentsContract.deleteDocument(resolver, document(child.id))
                    }
                } catch (error: Exception) {
                    Log.w(TAG, "the sync folder's $path stays: $error")
                }
            }
        }
    }

    /**
     * The file [existing] (named [name], at [path]) under a hidden name that keeps its own
     * ([SET_ASIDE]), for a write or a delete; it must have the content [hash] there, else another
     * app changed it meanwhile: it gets its name back and this throws [SaveSync.ChangedMeanwhile].
     */
    private fun setAside(existing: Child, name: String, hash: String, path: String): Uri {
        val old = rename(document(existing.id), "$SET_ASIDE${UUID.randomUUID().toString().take(8)}-$name")
        if (resolver.openInputStream(old)?.use(SaveSync::sha256) != hash) {
            // Not the file the plan saw: back under its name (or, failing that, by the next clean).
            try {
                rename(old, name)
            } catch (error: Exception) {
                Log.w(TAG, "$path stays set aside until the next sync: $error")
            }
            throw SaveSync.ChangedMeanwhile(path)
        }
        return old
    }

    /** [content] into the document [uri], flushed to the disk. */
    private fun writeTo(uri: Uri, content: (OutputStream) -> Unit) {
        val descriptor = resolver.openFileDescriptor(uri, "w") ?: throw IOException("cannot write $uri")
        ParcelFileDescriptor.AutoCloseOutputStream(descriptor).use { out ->
            BufferedOutputStream(out).let { buffered ->
                content(buffered)
                buffered.flush()
            }
            out.fd.sync()
        }
    }

    /**
     * The written [temporary] takes [name] in the folder [parent]: the file there now (if any),
     * which must have the content [over] (null: no file), is set aside first ([setAside]) and
     * removed last; when [name] cannot be taken exactly, it gets its name back and this fails.
     * The document now named [name].
     */
    private fun place(parent: String, temporary: Uri, name: String, over: String?, path: String): Uri {
        val existing = child(parent, name)
        if (existing?.folder == true) throw IOException("$name is a folder in the picked folder")
        if ((existing == null) != (over == null)) throw SaveSync.ChangedMeanwhile(path)
        val old = existing?.let { setAside(it, name, over!!, path) }
        val placed = try {
            rename(temporary, name)
        } catch (error: Exception) {
            old?.let { rename(it, name) }
            throw error
        }
        if (displayName(placed) != name) {
            DocumentsContract.deleteDocument(resolver, placed)
            old?.let { rename(it, name) }
            throw IOException("the folder did not take the name $name")
        }
        old?.let { DocumentsContract.deleteDocument(resolver, it) }
        return placed
    }

    private fun rename(uri: Uri, name: String): Uri = DocumentsContract.renameDocument(resolver, uri, name) ?: uri

    private fun canRename(uri: Uri): Boolean =
        resolver.query(uri, arrayOf(Document.COLUMN_FLAGS), null, null, null)?.use { cursor ->
            cursor.moveToFirst() && cursor.getInt(0) and Document.FLAG_SUPPORTS_RENAME != 0
        } == true

    private fun displayName(uri: Uri): String? =
        resolver.query(uri, arrayOf(Document.COLUMN_DISPLAY_NAME), null, null, null)?.use { cursor ->
            if (cursor.moveToFirst()) cursor.getString(0) else null
        }

    /** The folder at [path] (`""`: the top), made where missing; fails where a file holds a name on the way. */
    private fun makeFolder(path: String): String {
        if (path.isEmpty()) return top
        folders[path]?.let { return it }
        val parent = makeFolder(path.substringBeforeLast('/', ""))
        val name = path.substringAfterLast('/')
        val existing = child(parent, name)
        val id = when {
            existing?.folder == true -> existing.id
            existing != null -> throw IOException("$path is a file in the picked folder")
            else -> {
                val made = DocumentsContract.createDocument(resolver, document(parent), Document.MIME_TYPE_DIR, name)
                    ?: throw IOException("cannot make the folder $path")
                if (displayName(made) != name) {
                    DocumentsContract.deleteDocument(resolver, made)
                    throw IOException("the folder did not take the name $path")
                }
                DocumentsContract.getDocumentId(made)
            }
        }
        folders[path] = id
        return id
    }

    /** The folder at [path] when it is there. */
    private fun findFolder(path: String): String? {
        if (path.isEmpty()) return top
        folders[path]?.let { return it }
        val parent = findFolder(path.substringBeforeLast('/', "")) ?: return null
        val child = child(parent, path.substringAfterLast('/'))?.takeIf { it.folder } ?: return null
        folders[path] = child.id
        return child.id
    }

    /** The entry [name] in the folder [parent], read now (another app may have changed it since [files]). */
    private fun child(parent: String, name: String): Child? = children(parent).firstOrNull { it.name == name }

    private fun children(parent: String): List<Child> {
        val uri = DocumentsContract.buildChildDocumentsUriUsingTree(tree, parent)
        val cursor: Cursor = resolver.query(uri, CHILD_COLUMNS, null, null, null) ?: throw IOException("cannot read the picked folder")
        return cursor.use {
            buildList {
                while (it.moveToNext()) {
                    val name = it.getString(1) ?: continue
                    add(Child(it.getString(0), name, it.getString(2) == Document.MIME_TYPE_DIR, if (it.isNull(3)) 0 else it.getLong(3)))
                }
            }
        }
    }

    /**
     * Every entry below the folder [id] (at [prefix], [depth] below the top) that a sync takes
     * ([SaveSync.taken]; with [hidden], the [SaveSync.TEMPORARY] files too), with its path; into
     * the folders, at most [SaveSync.MAX_DEPTH].
     */
    private fun walk(id: String, prefix: String, depth: Int, hidden: Boolean = false, each: (Child, String) -> Unit) {
        for (child in children(id).sortedBy { it.name }) {
            if (!SaveSync.taken(child.name) && !(hidden && !child.folder && child.name.startsWith(SaveSync.TEMPORARY))) continue
            val path = prefix + child.name
            if (child.folder) {
                if (depth < SaveSync.MAX_DEPTH) {
                    folders[path] = child.id
                    walk(child.id, "$path/", depth + 1, hidden, each)
                }
            } else {
                each(child, path)
            }
        }
    }

    private fun document(id: String): Uri = DocumentsContract.buildDocumentUriUsingTree(tree, id)

    private companion object {
        const val TAG = "PD2SaveSync"
        const val BINARY = "application/octet-stream"
        /** The start of a set-aside file's name ([setAside]): `<SET_ASIDE><8 hex>-<its own name>`, hidden. */
        const val SET_ASIDE = "${SaveSync.TEMPORARY}old-"
        val CHILD_COLUMNS = arrayOf(Document.COLUMN_DOCUMENT_ID, Document.COLUMN_DISPLAY_NAME, Document.COLUMN_MIME_TYPE, Document.COLUMN_LAST_MODIFIED)
    }
}

/** What Settings shows of the save sync: a sync running, and how the last one went. */
data class SaveSyncState(
    val running: Boolean = false,
    /** When the last sync that ran ended (wall-clock milliseconds); null: none yet with this folder. */
    val at: Long? = null,
    /** Characters and other files that came into the game's Save, and that went to the folder, at that sync. */
    val toApp: Int = 0,
    val toFolder: Int = 0,
    /** Why the last sync did not run or did not finish; null: it did. */
    val problem: Problem? = null,
) {
    enum class Problem {
        /** The folder is not there or the grant is gone (an SD card out, the folder removed). */
        FOLDER_GONE,
        /** Sync now came while the game, a launch, the official PD2 launcher or a job ran. */
        BUSY,
        /** Some files did not sync (the log says which); the rest did. */
        FILES_FAILED,
        /** The sync stopped with an error. */
        FAILED,
    }

    /** Settings' line for this state at wall-clock [now], and its tone. */
    fun line(now: Long): Pair<Line, Tone> = when {
        running -> Line(R.string.sync_running) to Tone.WORKING
        problem == Problem.FOLDER_GONE -> Line(R.string.sync_folder_gone) to Tone.ERROR
        problem == Problem.BUSY -> Line(R.string.sync_busy) to Tone.WARNING
        problem == Problem.FILES_FAILED -> Line(R.string.sync_files_failed) to Tone.WARNING
        problem == Problem.FAILED -> Line(R.string.sync_failed) to Tone.ERROR
        at == null -> Line(R.string.sync_never) to Tone.PENDING
        else -> {
            val age = (now - at).coerceAtLeast(0)
            when {
                age < MINUTE_MS -> Line(R.string.sync_last_now, toApp, toFolder)
                age < HOUR_MS -> Line(R.string.sync_last_minutes, age / MINUTE_MS, toApp, toFolder)
                age < 2 * HOUR_MS -> Line(R.string.sync_last_one_hour, toApp, toFolder)
                age < 2 * DAY_MS -> Line(R.string.sync_last_hours, age / HOUR_MS, toApp, toFolder)
                else -> Line(R.string.sync_last_days, age / DAY_MS, toApp, toFolder)
            } to Tone.OK
        }
    }

    private companion object {
        const val MINUTE_MS = 60_000L
        const val HOUR_MS = 60 * MINUTE_MS
        const val DAY_MS = 24 * HOUR_MS
    }
}

/**
 * The save sync on Android ([SaveSync]): the picked folder's lasting grant, the record
 * (`files/userdata/sync-record`), the state Settings shows ([state], kept in the `save_sync`
 * preferences), and the sync itself ([run]), one at a time. The session service runs it before
 * each Play and each press of the official PD2 launcher, after the game and after the official
 * launcher's session, and when the player turns the setting on, picks another folder or taps
 * Sync now; never while the game, a launch, the official launcher or a job runs (the service
 * checks that).
 */
object SaveSyncFolder {
    private const val PREFERENCES = "save_sync"
    private const val AT = "at"
    private const val TO_APP = "to_app"
    private const val TO_FOLDER = "to_folder"
    private const val PROBLEM = "problem"
    /** Android's own provider of shared storage and SD cards, whose tree IDs are `<volume>:<path>`. */
    private const val EXTERNAL_STORAGE = "com.android.externalstorage.documents"
    private val lock = Any()
    private val _state = MutableStateFlow(SaveSyncState())
    private var loaded = false

    /** Settings' view of the sync; [load] first. */
    val state: StateFlow<SaveSyncState> = _state.asStateFlow()

    fun recordFile(paths: SessionPaths) = File(paths.userData, "sync-record")

    /** Reads the last sync's state once per process. */
    fun load(context: Context) {
        synchronized(lock) {
            if (loaded) return
            loaded = true
            val prefs = context.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE)
            _state.value = SaveSyncState(
                at = prefs.getLong(AT, 0).takeIf { it > 0 },
                toApp = prefs.getInt(TO_APP, 0),
                toFolder = prefs.getInt(TO_FOLDER, 0),
                problem = SaveSyncState.Problem.entries.firstOrNull { it.name == prefs.getString(PROBLEM, null) },
            )
        }
    }

    private fun store(context: Context, state: SaveSyncState) {
        _state.value = state
        context.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE).edit()
            .putLong(AT, state.at ?: 0).putInt(TO_APP, state.toApp).putInt(TO_FOLDER, state.toFolder)
            .putString(PROBLEM, state.problem?.name).apply()
    }

    /** The picker's starting place: the folder in use, else Documents in the device's shared storage. */
    fun pickerStart(folder: String?): Uri =
        folder?.let(Uri::parse) ?: DocumentsContract.buildDocumentUri(EXTERNAL_STORAGE, "primary:Documents")

    /** How Settings names the folder [uri]: `Documents/PD2 Saves`, `<sdCard>/PD2 Saves`, or the provider's name for it. */
    fun label(context: Context, uri: Uri): String {
        val id = try {
            DocumentsContract.getTreeDocumentId(uri)
        } catch (_: IllegalArgumentException) {
            null
        }
        return label(uri.authority, id, context.getString(R.string.sync_internal_storage), context.getString(R.string.sync_sd_card))
            ?: runCatching {
                context.contentResolver.query(DocumentsContract.buildDocumentUriUsingTree(uri, id), arrayOf(Document.COLUMN_DISPLAY_NAME), null, null, null)
                    ?.use { if (it.moveToFirst()) it.getString(0) else null }
            }.getOrNull()
            ?: uri.lastPathSegment.orEmpty()
    }

    /** [label] for Android's storage provider (tree [id] `primary:Documents/PD2 Saves`, `1A2B-3C4D:PD2 Saves`); null for another. */
    fun label(authority: String?, id: String?, internal: String, sdCard: String): String? {
        if (authority != EXTERNAL_STORAGE || id == null || ':' !in id) return null
        val volume = id.substringBefore(':')
        val path = id.substringAfter(':').trim('/')
        val top = if (volume == "primary") internal else sdCard
        return when {
            path.isEmpty() -> top
            volume == "primary" -> path
            else -> "$top/$path"
        }
    }

    /** Why [choose] did not take a folder. */
    enum class Refusal { OWN_FOLDER, NO_ACCESS }

    /**
     * The folder the picker gave: its lasting grant is taken, the setting names it, and an earlier
     * folder's grant and the record of the sync with it go (the first sync with this one is a
     * merge). A folder of this app's own "PD2 Launcher" root is refused (it is the game folder).
     * The caller then has the session service sync. Blocking.
     */
    fun choose(context: Context, uri: Uri): Refusal? {
        if (uri.authority == GameFilesProvider.authority(context)) return Refusal.OWN_FOLDER
        val flags = Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_WRITE_URI_PERMISSION
        try {
            context.contentResolver.takePersistableUriPermission(uri, flags)
        } catch (error: SecurityException) {
            Log.w("PD2SaveSync", "no lasting access to $uri: $error")
            return Refusal.NO_ACCESS
        }
        synchronized(lock) {
            // On the newest settings: a change made meanwhile on another screen stays.
            var before: String? = null
            AppSettingsStore.update(context) {
                before = it.syncFolder
                it.copy(syncFolder = uri.toString())
            }
            if (before != uri.toString()) {
                before?.let { release(context, Uri.parse(it)) }
                forget(context)
            }
        }
        return null
    }

    /** The setting goes off: both copies stay as they are; the grant, the record and the state go. Blocking. */
    fun turnOff(context: Context) {
        synchronized(lock) {
            var before: String? = null
            AppSettingsStore.update(context) {
                before = it.syncFolder
                it.copy(syncFolder = null)
            }
            before?.let { release(context, Uri.parse(it)) }
            forget(context)
        }
    }

    private fun forget(context: Context) {
        recordFile(SessionPaths(DeviceCheck.dataDir(context))).delete()
        store(context, SaveSyncState())
    }

    private fun release(context: Context, uri: Uri) {
        try {
            context.contentResolver.releasePersistableUriPermission(uri, Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_WRITE_URI_PERMISSION)
        } catch (_: SecurityException) {
        }
    }

    /**
     * [block] while no sync runs and none starts: a launch holds it from the Save's link through
     * its sync to the engine's command, so a sync that waited for it then sees the launch
     * ([run]'s busy). Blocking; the same thread may sync inside it.
     */
    fun <T> locked(block: () -> T): T = synchronized(lock) { block() }

    /**
     * One sync of the player's Save with the folder the setting names, if any ([SaveSync.sync]),
     * one at a time; [why] names it in the log: one line for the sync, and with [detailed] a line
     * for each file. [busy], asked once the sync has its turn (null: a launch's own sync): the
     * game, a launch, the official launcher or a job runs, so no sync now
     * ([SaveSyncState.Problem.BUSY]). A folder that is not there (or whose grant is gone) is no
     * sync, and the state says so. Never throws. Blocking.
     */
    fun run(context: Context, paths: SessionPaths, why: String, detailed: Boolean, busy: (() -> Boolean)?, log: (String) -> Unit) {
        synchronized(lock) {
            load(context)
            val folder = AppSettingsStore.load(context).syncFolder ?: return
            if (busy?.invoke() == true) {
                log("save sync ($why): not now: the game, a launch, the official launcher or a job runs")
                store(context, _state.value.copy(running = false, problem = SaveSyncState.Problem.BUSY))
                return
            }
            val uri = Uri.parse(folder)
            val started = System.currentTimeMillis()
            _state.update { it.copy(running = true) }
            val tree = DocumentTree(context.contentResolver, uri)
            val granted = context.contentResolver.persistedUriPermissions.any { it.uri == uri && it.isReadPermission && it.isWritePermission }
            if (!granted || !tree.available()) {
                log("save sync ($why): not done: the folder is not available (${if (granted) "it is not there" else "the app has no access to it"}); the game's characters stay as they are")
                store(context, _state.value.copy(running = false, problem = SaveSyncState.Problem.FOLDER_GONE))
                return
            }
            val result = try {
                SaveSync.sync(FileTree(paths.userSave), tree, folder, recordFile(paths), paths.saveBackups, started, if (detailed) log else { _ -> })
            } catch (error: Exception) {
                val gone = !tree.available()
                log("save sync ($why): stopped: $error${if (gone) "; the folder is not available any more" else ""}")
                store(context, _state.value.copy(running = false, problem = if (gone) SaveSyncState.Problem.FOLDER_GONE else SaveSyncState.Problem.FAILED))
                return
            }
            val outcome = result.outcome
            log(
                "save sync ($why) with ${label(context, uri)}: ${outcome.toApp} in, ${outcome.toFolder} out" +
                    when {
                        result.first -> " (the first sync with this folder: a merge)"
                        result.emptied -> " (one side had no file at all: a merge, nothing deleted)"
                        else -> ""
                    } +
                    (outcome.backup?.let { ", ${outcome.backedUp} files to Backups/${it.name}" } ?: "") +
                    (if (outcome.backupFolders > 0) ", ${outcome.backupFolders} backup folders copied to the folder" else "") +
                    (if (outcome.failed.isNotEmpty()) ", not synced: ${outcome.failed.joinToString("; ")}" else "") +
                    "; ${result.files} files in each, ${System.currentTimeMillis() - started} ms",
            )
            store(
                context,
                SaveSyncState(
                    at = System.currentTimeMillis(), toApp = outcome.toApp, toFolder = outcome.toFolder,
                    problem = if (outcome.failed.isNotEmpty()) SaveSyncState.Problem.FILES_FAILED else null,
                ),
            )
        }
    }
}
