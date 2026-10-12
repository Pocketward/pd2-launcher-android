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
import android.provider.DocumentsContract
import android.provider.DocumentsContract.Document
import android.provider.DocumentsContract.Root
import android.provider.DocumentsProvider
import android.util.Log
import android.webkit.MimeTypeMap
import com.pd2launcher.app.control.Game
import java.io.File
import java.io.FileNotFoundException
import java.nio.file.Files
import java.nio.file.LinkOption
import java.nio.file.attribute.PosixFilePermissions
import org.json.JSONObject

/**
 * The "PD2 Launcher" root in Android's Files app and every file picker: the Diablo II folder in
 * use, its real files (no copies), read-write; its `Save` shows the characters in
 * `files/userdata/Save` ([SaveKeeper]). [DocumentPaths] maps the IDs and refuses any path out of
 * it. Nothing changes while the game runs (it writes its folder,
 * and the characters when it closes) or while the Launcher works on the folder.
 */
class GameFilesProvider : DocumentsProvider() {
    private lateinit var authority: String

    override fun attachInfo(context: Context, info: ProviderInfo) {
        authority = info.authority
        super.attachInfo(context, info)
    }

    override fun onCreate(): Boolean = true

    private fun documents(): DocumentPaths = documents(context!!)

    /** Throws, with why for the player, when [documentId] cannot change now ([DocumentPaths.refusal]). */
    private fun checkWritable(documents: DocumentPaths, documentId: String, whole: Boolean = false) {
        val refusal = documents.refusal(documentId, gameRunning(), busy(), whole) ?: return
        val message = context!!.getString(
            when (refusal) {
                DocumentPaths.Refusal.FIXED -> R.string.files_refused_fixed
                DocumentPaths.Refusal.GAME_RUNNING -> R.string.files_refused_game_running
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
        val writable = documents.refusal(id, gameRunning(), busy()) == null
        var flags = 0
        if (writable && folder) flags = flags or Document.FLAG_DIR_SUPPORTS_CREATE
        if (writable && !folder) flags = flags or Document.FLAG_SUPPORTS_WRITE
        if (writable && id != DocumentPaths.ROOT) {
            flags = flags or Document.FLAG_SUPPORTS_DELETE or Document.FLAG_SUPPORTS_RENAME or Document.FLAG_SUPPORTS_MOVE
        }
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
        if (access == ParcelFileDescriptor.MODE_READ_ONLY) return ParcelFileDescriptor.open(file, access)
        checkWritable(documents, documentId)
        val parent = documents.parent(documentId)
        return ParcelFileDescriptor.open(file, access, Handler(Looper.getMainLooper())) { notifyChanged(parent) }
    }

    override fun createDocument(parentDocumentId: String, mimeType: String, displayName: String): String {
        val documents = documents()
        checkWritable(documents, parentDocumentId)
        if (!documents.file(parentDocumentId).isDirectory) throw FileNotFoundException("$parentDocumentId is not a folder")
        val id = freeName(documents, parentDocumentId, displayName)
        val path = documents.file(id).toPath()
        // Owner-only, as the app's own files are: the app process's umask is 0.
        if (mimeType == Document.MIME_TYPE_DIR) {
            Files.createDirectory(path, PosixFilePermissions.asFileAttribute(PosixFilePermissions.fromString("rwx------")))
        } else {
            Files.createFile(path, PosixFilePermissions.asFileAttribute(PosixFilePermissions.fromString("rw-------")))
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
        checkWritable(documents, documentId, whole = true)
        // Follows no link: removing the game folder's Save removes the link, never the characters.
        UserlandInstaller.deleteTree(documents.file(documentId).toPath())
        documents.parent(documentId)?.let(::notifyChanged)
    }

    override fun renameDocument(documentId: String, displayName: String): String {
        val documents = documents()
        checkWritable(documents, documentId, whole = true)
        val parent = documents.parent(documentId)!!
        val target = documents.child(parent, displayName, renaming = documents.file(documentId).name)
        move(documents.file(documentId), documents.file(target))
        notifyChanged(parent)
        return target
    }

    override fun moveDocument(sourceDocumentId: String, sourceParentDocumentId: String, targetParentDocumentId: String): String {
        val documents = documents()
        checkWritable(documents, sourceDocumentId, whole = true)
        checkWritable(documents, targetParentDocumentId)
        val target = documents.child(targetParentDocumentId, documents.file(sourceDocumentId).name)
        move(documents.file(sourceDocumentId), documents.file(target))
        documents.parent(sourceDocumentId)?.let(::notifyChanged)
        notifyChanged(targetParentDocumentId)
        return target
    }

    /** A rename (both are below `files/`, one file system); never over an existing file. */
    private fun move(from: File, to: File) {
        if (Files.exists(to.toPath(), LinkOption.NOFOLLOW_LINKS)) throw IllegalStateException("${to.name} already exists")
        Files.move(from.toPath(), to.toPath())
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

    companion object {
        private const val TAG = "pd2-files"
        const val ROOT = "pd2launcher"

        /** The game folder in use, as documents ([DocumentPaths]). */
        fun documents(context: Context): DocumentPaths {
            val paths = SessionPaths(DeviceCheck.dataDir(context))
            return DocumentPaths(gameFolder(paths), paths.userData)
        }

        /**
         * The game folder in use: the engine's word while it runs, else the Launcher's record of
         * a folder used in place (`prefix/.pd2launcher-game-folder.json`);
         * without one, the Managed prefix's.
         */
        private fun gameFolder(paths: SessionPaths): File {
            val snapshot = SessionService.status.value.snapshot
            if (snapshot != null) return SaveKeeper.liveSave(paths, snapshot.gameFolder?.path).parentFile!!
            val recorded = try {
                val record = File(paths.managedRoot, "prefix/.pd2launcher-game-folder.json")
                if (record.isFile && record.length() < 64 * 1024) JSONObject(record.readText()).getString("path") else null
            } catch (error: Exception) {
                Log.w(TAG, "game folder record unreadable: $error")
                null
            }
            return SaveKeeper.liveSave(paths, recorded).parentFile!!
        }

        fun gameRunning(): Boolean = SessionService.status.value.snapshot?.game == Game.RUNNING

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
