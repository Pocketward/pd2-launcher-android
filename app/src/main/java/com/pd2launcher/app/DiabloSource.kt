package com.pd2launcher.app

import com.pd2launcher.app.control.Command
import com.pd2launcher.app.control.ExistingProblem
import com.pd2launcher.app.control.ImportMode
import com.pd2launcher.app.control.Installer
import com.pd2launcher.app.control.Snapshot

/**
 * Step 2's two ways to Diablo II. The engine also takes installer files on the desktop; the
 * Android app does not offer them.
 */
enum class SourceChoice {
    /** Everything is downloaded: Diablo II and LoD from Blizzard, Project Diablo 2 (the default). */
    BLIZZARD,
    /** The player's Diablo II + LoD folder, picked with the system picker and used in place. */
    EXISTING,
}

/** The app's part of [SourceChoice.EXISTING]: copying the picked folder in. */
sealed interface SourceCopy {
    data object Idle : SourceCopy

    data class Copying(val done: Long, val total: Long) : SourceCopy

    /** The pick needs [needed] bytes in the app's folder and [free] are left. */
    data class NoSpace(val needed: Long, val free: Long) : SourceCopy

    data class Failed(val reason: String) : SourceCopy

    /** A Diablo II folder refused by its file list or by the engine; nothing of it is kept. */
    data class Refused(val problem: ExistingProblem) : SourceCopy

    /** A Diablo II folder copied in and handed to the engine, holding [projectDiablo]. */
    data class Copied(val projectDiablo: ProjectDiabloFound) : SourceCopy
}

/** The Project Diablo 2 a picked Diablo II folder holds. */
enum class ProjectDiabloFound {
    NONE,
    /** One this Launcher installed and keeps up to date: Install adopts it, no PD2 installer runs. */
    LAUNCHER,
    /** Another one (the official launcher's): Install updates it. */
    OTHER,
}

/**
 * A picked Diablo II folder judged by its file list alone, before anything is copied: the
 * files Diablo II and Lord of Destruction need must be there and not empty (the engine checks
 * the version once the copy is in), and its ProjectD2 tells whether the PD2 installer runs.
 */
object ExistingFolder {
    /** As the engine's lists of the same names: what a complete Diablo II and Lord of Destruction need. */
    private val BASE_FILES = listOf(
        "Game.exe", "Patch_D2.mpq", "Diablo II.exe", "d2char.mpq", "d2data.mpq", "d2music.mpq",
        "d2sfx.mpq", "d2speech.mpq", "d2video.mpq",
    )
    private val EXPANSION_FILES = listOf("d2exp.mpq", "d2xmusic.mpq", "d2xtalk.mpq", "d2xvideo.mpq")
    private const val PROJECT_D2 = "ProjectD2"
    /** The update records the Launcher keeps in a ProjectD2 it installed. */
    private const val ACCEPTED_MANIFEST = ".pd2launcher/accepted-manifest.json"

    sealed interface Check {
        data class Usable(val projectDiablo: ProjectDiabloFound) : Check
        data class Refused(val problem: ExistingProblem) : Check
    }

    /** [files]: each file's '/'-separated path below the picked folder and its size. */
    fun check(files: Map<String, Long>): Check {
        val top = files.entries.filter { '/' !in it.key }.associate { it.key.lowercase() to it.value }
        if ("game.exe" !in top && "d2data.mpq" !in top) return Check.Refused(ExistingProblem.NotDiablo)
        if ("d2exp.mpq" !in top) return Check.Refused(ExistingProblem.NoExpansion)
        for (name in BASE_FILES + EXPANSION_FILES) {
            // An empty file is what a copy that stopped part way leaves.
            if ((top[name.lowercase()] ?: 0L) <= 0L) return Check.Refused(ExistingProblem.MissingFile(name))
        }
        val projectFiles = files.keys.map(::canonical).filter { it.startsWith("$PROJECT_D2/") }
        val projectDiablo = when {
            projectFiles.isEmpty() -> ProjectDiabloFound.NONE
            "$PROJECT_D2/$ACCEPTED_MANIFEST" in projectFiles -> ProjectDiabloFound.LAUNCHER
            else -> ProjectDiabloFound.OTHER
        }
        return Check.Usable(projectDiablo)
    }

    /**
     * Where [relative] goes in the copy: a top-level name the game needs, or ProjectD2, takes the
     * Launcher's spelling (Windows ignores case, the Launcher does not), so the copy can always
     * be used in place.
     */
    fun canonical(relative: String): String {
        val first = relative.substringBefore('/')
        val name = (BASE_FILES + EXPANSION_FILES + PROJECT_D2).firstOrNull { it.equals(first, ignoreCase = true) } ?: first
        return name + relative.substring(first.length)
    }
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
     * The choice the engine's [installer] stands for; null when none is made yet, and for installer
     * files, which the Android app does not offer (Continue then selects the download).
     */
    fun choiceOf(installer: Installer): SourceChoice? = when (installer) {
        Installer.NotSelected, is Installer.Folder, is Installer.Invalid -> null
        Installer.BlizzardDownload -> SourceChoice.BLIZZARD
        is Installer.Existing -> SourceChoice.EXISTING
    }

    fun view(selected: SourceChoice, session: SessionStatus, copy: SourceCopy): View {
        val snapshot = session.snapshot?.takeIf { session.engine == SessionStatus.Engine.CONNECTED }
        fun view(tone: Tone, status: Line, canContinue: Boolean = false, progress: Float? = null) =
            View(selected, tone, status, canContinue, continuePicks = false, progress = progress)
        // No folder accepted: Continue opens the picker (again), as Choose Diablo II folder does.
        fun pick(tone: Tone, status: Line) = View(selected, tone, status, canContinue = true, continuePicks = true, progress = null)

        if (selected != SourceChoice.BLIZZARD) {
            when (copy) {
                is SourceCopy.Copying -> return view(
                    Tone.WORKING,
                    Line(R.string.source_copying, copy.done shr 20, copy.total shr 20),
                    progress = if (copy.total > 0) (copy.done.toFloat() / copy.total).coerceIn(0f, 1f) else null,
                )
                is SourceCopy.NoSpace -> return pick(Tone.ERROR, Line(R.string.source_no_space, copy.needed shr 20, copy.free shr 20))
                is SourceCopy.Failed -> return pick(Tone.ERROR, Line(R.string.source_copy_failed, copy.reason))
                is SourceCopy.Refused -> return pick(Tone.ERROR, problem(copy.problem))
                SourceCopy.Idle, is SourceCopy.Copied -> {}
            }
        }
        if (snapshot == null) return view(Tone.PENDING, Line(R.string.source_waiting))
        val installer = snapshot.installer
        return when (selected) {
            SourceChoice.BLIZZARD -> view(Tone.OK, Line(R.string.source_blizzard_ready), canContinue = true)
            SourceChoice.EXISTING -> when {
                installer !is Installer.Existing -> pick(Tone.PENDING, Line(R.string.source_existing_pick))
                installer.problem != null -> pick(Tone.ERROR, problem(installer.problem))
                installer.inPlacePossible -> view(Tone.OK, inPlace((copy as? SourceCopy.Copied)?.projectDiablo), canContinue = true)
                else -> view(Tone.OK, Line(R.string.source_existing_copy, installer.copyBytes shr 20), canContinue = true)
            }
        }
    }

    /** What Continue sends for [selected]; the picks were already sent once copied. */
    fun continueCommand(selected: SourceChoice, installer: Installer?): Command? =
        if (selected == SourceChoice.BLIZZARD && installer != Installer.BlizzardDownload) Command.UseBlizzardDownload else null

    /** The command that hands a finished folder copy in [folder] to the engine. */
    fun selectCommand(folder: String): Command = Command.SelectExistingInstall(folder)

    /** In place when the Launcher can use the folder where it is (no second copy), else a copy. */
    fun importMode(existing: Installer.Existing): ImportMode =
        if (existing.inPlacePossible) ImportMode.IN_PLACE else ImportMode.COPY

    /** Install for an existing folder imports it instead of asking for keys. */
    fun importsExisting(snapshot: Snapshot): Boolean =
        snapshot.installer.let { it is Installer.Existing && it.problem == null }

    private fun inPlace(projectDiablo: ProjectDiabloFound?): Line = Line(
        when (projectDiablo) {
            ProjectDiabloFound.LAUNCHER -> R.string.source_existing_with_pd2
            ProjectDiabloFound.OTHER -> R.string.source_existing_update_pd2
            ProjectDiabloFound.NONE, null -> R.string.source_existing_in_place
        },
    )

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
