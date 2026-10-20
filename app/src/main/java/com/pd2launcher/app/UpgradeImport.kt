package com.pd2launcher.app

import com.pd2launcher.app.control.Command
import com.pd2launcher.app.control.Installation
import com.pd2launcher.app.control.Installer
import com.pd2launcher.app.control.Snapshot
import java.io.File
import java.nio.file.Path

/**
 * The Import that follows an update in place from the FEX line that failed
 * ([LegacyPrefix.setAside]): the older installation's game folder waits in
 * [SessionPaths.upgradeGame]. The session service hands it to the engine as the existing Diablo II
 * while nothing of the installation is set up yet, so Install imports it with one press and
 * nothing is downloaded: again at each start (the system's folder picker cannot reach it), until
 * the player chooses another source ([DECLINED], [declines]). Once the installation is Ready the
 * older prefix goes, and so does the waiting folder unless the installation uses it.
 * An update in place that passed, whose moved game the engine's audit then rejects, gets the same
 * Import as Repair's first choice ([offersImport], [LegacyPrefix.takeOut]); Start over comes second.
 * Until that first Ready a needs_client goes there too, not to Play's Check files
 * ([Onboarding.installed]'s updatedInPlace).
 */
object UpgradeImport {
    /** [SessionPaths.importedGame]'s token for the waiting folder. */
    const val TOKEN = "upgrade"

    /** Beside the waiting folder, in its token folder: the player chose another source ([declines]). */
    const val DECLINED = ".declined"

    /**
     * Whether [next] calls for handing the waiting folder [kept] to the engine: the folder is there
     * ([keptThere]), the player did not choose another source ([declined]), the engine has audited
     * the installation and nothing of it is set up (needs_base_game), no job runs, and the engine
     * has not this folder as the existing Diablo II already.
     */
    fun selects(next: Snapshot, kept: File, keptThere: Boolean, declined: Boolean): Boolean =
        keptThere && !declined && Onboarding.audited(next) && next.activeJob == null &&
            next.installation == Installation.NEEDS_BASE_GAME &&
            (next.installer as? Installer.Existing)?.path?.let { File(it).canonicalFile } != kept.canonicalFile

    /**
     * Whether [command] chooses another source than the waiting folder [kept]: Blizzard's download,
     * or another existing folder. The app's own choice of [kept] is not one.
     */
    fun declines(command: Command, kept: File): Boolean = when (command) {
        is Command.UseBlizzardDownload -> true
        is Command.SelectExistingInstall -> File(command.path).canonicalFile != kept.canonicalFile
        else -> false
    }

    /**
     * Whether Repair offers Import of the moved game first: the app's record says installed
     * ([SetupRecord.INSTALLED]: Play or Repair), an update in place moved the game and the
     * installation was not Ready since ([updated], [LegacyPrefix.UPDATED]), the managed game folder
     * is there ([gameThere]), and the engine's audit, with nothing running, finds the installation
     * not Ready (damaged or not known as well: the moved game is the player's either way).
     */
    fun offersImport(snapshot: Snapshot?, record: SetupRecord, updated: Boolean, gameThere: Boolean): Boolean =
        record == SetupRecord.INSTALLED && updated && gameThere && snapshot != null && Onboarding.audited(snapshot) &&
            snapshot.activeJob == null && snapshot.installation != Installation.READY

    /**
     * Whether an imported installation's game came from a folder the player picked, so the notice
     * after the first game and Settings → App → Storage speak of it (one from Android's own storage
     * can go; for Lite's folder, Lite still has its own copy; [InstallerDownloads.importedFolderShown]): not
     * when the game in use is the waiting folder [kept] and the player never picked a folder
     * ([picked], [SourceImport.pickedFolder]): that game is the older app's own install, and
     * nothing was copied. The game in use is the one the engine names ([gameFolder]), else the
     * Launcher's record ([recorded], [GameFilesProvider.recordedGameFolder]): the engine names a
     * folder an install starts to use in place only from its next start.
     */
    fun playerCopied(gameFolder: String?, recorded: String?, kept: File, picked: String?): Boolean {
        val inUse = gameFolder ?: recorded
        return picked != null || inUse == null || File(inUse).canonicalFile != kept.canonicalFile
    }

    /**
     * What goes once the installation is Ready ([AppRuntime.Sweep.REMOVE]): the older prefix that
     * waited ([LegacyPrefix.removeOld], its characters kept first), the mark of an update in place
     * ([LegacyPrefix.UPDATED]), and the waiting folder's token folder unless [inUse] (the game
     * folders the engine and the Launcher's record name) holds it, or the Launcher's record is
     * there but cannot be read (it could name that folder). Returns what went.
     */
    fun cleanUp(
        paths: SessionPaths,
        inUse: Collection<String?>,
        log: (String) -> Unit,
        syncDirectory: (Path) -> Unit = ::fsyncDirectory,
    ): List<File> {
        val gone = ArrayList<File>()
        val old = File(paths.managedRoot, LegacyPrefix.OLD)
        if (old.exists()) {
            LegacyPrefix(
                paths.managedRoot, File(paths.managedRoot, AppRuntime.PREFIX_TEMPLATE), paths.userSave, log,
                syncDirectory = syncDirectory,
            ).removeOld()
            gone += old
        }
        val updated = File(paths.managedRoot, LegacyPrefix.UPDATED)
        if (updated.delete()) gone += updated
        val token = paths.upgradeGame.parentFile!!
        val record = File(paths.managedRoot, "prefix/${LegacyPrefix.POINTER}")
        val unreadable = record.exists() && LegacyPrefix.recordedPath(record) == null
        if (unreadable && token.exists()) log("after the update's Import: the game folder record cannot be read; ${token.name} stays")
        if (token.exists() && !unreadable && inUse.none { it != null && File(it).canonicalFile.parentFile == token.canonicalFile }) {
            UserlandInstaller.deleteTree(token.toPath())
            gone += token
        }
        if (gone.isNotEmpty()) log("after the update's Import: removed ${gone.joinToString { it.name }}")
        return gone
    }
}
