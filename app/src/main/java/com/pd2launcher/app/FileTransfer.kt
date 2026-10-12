package com.pd2launcher.app

import android.content.Context
import android.net.Uri
import android.provider.DocumentsContract
import android.provider.OpenableColumns
import java.io.File
import java.io.IOException
import java.nio.file.Files
import java.nio.file.LinkOption
import java.nio.file.StandardCopyOption
import java.nio.file.attribute.PosixFilePermissions

/**
 * Settings → App → Game files: Import files… (picked files into a folder of the game folder) and
 * Export files… (files and folders of the game folder into a folder the player picks), any file
 * alike. Import refuses while the game runs or the Launcher
 * works on the game folder, as [GameFilesProvider] does. Blocking: call off the main thread.
 */
object FileTransfer {
    /** For a file of the same name already in the destination. */
    enum class Choice { OVERWRITE, SKIP, KEEP_BOTH }

    sealed interface Outcome {
        data class Refused(val refusal: DocumentPaths.Refusal) : Outcome
        data class Done(val copied: Int, val skipped: Int) : Outcome
        data class Failed(val reason: String) : Outcome
    }

    /** `name (2).ext`, `name (3).ext` and on: the first that is not [taken]. */
    fun keepBothName(name: String, taken: (String) -> Boolean): String {
        val dot = name.lastIndexOf('.').takeIf { it > 0 } ?: name.length
        return generateSequence(2) { it + 1 }.map { "${name.substring(0, dot)} ($it)${name.substring(dot)}" }.first { !taken(it) }
    }

    /**
     * What an export of [ids] writes: each file and folder below them, by its path relative to
     * its selected item's folder, folders before what they hold. Only what [documents] lists
     * (links out of the game folder and the Launcher's records are not); the Save link is
     * followed, so Save exports the characters.
     */
    fun exportEntries(documents: DocumentPaths, ids: List<String>): List<Pair<String, File>> {
        val entries = mutableListOf<Pair<String, File>>()
        fun add(id: String, path: String, depth: Int) {
            val file = documents.file(id)
            entries += path to file
            if (file.isDirectory && depth < MAX_DEPTH) {
                for (child in documents.children(id)) add(child, "$path/${child.substringAfterLast('/')}", depth + 1)
            }
        }
        for (id in ids) add(id, id.substringAfterLast('/'), 0)
        return entries
    }

    /**
     * Copies the picked [uris] into [folderId]; [ask] decides for each one whose name is taken.
     * A file is written through a temporary name, so the game never sees half of it.
     */
    suspend fun import(context: Context, uris: List<Uri>, folderId: String, ask: suspend (String) -> Choice): Outcome {
        val documents = GameFilesProvider.documents(context)
        return try {
            val refusal = documents.refusal(folderId, GameFilesProvider.gameRunning(), GameFilesProvider.busy())
            if (refusal != null) return Outcome.Refused(refusal)
            val folder = documents.file(folderId)
            var copied = 0
            var skipped = 0
            for (uri in uris) {
                val picked = displayName(context, uri) ?: throw IOException("a picked file has no name")
                var name = documents.child(folderId, picked).substringAfterLast('/')
                if (Files.exists(File(folder, name).toPath(), LinkOption.NOFOLLOW_LINKS)) {
                    when (ask(name)) {
                        Choice.SKIP -> { skipped++; continue }
                        Choice.KEEP_BOTH -> name = keepBothName(name) { Files.exists(File(folder, it).toPath(), LinkOption.NOFOLLOW_LINKS) }
                        Choice.OVERWRITE -> Unit
                    }
                }
                // Asked: the game may have started meanwhile.
                documents.refusal(folderId, GameFilesProvider.gameRunning(), GameFilesProvider.busy())?.let { return Outcome.Refused(it) }
                val target = File(folder, name)
                if (target.isDirectory) throw IOException("$name is a folder")
                val temporary = File(folder, ".$name$TEMPORARY")
                try {
                    Files.deleteIfExists(temporary.toPath())
                    Files.createFile(temporary.toPath(), PosixFilePermissions.asFileAttribute(PosixFilePermissions.fromString("rw-------")))
                    val input = context.contentResolver.openInputStream(uri) ?: throw IOException("cannot read $picked")
                    input.use { from -> Files.newOutputStream(temporary.toPath()).use { from.copyTo(it) } }
                    Files.move(temporary.toPath(), target.toPath(), StandardCopyOption.REPLACE_EXISTING, StandardCopyOption.ATOMIC_MOVE)
                } finally {
                    Files.deleteIfExists(temporary.toPath())
                }
                copied++
            }
            Outcome.Done(copied, skipped)
        } catch (error: Exception) {
            Outcome.Failed(error.message ?: error.javaClass.simpleName)
        }
    }

    /** Copies [ids] of the game folder ([exportEntries]) into the folder [tree] the player picked. */
    fun export(context: Context, ids: List<String>, tree: Uri): Outcome {
        return try {
            val entries = exportEntries(GameFilesProvider.documents(context), ids)
            val resolver = context.contentResolver
            val folders = mutableMapOf("" to DocumentsContract.buildDocumentUriUsingTree(tree, DocumentsContract.getTreeDocumentId(tree)))
            var copied = 0
            for ((path, file) in entries) {
                val parent = folders.getValue(path.substringBeforeLast('/', ""))
                val name = path.substringAfterLast('/')
                if (file.isDirectory) {
                    folders[path] = DocumentsContract.createDocument(resolver, parent, DocumentsContract.Document.MIME_TYPE_DIR, name)
                        ?: throw IOException("cannot create $name")
                    continue
                }
                val document = DocumentsContract.createDocument(resolver, parent, "application/octet-stream", name)
                    ?: throw IOException("cannot create $name")
                val output = resolver.openOutputStream(document, "w") ?: throw IOException("cannot write $name")
                output.use { to -> file.inputStream().use { it.copyTo(to) } }
                copied++
            }
            Outcome.Done(copied, 0)
        } catch (error: Exception) {
            Outcome.Failed(error.message ?: error.javaClass.simpleName)
        }
    }

    private fun displayName(context: Context, uri: Uri): String? =
        context.contentResolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)?.use { cursor ->
            if (cursor.moveToFirst()) cursor.getString(0) else null
        }

    private const val TEMPORARY = ".pd2import"
    private const val MAX_DEPTH = 32
}
