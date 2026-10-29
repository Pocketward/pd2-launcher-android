package com.pd2launcher.app

import com.pd2launcher.app.control.Installation
import com.pd2launcher.app.control.Installer
import com.pd2launcher.app.control.Job
import com.pd2launcher.app.control.OperationStatus
import com.pd2launcher.app.control.Snapshot
import com.pd2launcher.app.ui.TaskState

/**
 * The key page while Download runs (the owner's design of 2026-10-28): Continue on the source
 * page starts Download (Blizzard's two small downloaders and Project Diablo 2's files) and opens
 * the key page at once. One line there says how Download stands; Agree and install gives the keys
 * at once, and Install Diablo II starts by itself when Download has ended. The game itself
 * (Fast download or Blizzard's installers) downloads only after Agree and install. Pure, so every
 * state is unit tested.
 */
object KeysWhileDownloading {
    private val NEEDS_INSTALLERS = setOf(Installation.NEEDS_BASE_GAME, Installation.NEEDS_EXPANSION)

    /** How long the key page waits for Continue's own start of Download before it starts it ([startsOnKeyPage]). */
    const val START_WAIT_MS = 3_000L

    /**
     * Continue with [selected] opens the key page at once: a download of Diablo II (Fast download
     * or Blizzard's installers) while Diablo II or Lord of Destruction is still to install. The
     * engine's [snapshot] may still name the earlier installer; Continue selects the download.
     */
    fun keysAtOnce(selected: SourceChoice, snapshot: Snapshot?): Boolean =
        selected != SourceChoice.EXISTING && snapshot != null && snapshot.installation in NEEDS_INSTALLERS

    /** The source downloads Diablo II (Fast download or Blizzard's installers), which is still to install. */
    fun downloadsFirst(snapshot: Snapshot?): Boolean =
        snapshot != null && snapshot.installer == Installer.BlizzardDownload && snapshot.installation in NEEDS_INSTALLERS

    /** The key page's line about Download, with its overall percent while it runs; null: no such line. */
    fun line(session: SessionStatus): Line? {
        if (!downloadsFirst(session.snapshot)) return null
        val download = Onboarding.download(session)
        return when (download.state) {
            DownloadState.RUNNING, DownloadState.STOPPING ->
                percent(download, session.snapshot?.operation?.progress?.unit)?.let { Line(R.string.keys_installers_percent, it) }
                    ?: Line(R.string.keys_installers_running)
            DownloadState.DONE -> Line(R.string.keys_installers_done)
            // Why it failed, in the words of the failure code (Failures), as on the Download page.
            DownloadState.FAILED -> download.hint
            DownloadState.STOPPED -> Line(R.string.keys_installers_stopped)
            DownloadState.WAITING, DownloadState.READY -> Line(R.string.keys_installers_running)
        }
    }

    /**
     * Download stopped or failed: the line offers Try again, which starts it again. Agree and
     * install then starts it again too, and the keys join it.
     */
    fun retries(session: SessionStatus): Boolean =
        downloadsFirst(session.snapshot) &&
            Onboarding.download(session).state.let { it == DownloadState.FAILED || it == DownloadState.STOPPED }

    /**
     * The engine holds keys that joined a Download that failed or stopped ([Snapshot.authorized]):
     * a new Download needs the game environment, as Install Diablo II follows it by itself with
     * those keys. So Continue and the key page's Try again start it with the game environment and
     * open Install, not the key page.
     */
    fun keysWait(snapshot: Snapshot?): Boolean = snapshot?.authorized == true && downloadsFirst(snapshot)

    /**
     * Continue with [selected] runs Download again for the keys the engine holds ([keysWait]):
     * the same source ([sourceChanges] false), whose Diablo II is still to install.
     */
    fun continueJoins(selected: SourceChoice, snapshot: Snapshot?, sourceChanges: Boolean): Boolean =
        !sourceChanges && keysAtOnce(selected, snapshot) && keysWait(snapshot)

    /**
     * The key page starts Download by itself when nothing started it in this engine's run (the app
     * was closed since Continue); a stop or a failure waits for Try again.
     */
    fun startsOnKeyPage(session: SessionStatus): Boolean =
        downloadsFirst(session.snapshot) && Onboarding.download(session).state == DownloadState.READY

    /**
     * The line to show while Download runs, after [shown]: a check between two files of Project
     * Diablo 2 names no part ([line] says only "running"), so the percent shown stays until the
     * next one comes. Any other line replaces it.
     */
    fun hold(shown: Line?, next: Line?): Line? =
        if (next == Line(R.string.keys_installers_running) && shown?.id == R.string.keys_installers_percent) shown else next

    /**
     * The whole of Download in percent: the items before the one that downloads now, and that
     * one's own share ([Screen.Download.progress] is the item's own). Project Diablo 2's part
     * counts its files ([unit] "files"); a byte figure there is one file's own, not the part's.
     */
    private fun percent(download: Screen.Download, unit: String?): Int? {
        val items = download.items.ifEmpty { return null }
        val index = items.indexOfFirst { it.state == TaskState.RUNNING || it.state == TaskState.STOPPED }
        if (index < 0) return null
        val counts = items[index].item != DownloadItem.PROJECT_DIABLO || unit == "files"
        val within = download.progress?.takeIf { counts } ?: 0f
        return ((index + within) * 100 / items.size).toInt().coerceIn(0, 100)
    }

    /**
     * Download has ended, and no Install Diablo II follows it: it failed or stopped, or it ended
     * well with no keys waiting ([Snapshot.authorized]) or with a stop asked at its very end. The
     * engine shows its end in steps (no job, then the operation's result, then a stop's
     * [Snapshot.stopResult]), so this looks at the state, not at a change between two snapshots.
     * For a Download that keys joined, the game environment started for them then stops.
     */
    fun noInstallFollows(snapshot: Snapshot): Boolean =
        snapshot.activeJob == null && snapshot.operation?.job == Job.DOWNLOAD &&
            snapshot.operation.status.let {
                it == OperationStatus.FAILED || it == OperationStatus.INDETERMINATE || it == OperationStatus.CANCELLED ||
                    (it == OperationStatus.SUCCEEDED && !installFollows(snapshot))
            }

    /**
     * Download ended well and Install Diablo II follows it by itself: keys wait, and no stop shows.
     * A stop asked at its very end shows in a later snapshot ([Snapshot.stopResult]).
     */
    fun installFollows(snapshot: Snapshot): Boolean = snapshot.authorized && snapshot.stopResult == null
}
