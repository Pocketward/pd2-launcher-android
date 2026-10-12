package com.pd2launcher.app

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
 * session to end ([showNotice]). The environment dependency installers
 * (Gecko, Mono, .NET, the prefix's Windows Installer cache) are not here: the app removes those
 * by itself ([AppRuntime.removeArchives], [InstallerCache]). After an import the notice also says
 * the Diablo II folder the player copied onto the device can go: the app has its own copy, and it
 * never deletes the player's folder itself.
 */
object InstallerDownloads {
    /** Below the Launcher's managed root. */
    val PATHS = listOf("cache/artifacts/d2-payload", "cache/artifacts/lod-payload", "cache/client")

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
    fun bytes(managedRoot: File): Long = PATHS.sumOf { treeBytes(File(managedRoot, it).toPath()) }

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
     * [imported] (its copied folder can go, even with nothing downloaded), until the player
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
     * The notice for [bytes] of downloads; [imported] adds the folder the player copied onto the
     * device from their PC, by [folder] where the app knows it ([SourceImport.pickedFolder]).
     */
    fun notice(bytes: Long, imported: Boolean, folder: String?): Notice {
        val size = size(bytes)
        val downloads = bytes > 0
        val copied = when {
            !imported -> null
            downloads -> folder?.let { Line(R.string.downloads_notice_folder, it) } ?: Line(R.string.downloads_notice_folder_unnamed)
            else -> folder?.let { Line(R.string.downloads_notice_folder_only, it) } ?: Line(R.string.downloads_notice_folder_only_unnamed)
        }
        return Notice(
            title = if (imported) Line(R.string.downloads_notice_title_import) else Line(R.string.downloads_notice_title, size),
            body = listOfNotNull(Line(R.string.downloads_notice_body, size).takeIf { downloads }, copied),
            remove = Line(R.string.downloads_notice_remove, size).takeIf { downloads },
        )
    }

    /** Settings → App → Storage's line for an imported installation: the copied folder can go. */
    fun copiedFolder(folder: String?): Line =
        folder?.let { Line(R.string.settings_copied_folder, it) } ?: Line(R.string.settings_copied_folder_unnamed)

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
