package com.pd2launcher.app

import androidx.annotation.StringRes
import com.pd2launcher.app.control.Game
import com.pd2launcher.app.control.Installation
import com.pd2launcher.app.control.Snapshot
import java.io.File
import java.io.IOException
import java.nio.file.FileVisitResult
import java.nio.file.Files
import java.nio.file.LinkOption
import java.nio.file.Path
import java.nio.file.SimpleFileVisitor
import java.nio.file.attribute.BasicFileAttributes
import java.util.Locale

/**
 * The game downloads the player may remove once the game works:
 * the game files Blizzard's downloaders fetched and the Project Diablo 2 client download cache.
 * Kept, they let a reinstall reuse them. Nothing removes them by itself: the one-time notice on
 * Play and Settings → App → Storage run the engine's `--remove-downloads`, which removes these
 * folders (the engine's `INSTALLER_DOWNLOADS`); the notice waits for the first game
 * session to end ([showNotice]). The prefix's Windows Installer cache is not here: the app
 * removes it by itself ([InstallerCache]). After an import from Android's own storage the notice
 * also says the Diablo II folder the player copied onto the device can go: the app has its own
 * copy, and it never deletes the player's folder itself. After an import of Lite's game folder it
 * says that Lite still has its own copy ([notice]).
 */
object InstallerDownloads {
    /** The installers of Diablo II and Lord of Destruction that Blizzard's downloaders fetched, below the Launcher's managed root. */
    val INSTALLERS = listOf("cache/artifacts/d2-payload", "cache/artifacts/lod-payload")

    /**
     * The Project Diablo 2 client files that the Download step keeps, below the managed root (the
     * engine's `CLIENT_PREFETCH`): the files it downloaded and, after an import, the files of the
     * folder's own client that matched, copied there instead of downloaded.
     */
    const val CLIENT = "cache/client"

    /** Below the Launcher's managed root: all that Remove removes. */
    val PATHS = INSTALLERS + CLIENT

    /**
     * What the downloads hold, in bytes: the [installers] of Diablo II and Lord of Destruction,
     * and the Project Diablo 2 [client] files.
     */
    data class Downloads(val installers: Long, val client: Long) {
        val bytes: Long get() = installers + client

        /**
         * Only the Project Diablo 2 files, as after an import: the game came with the folder, so
         * there are no installers, and the words say so.
         */
        val clientOnly: Boolean get() = installers == 0L && client > 0
    }

    /**
     * The engine's record that the installation was imported, below the managed root
     * (the engine's `IMPORT_RECEIPT_NAME`): written as the import ends, gone with Start over.
     */
    const val IMPORT_RECORD = "prefix/.pd2launcher-imported-game.json"

    enum class Refusal {
        /** The game is not installed yet: the downloads are still needed. */
        NOT_READY,
        /** An install, an update, a check or a repair runs. */
        BUSY,
        GAME_RUNNING,
    }

    sealed interface Outcome {
        data object Removed : Outcome
        data class Refused(val refusal: Refusal) : Outcome
        data class Failed(val reason: String) : Outcome
    }

    /** Bytes of every file below [PATHS] in [managedRoot]; links are not followed. */
    fun bytes(managedRoot: File): Long = downloads(managedRoot).bytes

    /** What [PATHS] in [managedRoot] hold, by kind ([Downloads]); links are not followed. */
    fun downloads(managedRoot: File): Downloads = Downloads(
        installers = INSTALLERS.sumOf { treeBytes(File(managedRoot, it).toPath()) },
        client = treeBytes(File(managedRoot, CLIENT).toPath()),
    )

    /** Settings → App → Storage's words: the row's [title], its line with the size ([body]) and the question before Remove ([confirm]). */
    data class Words(@param:StringRes val title: Int, @param:StringRes val body: Int, @param:StringRes val confirm: Int)

    /**
     * The words for [downloads]: the installers, or only the Project Diablo 2 files
     * ([Downloads.clientOnly]). While nothing is counted (still counting, or nothing left after
     * Remove) the engine's import record says which ([imported]): an import has no installers.
     */
    fun words(downloads: Downloads?, imported: Boolean): Words = if (downloads?.takeIf { it.bytes > 0 }?.clientOnly ?: imported) {
        Words(R.string.settings_downloads_client, R.string.settings_downloads_client_body, R.string.settings_downloads_confirm_client)
    } else {
        Words(R.string.settings_downloads, R.string.settings_downloads_body, R.string.settings_downloads_confirm)
    }

    /** Whether the installation in [managedRoot] was imported ([IMPORT_RECORD]). */
    fun imported(managedRoot: File): Boolean = File(managedRoot, IMPORT_RECORD).isFile

    /** Why removing is refused now, or null when it may run. */
    fun refusal(snapshot: Snapshot?): Refusal? = when {
        snapshot?.game == Game.RUNNING -> Refusal.GAME_RUNNING
        snapshot?.activeJob != null -> Refusal.BUSY
        snapshot?.installation != Installation.READY -> Refusal.NOT_READY
        else -> null
    }

    /**
     * The notice on Play shows once, after the first game session: never on the first arrival
     * right after the install, only once a game has run and ended ([gamePlayed]; the player has
     * tried the game, [played]), while the installation is Ready with downloads to remove or
     * [imported] (a folder from Android's own storage can go, or Lite still has its own copy, even
     * with nothing downloaded; [importedFolderShown]), until the player
     * answers it (Remove, Later or OK; [answered]). Start over forgets both answers
     * ([AppSettings.afterStartOver]).
     */
    fun showNotice(snapshot: Snapshot?, answered: Boolean, gamePlayed: Boolean, bytes: Long?, imported: Boolean = false): Boolean =
        !answered && gamePlayed && bytes != null && (bytes > 0 || imported) && refusal(snapshot) == null

    /** How long a game session runs before it counts as played ([played]). */
    const val PLAYED_MS = 30_000L

    /**
     * A game session that started at [startedAt] (null: not seen starting) and ended at [endedAt]
     * counts as played: it ran at least [PLAYED_MS]; one that ended at once (a crash at start) does not.
     */
    fun played(startedAt: Long?, endedAt: Long): Boolean = startedAt != null && endedAt - startedAt >= PLAYED_MS

    /**
     * What the notice says: its [title], its paragraphs, and [remove], the button that frees the
     * downloads; null when there are none (an import with nothing downloaded: OK only).
     */
    data class Notice(val title: Line, val body: List<Line>, val remove: Line?)

    /**
     * Where an imported folder came from, by the documents provider [authority] that gave it
     * ([SourceImport.pickedFrom]): Android's own storage ([SourceImport.SHARED_STORAGE], also a USB
     * drive or an SD card, or [SourceImport.DOWNLOADS]), PD2 Launcher Lite's game folder, or
     * another app's. A folder recorded without its provider (by an older version) counts as
     * Android's own storage, as it did then.
     */
    enum class FolderSource { SHARED_STORAGE, LITE, OTHER }

    fun folderSource(authority: String?): FolderSource = when (authority) {
        null, SourceImport.SHARED_STORAGE, SourceImport.DOWNLOADS -> FolderSource.SHARED_STORAGE
        in LITE_FOLDERS -> FolderSource.LITE
        else -> FolderSource.OTHER
    }

    /** The game folders of PD2 Launcher Lite and of its development build, as their providers name them. */
    private val LITE_FOLDERS = setOf("com.pd2launcher.lite.documents", "com.pd2launcher.lite.dev.documents")

    /**
     * Whether the notice and Settings → App → Storage speak of the imported folder: only after an
     * import ([imported]) from Android's own storage or from Lite ([from]). After an import from
     * another app's folder they say nothing about it: the notice shows only for downloads to free.
     */
    fun importedFolderShown(imported: Boolean, from: String?): Boolean = imported && folderSource(from) != FolderSource.OTHER

    /**
     * The notice for [bytes] of downloads, which are only the Project Diablo 2 files when
     * [clientOnly] ([Downloads.clientOnly]); [imported] adds what to know about the folder the
     * player picked ([importedFolderShown]), by [folder] where the app knows it
     * ([SourceImport.pickedFolder]) and by the provider it came from ([from], [folderSource]). Only
     * a folder from Android's own storage is one the player copied onto the device, so only then
     * does the notice say that the folder can be deleted; for Lite's folder it says that Lite still
     * has its own copy.
     */
    fun notice(bytes: Long, imported: Boolean, folder: String?, from: String?, clientOnly: Boolean = false): Notice {
        // Another app's folder: no word on it, whatever the caller says ([importedFolderShown]).
        val shown = importedFolderShown(imported, from)
        val size = size(bytes)
        val downloads = bytes > 0
        val copied = when {
            !shown -> null
            folderSource(from) == FolderSource.LITE -> Line(R.string.downloads_notice_lite)
            downloads -> folder?.let { Line(R.string.downloads_notice_folder, it) } ?: Line(R.string.downloads_notice_folder_unnamed)
            else -> folder?.let { Line(R.string.downloads_notice_folder_only, it) } ?: Line(R.string.downloads_notice_folder_only_unnamed)
        }
        val body = if (clientOnly) R.string.downloads_notice_body_client else R.string.downloads_notice_body
        return Notice(
            title = if (shown) Line(R.string.downloads_notice_title_import) else Line(R.string.downloads_notice_title, size),
            body = listOfNotNull(Line(body, size).takeIf { downloads }, copied),
            remove = Line(R.string.downloads_notice_remove, size).takeIf { downloads },
        )
    }

    /**
     * Settings → App → Storage's line for an imported installation: a folder from Android's own
     * storage can be deleted; for Lite's folder, Lite still has its own copy; for another app's
     * folder, none ([importedFolderShown], [from]).
     */
    fun copiedFolder(folder: String?, from: String?): Line? = when {
        !importedFolderShown(true, from) -> null
        folderSource(from) == FolderSource.LITE -> Line(R.string.settings_copied_folder_lite)
        else -> folder?.let { Line(R.string.settings_copied_folder, it) } ?: Line(R.string.settings_copied_folder_unnamed)
    }

    /** The engine's `--remove-downloads` exit status as an outcome. */
    fun outcome(exitCode: Int?): Outcome = when (exitCode) {
        0 -> Outcome.Removed
        2 -> Outcome.Refused(Refusal.BUSY)
        else -> Outcome.Failed(if (exitCode == null) "it did not finish" else "exit status $exitCode")
    }

    /** `2.4 GB` (decimal, as Android's storage settings count), `120 MB` below one GB. */
    fun size(bytes: Long): String =
        if (bytes >= 1_000_000_000L) {
            String.format(Locale.US, "%.1f GB", bytes / 1e9)
        } else {
            String.format(Locale.US, "%d MB", (bytes + 500_000) / 1_000_000)
        }

    private fun treeBytes(path: Path): Long {
        if (!Files.isDirectory(path, LinkOption.NOFOLLOW_LINKS)) return 0
        var total = 0L
        Files.walkFileTree(path, object : SimpleFileVisitor<Path>() {
            override fun visitFile(file: Path, attrs: BasicFileAttributes): FileVisitResult {
                if (attrs.isRegularFile) total += attrs.size()
                return FileVisitResult.CONTINUE
            }

            override fun visitFileFailed(file: Path, exc: IOException): FileVisitResult = FileVisitResult.CONTINUE
        })
        return total
    }
}
