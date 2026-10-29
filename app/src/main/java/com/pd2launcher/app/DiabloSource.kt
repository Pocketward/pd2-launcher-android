package com.pd2launcher.app

import com.pd2launcher.app.control.Command
import com.pd2launcher.app.control.ExistingProblem
import com.pd2launcher.app.control.ImportMode
import com.pd2launcher.app.control.InstallSource
import com.pd2launcher.app.control.Job
import com.pd2launcher.app.control.Installer
import com.pd2launcher.app.control.SettingChange
import com.pd2launcher.app.control.Snapshot

/**
 * Step 2's three ways to Diablo II, in the page's order. The engine also takes installer files on
 * the desktop; the Android app does not offer them.
 */
enum class SourceChoice {
    /**
     * Fast download (the default): the engine fetches Diablo II and Lord of Destruction from
     * Blizzard and installs them itself, with no Wine (the engine's `direct_download` source).
     */
    FAST,
    /** Blizzard's installers: Blizzard's own downloader and installers run under Wine (`official`). */
    BLIZZARD,
    /** The player's Diablo II + LoD folder, picked with the system picker and copied into the app. */
    EXISTING,
    ;

    companion object {
        /** The choice for a new install. */
        val DEFAULT = FAST

        /** The choice saved as [name] in the activity's saved state; null for none, or for a name this version does not know. */
        fun named(name: String?): SourceChoice? = entries.firstOrNull { it.name == name }
    }
}

/** The app's part of [SourceChoice.EXISTING]: copying the picked folder in. */
sealed interface SourceCopy {
    data object Idle : SourceCopy

    data class Copying(val done: Long, val total: Long) : SourceCopy

    /** The pick needs [needed] bytes in the app's folder and [free] are left. */
    data class NoSpace(val needed: Long, val free: Long) : SourceCopy

    data class Failed(val reason: String) : SourceCopy

    /** A Diablo II folder refused by the engine; nothing of it is kept. */
    data class Refused(val problem: ExistingProblem) : SourceCopy

    /**
     * A picked folder that lacks [files] (of [ExistingFolder.IMPORT_FILES]): it is not copied,
     * and the import does not start.
     */
    data class Missing(val files: List<String>) : SourceCopy

    /** A Diablo II folder copied in and handed to the engine, holding [projectDiablo]. */
    data class Copied(val projectDiablo: ProjectDiabloFound) : SourceCopy
}

/** The Project Diablo 2 a picked Diablo II folder holds: the import keeps it as it is. */
enum class ProjectDiabloFound {
    /** One that a PD2 Launcher recorded (`ProjectD2/.pd2launcher`): the import records it again. */
    LAUNCHER,
    /** Another one (the official launcher's, of any version, or modded). */
    OTHER,
}

/**
 * A picked Diablo II folder judged by its file list alone, before anything is copied (the owner's
 * import rule of 2026-10-26): the important files of Diablo II, Lord of Destruction and Project
 * Diablo 2 ([IMPORT_FILES]) must be there and not empty, names in any case, with no version check.
 * Every missing one is named, and the import does not start. The engine checks the copy's files
 * with the same list once it is in, records the copied Project Diablo 2 as it is, and never updates
 * it: Play's update check offers the update.
 */
object ExistingFolder {
    /** As the engine's lists of the same names: the files Windows names of Diablo II and Lord of Destruction. */
    private val BASE_FILES = listOf(
        "Game.exe", "Patch_D2.mpq", "Diablo II.exe", "d2char.mpq", "d2data.mpq", "d2music.mpq",
        "d2sfx.mpq", "d2speech.mpq", "d2video.mpq",
    )
    private val EXPANSION_FILES = listOf("d2exp.mpq", "d2xmusic.mpq", "d2xtalk.mpq", "d2xvideo.mpq")
    private const val PROJECT_D2 = "ProjectD2"
    /** The update records a PD2 Launcher keeps in a ProjectD2 it installed. */
    private const val ACCEPTED_MANIFEST = ".pd2launcher/accepted-manifest.json"

    /**
     * What an import needs (the engine's `ANDROID_IMPORT_FILES`): Diablo II's and Lord of
     * Destruction's programs and main archives, and Project Diablo 2's programs.
     */
    val IMPORT_FILES = listOf(
        "Game.exe", "Diablo II.exe", "d2data.mpq", "d2char.mpq", "d2sfx.mpq", "d2exp.mpq",
        "$PROJECT_D2/Game.exe", "$PROJECT_D2/ProjectDiablo.dll", "$PROJECT_D2/PD2Launcher.exe",
    )

    sealed interface Check {
        data class Usable(val projectDiablo: ProjectDiabloFound) : Check

        /** The files of [IMPORT_FILES] that are not there or are empty, in its order. */
        data class Missing(val files: List<String>) : Check
    }

    /** [files]: each file's '/'-separated path below the picked folder and its size. */
    fun check(files: Map<String, Long>): Check {
        // An empty file is what a copy that stopped part way leaves.
        val sizes = files.entries.associate { canonical(it.key).lowercase() to it.value }
        val missing = IMPORT_FILES.filter { (sizes[it.lowercase()] ?: 0L) <= 0L }
        if (missing.isNotEmpty()) return Check.Missing(missing)
        val recorded = "$PROJECT_D2/$ACCEPTED_MANIFEST".lowercase() in sizes
        return Check.Usable(if (recorded) ProjectDiabloFound.LAUNCHER else ProjectDiabloFound.OTHER)
    }

    /**
     * Where [relative] goes in the copy: a top-level name the game needs, ProjectD2, and Project
     * Diablo 2's programs in it take the Launcher's spelling (Windows ignores case, the Launcher
     * does not), so the copy can always be used in place and its programs are found.
     */
    fun canonical(relative: String): String {
        val first = relative.substringBefore('/')
        val name = (BASE_FILES + EXPANSION_FILES + PROJECT_D2).firstOrNull { it.equals(first, ignoreCase = true) } ?: first
        val rest = relative.substring(first.length)
        if (name == PROJECT_D2) {
            val program = IMPORT_FILES.firstOrNull { it.equals(name + rest, ignoreCase = true) }
            if (program != null) return program
        }
        return name + rest
    }

    /** Whether [file], one of [IMPORT_FILES], is Project Diablo 2's. */
    fun isProjectDiablo(file: String): Boolean = file.startsWith("$PROJECT_D2/")
}

/**
 * Maps the source choice, the engine's installer and the app's copy onto step 2's screen. Pure,
 * so every state is unit tested.
 */
object DiabloSource {
    data class View(
        val selected: SourceChoice,
        val tone: Tone,
        /** How the selected choice stands: under the choices, or beside the copy's figure. */
        val status: Line,
        /** Continue is offered. */
        val canContinue: Boolean,
        /** Continue opens the folder picker: no folder is accepted yet. */
        val continuePicks: Boolean,
        /** 0..1 while the pick is copied. */
        val progress: Float?,
    )

    /**
     * The choice the engine's [snapshot] stands for: its installer, and for Blizzard's download
     * its install source. Null when none is made yet, and for installer files, which the Android
     * app does not offer (Continue then selects the download).
     */
    fun choiceOf(snapshot: Snapshot): SourceChoice? = when (snapshot.installer) {
        Installer.NotSelected, is Installer.Folder, is Installer.Invalid -> null
        Installer.BlizzardDownload ->
            if (snapshot.settings.installSource == InstallSource.DIRECT_DOWNLOAD) SourceChoice.FAST else SourceChoice.BLIZZARD
        is Installer.Existing -> SourceChoice.EXISTING
    }

    /**
     * The row the source page selects: the player's [selected] one, else the row the player last
     * selected before a restart ([saved], [AppSettings.sourceChoice]), else what the engine's
     * [snapshot] stands for, else Fast download. The engine selects Blizzard's download with its
     * own downloader by itself at each start, so that state is no choice of the player's and
     * shows the default.
     */
    fun pageChoice(selected: SourceChoice?, snapshot: Snapshot?, saved: SourceChoice? = null): SourceChoice =
        selected ?: saved ?: snapshot?.let(::choiceOf)?.takeUnless { it == SourceChoice.BLIZZARD } ?: SourceChoice.DEFAULT

    fun view(selected: SourceChoice, session: SessionStatus, copy: SourceCopy): View {
        val snapshot = session.snapshot?.takeIf { session.engine == SessionStatus.Engine.CONNECTED }
        fun view(tone: Tone, status: Line, canContinue: Boolean = false, progress: Float? = null) =
            View(selected, tone, status, canContinue, continuePicks = false, progress = progress)
        // No folder accepted: Continue opens the picker (again), as Choose Diablo II folder does.
        fun pick(tone: Tone, status: Line) = View(selected, tone, status, canContinue = true, continuePicks = true, progress = null)

        if (selected == SourceChoice.EXISTING) {
            when (copy) {
                is SourceCopy.Copying -> return view(
                    Tone.WORKING,
                    Line(R.string.source_copying, copy.done shr 20, copy.total shr 20),
                    progress = if (copy.total > 0) (copy.done.toFloat() / copy.total).coerceIn(0f, 1f) else null,
                )
                is SourceCopy.NoSpace -> return pick(Tone.ERROR, Line(R.string.source_no_space, copy.needed shr 20, copy.free shr 20))
                is SourceCopy.Failed -> return pick(Tone.ERROR, Line(R.string.source_copy_failed, copy.reason))
                is SourceCopy.Refused -> return pick(Tone.ERROR, problem(copy.problem))
                is SourceCopy.Missing -> return pick(Tone.ERROR, missing(copy.files))
                SourceCopy.Idle, is SourceCopy.Copied -> {}
            }
        }
        if (snapshot == null) return view(Tone.PENDING, Line(R.string.source_waiting))
        val installer = snapshot.installer
        // While the removal for a picked folder runs, the engine starts no Download: Continue waits.
        val free = snapshot.activeJob != Job.DISCARD_DOWNLOADS
        return when (selected) {
            SourceChoice.FAST, SourceChoice.BLIZZARD -> view(Tone.OK, Line(R.string.source_blizzard_ready), canContinue = free)
            SourceChoice.EXISTING -> when {
                installer !is Installer.Existing -> pick(Tone.PENDING, Line(R.string.source_existing_pick))
                installer.problem != null -> pick(Tone.ERROR, problem(installer.problem))
                installer.inPlacePossible -> view(Tone.OK, inPlace((copy as? SourceCopy.Copied)?.projectDiablo), canContinue = free)
                else -> view(Tone.OK, Line(R.string.source_existing_copy, installer.copyBytes shr 20), canContinue = free)
            }
        }
    }

    /**
     * The installer Continue selects for [selected]; the picks were already sent once copied.
     * Both downloads use Blizzard's download: only the install source tells them apart
     * ([sourceSetting]), so a change between them downloads nothing again.
     */
    fun continueCommand(selected: SourceChoice, installer: Installer?): Command? =
        if (selected != SourceChoice.EXISTING && installer != Installer.BlizzardDownload) Command.UseBlizzardDownload else null

    /**
     * The install source Continue sets for [selected] when the engine's [current] one differs:
     * `direct_download` for Fast download, `official` for Blizzard's installers. The engine saves
     * it and reads it when Install Diablo II starts. Null for a folder, which needs neither.
     */
    fun sourceSetting(selected: SourceChoice, current: InstallSource?): Command? {
        val wanted = when (selected) {
            SourceChoice.FAST -> InstallSource.DIRECT_DOWNLOAD
            SourceChoice.BLIZZARD -> InstallSource.OFFICIAL
            SourceChoice.EXISTING -> return null
        }
        return if (wanted != current) Command.ChangeSettings(SettingChange.InstallSource(wanted)) else null
    }

    /** The command that hands a finished folder copy in [folder] to the engine. */
    fun selectCommand(folder: String): Command = Command.SelectExistingInstall(folder)

    /** In place when the Launcher can use the folder where it is (no second copy), else a copy. */
    fun importMode(existing: Installer.Existing): ImportMode =
        if (existing.inPlacePossible) ImportMode.IN_PLACE else ImportMode.COPY

    /** Install for an existing folder imports it instead of asking for keys. */
    fun importsExisting(snapshot: Snapshot): Boolean =
        snapshot.installer.let { it is Installer.Existing && it.problem == null }

    /** The engine's refusal of the removal of the downloads' files (an install stage or another job runs). */
    const val DISCARD_REFUSED = "discard-downloads-not-permitted"

    /** What Install does with the folder's Project Diablo 2: the import keeps it as it is, with no update. */
    private fun inPlace(projectDiablo: ProjectDiabloFound?): Line =
        Line(if (projectDiablo != null) R.string.source_existing_keep_pd2 else R.string.source_existing_in_place)

    /**
     * A picked folder that lacks [files]: whether the game's files or Project Diablo 2's are not
     * there, and every missing file by name.
     */
    fun missing(files: List<String>): Line {
        val game = files.any { !ExistingFolder.isProjectDiablo(it) }
        val projectDiablo = files.any { ExistingFolder.isProjectDiablo(it) }
        val text = when {
            game && projectDiablo -> R.string.existing_missing_both
            projectDiablo -> R.string.existing_missing_pd2
            else -> R.string.existing_missing_game
        }
        return Line(text, files.joinToString(", "))
    }

    private fun problem(problem: ExistingProblem): Line = when (problem) {
        ExistingProblem.NotFound -> Line(R.string.existing_not_found)
        ExistingProblem.Unreadable -> Line(R.string.existing_unreadable)
        ExistingProblem.NotDiablo -> Line(R.string.existing_not_diablo)
        is ExistingProblem.UnsupportedVersion -> Line(R.string.existing_unsupported_version, problem.version ?: "?")
        ExistingProblem.ModifiedGame -> Line(R.string.existing_modified)
        ExistingProblem.ArchiveMismatch -> Line(R.string.existing_archive_mismatch)
        ExistingProblem.NoExpansion -> Line(R.string.existing_no_expansion)
        is ExistingProblem.MissingFile -> Line(R.string.existing_missing_file, problem.file)
        ExistingProblem.Links -> Line(R.string.existing_links)
        ExistingProblem.InsideLauncher -> Line(R.string.existing_inside_launcher)
        ExistingProblem.UnsupportedPath -> Line(R.string.existing_unsupported_path)
    }
}
