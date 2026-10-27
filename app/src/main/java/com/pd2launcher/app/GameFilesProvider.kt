package com.pd2launcher.app

import android.content.Context
import android.content.pm.ProviderInfo
import android.database.Cursor
import android.database.MatrixCursor
import android.net.Uri
import android.os.CancellationSignal
import android.os.Handler
import android.os.Looper
import android.os.ParcelFileDescriptor
import android.os.SystemClock
import android.provider.DocumentsContract
import android.provider.DocumentsContract.Document
import android.provider.DocumentsContract.Root
import android.provider.DocumentsProvider
import android.system.ErrnoException
import android.system.Os
import android.system.OsConstants
import android.util.Log
import android.webkit.MimeTypeMap
import com.pd2launcher.app.control.Game
import java.io.File
import java.io.FileNotFoundException
import java.nio.file.Files
import java.nio.file.LinkOption
import java.nio.file.Paths
import java.nio.file.attribute.PosixFilePermissions
import org.json.JSONObject

/**
 * The "PD2 Launcher" root in Android's Files app and every file picker: the Diablo II folder in
 * use, its real files (no copies), read-write; its `Save` shows the characters in
 * `files/userdata/Save` ([SaveKeeper]). [DocumentPaths] maps the IDs and refuses any path out of
 * it. Nothing changes while the game runs (it writes its folder, and the characters when it
 * closes), from a launch request until the game shows or the launch fails (the engine prepares
 * the game's files meanwhile), or while the Launcher works on the folder; otherwise any file can
 * change, programs and libraries too ([DocumentPaths.refusal]).
 */
class GameFilesProvider : DocumentsProvider() {
    private lateinit var authority: String

    override fun attachInfo(context: Context, info: ProviderInfo) {
        authority = info.authority
        super.attachInfo(context, info)
    }

    override fun onCreate(): Boolean = true

    private fun documents(): DocumentPaths = documents(context!!)

    /** Throws, with why for the player, when [documentId] cannot take [change] now ([DocumentPaths.refusal]). */
    private fun checkWritable(
        documents: DocumentPaths,
        documentId: String,
        change: DocumentPaths.Change = DocumentPaths.Change.WRITE,
    ) {
        val refusal = refusal(documents, documentId, change) ?: return
        val message = context!!.getString(
            when (refusal) {
                DocumentPaths.Refusal.FIXED -> R.string.files_refused_fixed
                DocumentPaths.Refusal.GAME_RUNNING -> R.string.files_refused_game_running
                DocumentPaths.Refusal.GAME_STARTING -> R.string.files_refused_game_starting
                DocumentPaths.Refusal.LAUNCHER_BUSY -> R.string.files_refused_busy
            },
        )
        Log.i(TAG, "refused for $documentId: $refusal")
        throw IllegalStateException(message)
    }

    override fun queryRoots(projection: Array<out String>?): Cursor {
        val result = MatrixCursor(projection ?: ROOT_COLUMNS)
        if (!documents().available()) return result
        val context = context!!
        result.newRow().apply {
            add(Root.COLUMN_ROOT_ID, ROOT)
            add(Root.COLUMN_DOCUMENT_ID, DocumentPaths.ROOT)
            add(Root.COLUMN_TITLE, context.getString(R.string.files_root_title))
            add(Root.COLUMN_SUMMARY, context.getString(R.string.files_root_summary))
            add(Root.COLUMN_FLAGS, Root.FLAG_SUPPORTS_CREATE or Root.FLAG_SUPPORTS_IS_CHILD or Root.FLAG_LOCAL_ONLY)
            add(Root.COLUMN_ICON, context.applicationInfo.icon.takeIf { it != 0 } ?: android.R.drawable.sym_def_app_icon)
            add(Root.COLUMN_AVAILABLE_BYTES, File(context.dataDir.path).usableSpace)
            add(Root.COLUMN_MIME_TYPES, "*/*")
        }
        return result
    }

    override fun queryDocument(documentId: String, projection: Array<out String>?): Cursor {
        val result = MatrixCursor(projection ?: DOCUMENT_COLUMNS)
        addRow(result, documents(), documentId)
        return result
    }

    override fun queryChildDocuments(parentDocumentId: String, projection: Array<out String>?, sortOrder: String?): Cursor {
        val documents = documents()
        val result = MatrixCursor(projection ?: DOCUMENT_COLUMNS)
        for (id in documents.children(parentDocumentId)) addRow(result, documents, id)
        result.setNotificationUri(context!!.contentResolver, DocumentsContract.buildChildDocumentsUri(authority, parentDocumentId))
        return result
    }

    private fun addRow(result: MatrixCursor, documents: DocumentPaths, id: String) {
        val file = documents.file(id)
        val folder = file.isDirectory
        val running = gameRunning()
        val busy = busy()
        val starting = gameStarting()
        val allows = { change: DocumentPaths.Change -> documents.refusal(id, running, busy, change, starting) == null }
        var flags = 0
        if (allows(DocumentPaths.Change.WRITE)) {
            flags = flags or if (folder) Document.FLAG_DIR_SUPPORTS_CREATE else Document.FLAG_SUPPORTS_WRITE
        }
        if (allows(DocumentPaths.Change.DELETE)) flags = flags or Document.FLAG_SUPPORTS_DELETE
        if (allows(DocumentPaths.Change.MOVE)) flags = flags or Document.FLAG_SUPPORTS_RENAME or Document.FLAG_SUPPORTS_MOVE
        result.newRow().apply {
            add(Document.COLUMN_DOCUMENT_ID, id)
            add(Document.COLUMN_DISPLAY_NAME, if (id == DocumentPaths.ROOT) context!!.getString(R.string.files_game) else file.name)
            if (id == DocumentPaths.ROOT) add(Document.COLUMN_SUMMARY, context!!.getString(R.string.files_root_summary))
            add(Document.COLUMN_MIME_TYPE, if (folder) Document.MIME_TYPE_DIR else mimeType(file.name))
            add(Document.COLUMN_SIZE, if (folder) null else file.length())
            add(Document.COLUMN_LAST_MODIFIED, file.lastModified())
            add(Document.COLUMN_FLAGS, flags)
        }
    }

    override fun openDocument(documentId: String, mode: String, signal: CancellationSignal?): ParcelFileDescriptor {
        val documents = documents()
        val file = documents.file(documentId)
        if (file.isDirectory) throw FileNotFoundException("$documentId is a folder")
        val access = ParcelFileDescriptor.parseMode(mode)
        val write = access != ParcelFileDescriptor.MODE_READ_ONLY
        if (write) checkWritable(documents, documentId)
        val flags = when (access and ParcelFileDescriptor.MODE_READ_WRITE) {
            ParcelFileDescriptor.MODE_READ_WRITE -> OsConstants.O_RDWR
            ParcelFileDescriptor.MODE_WRITE_ONLY -> OsConstants.O_WRONLY
            else -> OsConstants.O_RDONLY
        }
        // Opened without waiting (a FIFO would hold the open), neither created nor truncated, and
        // checked; only then opened again through that descriptor, the same file, for use.
        verified(documents, documentId, descriptor(file.path, flags or OsConstants.O_NONBLOCK), folder = false).use { probe ->
            val same = File("/proc/self/fd/${probe.fd}")
            if (!write) return ParcelFileDescriptor.open(same, access)
            val parent = documents.parent(documentId)
            val opened = ParcelFileDescriptor.open(
                same, access and (ParcelFileDescriptor.MODE_CREATE or ParcelFileDescriptor.MODE_TRUNCATE).inv(),
                Handler(Looper.getMainLooper()),
            ) { notifyChanged(parent) }
            if (access and ParcelFileDescriptor.MODE_TRUNCATE != 0) {
                try {
                    Os.ftruncate(opened.fileDescriptor, 0)
                } catch (error: ErrnoException) {
                    opened.close()
                    throw FileNotFoundException("$documentId: ${error.message}")
                }
            }
            return opened
        }
    }

    /**
     * [descriptor], opened for [documentId], when what it opened is a [folder] or a file as asked
     * and where it really is (the path the kernel gives it, links followed when it was opened) is
     * allowed ([DocumentPaths.openedFolder], [DocumentPaths.openedFile]); otherwise it is closed and
     * the open refused. A link swapped in after [DocumentPaths.file]'s check cannot redirect it.
     */
    private fun verified(
        documents: DocumentPaths,
        documentId: String,
        descriptor: ParcelFileDescriptor,
        folder: Boolean,
    ): ParcelFileDescriptor {
        val allowed = try {
            val stat = Os.fstat(descriptor.fileDescriptor)
            val reported = Os.readlink("/proc/self/fd/${descriptor.fd}")
            val ok = if (folder) {
                OsConstants.S_ISDIR(stat.st_mode) && documents.openedFolder(reported)
            } else {
                documents.openedFile(reported, OsConstants.S_ISREG(stat.st_mode), stat.st_nlink)
            }
            ok.also { if (!it) Log.w(TAG, "$documentId opened $reported (mode ${Integer.toOctalString(stat.st_mode)}, ${stat.st_nlink} names); refused") }
        } catch (error: ErrnoException) {
            Log.w(TAG, "$documentId: its descriptor cannot be checked: $error")
            false
        }
        if (!allowed) {
            descriptor.close()
            throw FileNotFoundException("$documentId leads out of the game folder")
        }
        return descriptor
    }

    /** [path] opened with [flags] (and close-on-exec), held as a ParcelFileDescriptor for its number. */
    private fun descriptor(path: String, flags: Int): ParcelFileDescriptor {
        val fd = try {
            Os.open(path, flags or OsConstants.O_CLOEXEC, 0)
        } catch (error: ErrnoException) {
            throw FileNotFoundException("$path: ${error.message}")
        }
        try {
            return ParcelFileDescriptor.dup(fd)
        } finally {
            Os.close(fd)
        }
    }

    /**
     * [use] with `/proc/self/fd/<n>` of folder [folderId] open (its links followed, as the game
     * folder's Save link is) and checked ([verified]): an entry reached through it cannot be moved
     * elsewhere by a link swapped in on the way afterwards, and a create, a removal or a rename does
     * not follow its last part. Opened as a path only (O_PATH): a folder with write and search
     * permission but no read permission takes changes as before, and nothing blocks on the open.
     */
    private fun <T> inFolder(documents: DocumentPaths, folderId: String, use: (String) -> T): T =
        verified(documents, folderId, descriptor(documents.file(folderId).path, O_PATH), folder = true)
            .use { use("/proc/self/fd/${it.fd}") }

    override fun createDocument(parentDocumentId: String, mimeType: String, displayName: String): String {
        val documents = documents()
        checkWritable(documents, parentDocumentId)
        if (!documents.file(parentDocumentId).isDirectory) throw FileNotFoundException("$parentDocumentId is not a folder")
        val id = freeName(documents, parentDocumentId, displayName)
        inFolder(documents, parentDocumentId) { folder ->
            val path = Paths.get(folder, id.substringAfterLast('/'))
            // Owner-only, as the app's own files are: the app process's umask is 0.
            if (mimeType == Document.MIME_TYPE_DIR) {
                Files.createDirectory(path, PosixFilePermissions.asFileAttribute(PosixFilePermissions.fromString("rwx------")))
            } else {
                Files.createFile(path, PosixFilePermissions.asFileAttribute(PosixFilePermissions.fromString("rw-------")))
            }
        }
        notifyChanged(parentDocumentId)
        return id
    }

    /** [name] in [parentId], or `name (2)` and on, the first that is not taken. */
    private fun freeName(documents: DocumentPaths, parentId: String, name: String): String {
        val taken = { candidate: String ->
            documents.caseClash(parentId, candidate) ||
                Files.exists(documents.file(documents.child(parentId, candidate)).toPath(), LinkOption.NOFOLLOW_LINKS)
        }
        return documents.child(parentId, if (taken(name)) FileTransfer.keepBothName(name, taken) else name)
    }

    override fun deleteDocument(documentId: String) {
        val documents = documents()
        checkWritable(documents, documentId, DocumentPaths.Change.DELETE)
        val parent = documents.parent(documentId) ?: throw FileNotFoundException("$documentId is the game folder")
        // Follows no link: removing the game folder's Save removes the link, never the characters,
        // and a folder's contents go each through its own open folder ([PinnedRemoval]).
        inFolder(documents, parent) { folder -> PinnedRemoval(OsFolders()).remove(folder, documentId.substringAfterLast('/')) }
        notifyChanged(parent)
    }

    override fun renameDocument(documentId: String, displayName: String): String {
        val documents = documents()
        checkWritable(documents, documentId, DocumentPaths.Change.MOVE)
        val parent = documents.parent(documentId)!!
        val target = documents.child(parent, displayName, renaming = documents.file(documentId).name)
        move(documents, documentId, target)
        notifyChanged(parent)
        return target
    }

    override fun moveDocument(sourceDocumentId: String, sourceParentDocumentId: String, targetParentDocumentId: String): String {
        val documents = documents()
        checkWritable(documents, sourceDocumentId, DocumentPaths.Change.MOVE)
        checkWritable(documents, targetParentDocumentId)
        val target = documents.child(targetParentDocumentId, documents.file(sourceDocumentId).name)
        move(documents, sourceDocumentId, target)
        documents.parent(sourceDocumentId)?.let(::notifyChanged)
        notifyChanged(targetParentDocumentId)
        return target
    }

    /**
     * A rename of [fromId] to [toId], each reached through its open folder ([inFolder]; both are
     * below `files/`, one file system); never over an existing file.
     */
    private fun move(documents: DocumentPaths, fromId: String, toId: String) {
        val fromFolder = documents.parent(fromId) ?: throw FileNotFoundException("$fromId is the game folder")
        val toFolder = documents.parent(toId) ?: throw FileNotFoundException("$toId is the game folder")
        inFolder(documents, fromFolder) { fromOpen ->
            inFolder(documents, toFolder) { toOpen ->
                val from = Paths.get(fromOpen, fromId.substringAfterLast('/'))
                val to = Paths.get(toOpen, toId.substringAfterLast('/'))
                if (Files.exists(to, LinkOption.NOFOLLOW_LINKS)) throw IllegalStateException("${to.fileName} already exists")
                Files.move(from, to)
            }
        }
    }

    override fun isChildDocument(parentDocumentId: String, documentId: String): Boolean =
        documents().isChild(parentDocumentId, documentId)

    override fun getDocumentType(documentId: String): String {
        val file = documents().file(documentId)
        return if (file.isDirectory) Document.MIME_TYPE_DIR else mimeType(file.name)
    }

    private fun notifyChanged(parentId: String?) {
        if (parentId == null) return
        context!!.contentResolver.notifyChange(DocumentsContract.buildChildDocumentsUri(authority, parentId), null, 0)
    }

    /**
     * Folders opened through [Os] below open ones, for [PinnedRemoval]: O_NOFOLLOW, so a link is no
     * folder here, and O_NONBLOCK, so a FIFO does not hold the open; what is not a folder by fstat is
     * closed again. One without read permission is made owner-only by its path first (a real folder
     * by lstat), and every one through its descriptor once open.
     */
    private class OsFolders : PinnedRemoval.Folders {
        private val held = HashMap<String, ParcelFileDescriptor>()

        override fun open(folder: String, name: String): String? {
            val path = "$folder/$name"
            val flags = OsConstants.O_RDONLY or OsConstants.O_NOFOLLOW or OsConstants.O_NONBLOCK or OsConstants.O_CLOEXEC
            val fd = try {
                try {
                    Os.open(path, flags, 0)
                } catch (error: ErrnoException) {
                    when {
                        error.errno == OsConstants.ELOOP -> return null
                        error.errno != OsConstants.EACCES -> throw error
                        !OsConstants.S_ISDIR(Os.lstat(path).st_mode) -> return null
                        else -> {
                            Os.chmod(path, OWNER_ONLY)
                            Os.open(path, flags, 0)
                        }
                    }
                }
            } catch (error: ErrnoException) {
                throw error.rethrowAsIOException()
            }
            val descriptor = try {
                ParcelFileDescriptor.dup(fd)
            } finally {
                Os.close(fd)
            }
            try {
                if (!OsConstants.S_ISDIR(Os.fstat(descriptor.fileDescriptor).st_mode)) {
                    descriptor.close()
                    return null
                }
                Os.fchmod(descriptor.fileDescriptor, OWNER_ONLY)
            } catch (error: ErrnoException) {
                descriptor.close()
                throw error.rethrowAsIOException()
            }
            return "/proc/self/fd/${descriptor.fd}".also { held[it] = descriptor }
        }

        override fun close(descriptor: String) {
            held.remove(descriptor)?.close()
        }
    }

    companion object {
        private const val TAG = "pd2-files"
        /** Linux's O_PATH (asm-generic: arm64 and x86-64 alike), which OsConstants does not name. */
        private const val O_PATH = 0x200000
        /** Owner read, write and search (0700). */
        private const val OWNER_ONLY = 0x1c0
        const val ROOT = "pd2launcher"

        /** The game folder in use, as documents ([DocumentPaths]). */
        fun documents(context: Context): DocumentPaths {
            val paths = SessionPaths(DeviceCheck.dataDir(context))
            return DocumentPaths(gameFolder(paths, SessionService.status.value.snapshot?.gameFolder?.path), playerFolder(paths))
        }

        /**
         * Tells Android's Files app and the file pickers to read the roots again ([queryRoots]):
         * the root is there only while there is a game folder, and they keep the roots they read
         * until they are told.
         */
        fun rootsChanged(context: Context) {
            context.contentResolver.notifyChange(DocumentsContract.buildRootsUri(authority(context)), null, 0)
        }

        /** `files/userdata`, where the game folder's Save leads; none when a link is on the way to it ([DocumentPaths.playerFolder]). */
        private fun playerFolder(paths: SessionPaths): File? = DocumentPaths.playerFolder(paths) ?: run {
            if (Files.exists(paths.userData.toPath(), LinkOption.NOFOLLOW_LINKS)) Log.w(TAG, "player folder refused: ${paths.userData}")
            null
        }

        /**
         * The game folder in use: the one the engine names ([engineFolder]), else the folder used
         * in place by the Launcher's record ([recorded], `prefix/.pd2launcher-game-folder.json`),
         * else the Managed prefix's. An import writes the record, and the engine names that folder
         * only from its next start, so in the session of the import the record says where the game
         * is. None when that is not where a game folder can be ([DocumentPaths.gameFolder]).
         */
        fun gameFolder(paths: SessionPaths, engineFolder: String?, recorded: () -> String? = { recordedGameFolder(paths) }): File? {
            val folder = SaveKeeper.liveSave(paths, engineFolder ?: recorded()).parentFile!!
            return DocumentPaths.gameFolder(paths, folder) ?: run {
                if (folder.exists()) Log.w(TAG, "game folder refused: $folder")
                null
            }
        }

        /**
         * The folder used in place by the engine's own record
         * (`prefix/.pd2launcher-game-folder.json`), which holds while the engine has not said yet
         * (starting or restarting, or in the session of an import: [gameFolder]); null without one.
         */
        fun recordedGameFolder(paths: SessionPaths): String? = try {
            val record = File(paths.managedRoot, "prefix/.pd2launcher-game-folder.json")
            if (record.isFile && record.length() < 64 * 1024) JSONObject(record.readText()).getString("path") else null
        } catch (error: Exception) {
            Log.w(TAG, "game folder record unreadable: $error")
            null
        }

        fun gameRunning(): Boolean = SessionService.status.value.snapshot?.game == Game.RUNNING

        /** A launch is under way ([DocumentPaths.gameStarting]). */
        fun gameStarting(): Boolean =
            DocumentPaths.gameStarting(SessionService.status.value.launchRequestedAt, SystemClock.elapsedRealtime())

        /** Whether [change] to [documentId] is refused now ([DocumentPaths.refusal]), by what the session does. */
        fun refusal(documents: DocumentPaths, documentId: String, change: DocumentPaths.Change = DocumentPaths.Change.WRITE) =
            documents.refusal(documentId, gameRunning(), busy(), change, gameStarting())

        /** A Launcher job (install, update, repair) is working on the game folder. */
        fun busy(): Boolean = SessionService.status.value.snapshot?.activeJob != null

        /** `<package>.documents`, as in AndroidManifest.xml. */
        fun authority(context: Context) = "${context.packageName}.documents"

        private val ROOT_COLUMNS = arrayOf(
            Root.COLUMN_ROOT_ID, Root.COLUMN_DOCUMENT_ID, Root.COLUMN_TITLE, Root.COLUMN_SUMMARY, Root.COLUMN_FLAGS,
            Root.COLUMN_ICON, Root.COLUMN_AVAILABLE_BYTES, Root.COLUMN_MIME_TYPES,
        )
        private val DOCUMENT_COLUMNS = arrayOf(
            Document.COLUMN_DOCUMENT_ID, Document.COLUMN_DISPLAY_NAME, Document.COLUMN_SUMMARY, Document.COLUMN_MIME_TYPE,
            Document.COLUMN_SIZE, Document.COLUMN_LAST_MODIFIED, Document.COLUMN_FLAGS,
        )

        /** Text files the player edits open in a text editor: loot filters, settings and logs. */
        private val TEXT = setOf("filter", "txt", "json", "ini", "cfg", "log", "yaml", "md")

        fun mimeType(name: String): String {
            val extension = name.substringAfterLast('.', "").lowercase()
            if (extension in TEXT) return "text/plain"
            return MimeTypeMap.getSingleton().getMimeTypeFromExtension(extension) ?: "application/octet-stream"
        }

        /** The Files app at the "PD2 Launcher" root. */
        fun rootUri(context: Context): Uri = DocumentsContract.buildRootUri(authority(context), ROOT)

        /** The game folder's document, a picker's starting place. */
        fun gameUri(context: Context): Uri = DocumentsContract.buildDocumentUri(authority(context), DocumentPaths.ROOT)
    }
}
