package com.pd2launcher.app

import com.pd2launcher.app.control.EnvironmentPart
import com.pd2launcher.app.control.EnvironmentSetup
import com.pd2launcher.app.control.Game
import com.pd2launcher.app.control.Host
import com.pd2launcher.app.control.Installation
import com.pd2launcher.app.control.Installer
import com.pd2launcher.app.control.Job
import com.pd2launcher.app.control.Operation
import com.pd2launcher.app.control.OperationStatus
import com.pd2launcher.app.control.Phase
import com.pd2launcher.app.control.Progress
import com.pd2launcher.app.control.Step
import com.pd2launcher.app.control.Update
import com.pd2launcher.app.ui.GameLayer
import com.pd2launcher.app.ui.InstallPlan
import com.pd2launcher.app.ui.InstallProgress
import com.pd2launcher.app.ui.InstallRun

/**
 * Debug builds only: `am start -n com.pd2launcher.app.dev/com.pd2launcher.app.MainActivity --es preview <name>` shows
 * one native screen with injected facts, for screens a provisioned device does not reach on its
 * own. It changes nothing: the session runs on, B leaves the preview.
 */
object DebugPreview {
    const val NOT_SUPPORTED = "not-supported"
    const val OLD_KERNEL = "old-kernel"
    const val UNPACK = "unpack"
    /**
     * Install over the live session from Blizzard's downloads: before it starts; running at
     * Diablo II (Blizzard's downloader fetching its game files), at Lord of Destruction (the same
     * for its files) or at Project Diablo 2 (its files); stopped or failed at Diablo II.
     */
    const val INSTALL_READY = "install-ready"
    /** Running at the game environment's web components (part 2 of 4), which report only their time. */
    const val INSTALL_ENVIRONMENT = "install-environment"
    const val INSTALL = "install"
    const val INSTALL_LOD = "install-lod"
    const val INSTALL_PD2 = "install-pd2"
    /** Running at Project Diablo 2 while its installer runs: no bytes or files, only its time. */
    const val INSTALL_PD2_INSTALLER = "install-pd2-installer"
    const val INSTALL_STOPPED = "install-stopped"
    const val INSTALL_FAILED = "install-failed"
    /** Check from the real device check and session, also once installed. */
    const val CHECK = "check"
    /** Check with notifications off and battery optimisation on, each with its grant button. */
    const val CHECK_GRANTS = "check-grants"
    /** Check with notifications on and battery optimisation off: every row passed, folded. */
    const val CHECK_PASSED = "check-passed"
    /** Download over the live session, with a download injected (running, stopped or failed). */
    const val DOWNLOAD = "download"
    const val DOWNLOAD_STOPPED = "download-stopped"
    const val DOWNLOAD_FAILED = "download-failed"
    /** The key page's layout: empty fields that take no input, without FLAG_SECURE. */
    const val KEYS = "keys"
    /** The same with fields that take focus and the keyboard, still empty (never the debug keys). */
    const val KEYS_TYPING = "keys-typing"
    /**
     * The Diablo II source page over the live session (a pick there does copy and select); its
     * Continue sends nothing and opens the running [DOWNLOAD] preview instead.
     */
    const val SOURCE = "source"
    /** Settings over the live session; `--ei tab`, `--es picker`, `--ez mouse` pick where. */
    const val SETTINGS = "settings"
    /** Repair over the live session; its actions do nothing here. */
    const val REPAIR = "repair"
    /** Play over the live session, also before an install (its buttons do nothing here). */
    const val PLAY = "play"
    /**
     * Play with its one-time notice ([InstallerDownloads.notice]): after an install from Blizzard's
     * downloads, after an import with nothing downloaded, and after an import that downloaded
     * Project Diablo 2. Its answers save nothing.
     */
    const val NOTICE = "notice"
    const val NOTICE_IMPORT = "notice-import"
    const val NOTICE_IMPORT_DOWNLOADS = "notice-import-downloads"
    /** About over the live session. */
    const val ABOUT = "about"
    /** The Game view's overlay over an empty screen: the glyph, the menu rail, the Controls panel, the controls editor. */
    const val GAME = "game"
    const val GAME_MENU = "game-menu"
    /** The menu rail as over the official PD2 launcher. */
    const val GAME_MENU_LAUNCHER = "game-menu-launcher"
    const val GAME_CONTROLS = "game-controls"
    const val GAME_EDIT = "game-edit"

    /** Play with Project Diablo 2's Available update: offered, running (a file at 42 %), failed. Its buttons do nothing here. */
    const val PLAY_UPDATE = "play-update"
    const val PLAY_UPDATING = "play-updating"
    const val PLAY_UPDATE_FAILED = "play-update-failed"

    /** [session] with the update preview [name] in its snapshot (an installed game); null for other previews. */
    fun playUpdate(name: String?, session: SessionStatus): SessionStatus? {
        val status = when (name) {
            PLAY_UPDATE -> null
            PLAY_UPDATING -> OperationStatus.RUNNING
            PLAY_UPDATE_FAILED -> OperationStatus.FAILED
            else -> return null
        }
        val snapshot = session.snapshot ?: return session
        val running = status == OperationStatus.RUNNING
        val operation = status?.let {
            Operation(
                phase = if (running) Phase.CLIENT_DOWNLOAD else Phase.VERIFICATION,
                status = it,
                title = "",
                detail = "",
                progress = if (running) {
                    Progress(42, 37_800_000, 90_100_000, "bytes", "37.8 MB of 90.1 MB · 6.2 MB/s · less than a minute left for this file")
                } else {
                    null
                },
                mutating = running,
                failure = if (it == OperationStatus.FAILED) "verified-update-failed" else null,
                job = Job.UPDATE,
                environment = null,
            )
        }
        return session.copy(
            refusal = null,
            snapshot = snapshot.copy(
                host = Host.SUPPORTED,
                installation = Installation.READY,
                update = Update.AVAILABLE,
                game = Game.IDLE,
                operation = operation,
                activeJob = if (running) Job.UPDATE else null,
            ),
        )
    }

    /** The notice preview [name] shows; null for other previews. */
    fun notice(name: String?): InstallerDownloads.Notice? = when (name) {
        NOTICE -> InstallerDownloads.notice(2_400_000_000L, imported = false, folder = null)
        NOTICE_IMPORT -> InstallerDownloads.notice(0, imported = true, folder = "Download/Diablo II")
        NOTICE_IMPORT_DOWNLOADS -> InstallerDownloads.notice(480_000_000L, imported = true, folder = "Download/Diablo II")
        else -> null
    }

    /** The Game view's layer [name] previews; null for other previews. */
    fun gameLayer(name: String?): GameLayer? = when (name) {
        GAME -> GameLayer.NONE
        GAME_MENU, GAME_MENU_LAUNCHER -> GameLayer.MENU
        GAME_CONTROLS -> GameLayer.CONTROLS
        GAME_EDIT -> GameLayer.EDIT
        else -> null
    }

    /** The Game view's preview [name] is over the official PD2 launcher. */
    fun gameLauncher(name: String?): Boolean = name == GAME_MENU_LAUNCHER

    /**
     * [session] with a download from Blizzard in its snapshot that runs or ended with [status]:
     * the Diablo II installer done, the Lord of Destruction one at 37 %, Project Diablo 2 next.
     */
    fun download(session: SessionStatus, status: OperationStatus = OperationStatus.RUNNING): SessionStatus {
        val snapshot = session.snapshot ?: return session
        val running = status == OperationStatus.RUNNING
        val operation = Operation(
            phase = Phase.EXPANSION_DOWNLOAD,
            status = status,
            title = "Downloading Lord of Destruction from Blizzard",
            detail = "",
            progress = Progress(
                percent = 37, completed = 196L shl 20, total = 530L shl 20, unit = "bytes",
                summary = "196 MB of 530 MB · 4.2 MB/s · about 1 minute left",
            ),
            mutating = running,
            failure = if (status == OperationStatus.FAILED) "connection-reset" else null,
            job = Job.DOWNLOAD,
            environment = null,
        )
        return session.copy(
            snapshot = snapshot.copy(
                operation = operation,
                activeJob = if (running) Job.DOWNLOAD else null,
                downloads = snapshot.downloads.copy(complete = false),
                installation = Installation.NEEDS_BASE_GAME,
                installer = Installer.BlizzardDownload,
                stopResult = null,
            ),
        )
    }

    /** Where an install preview stands: the engine's phase and job, and the progress it reports. */
    private class At(val phase: Phase, val job: Job, val progress: Progress?, val environment: EnvironmentSetup? = null)

    private fun at(name: String): At? = when (name) {
        INSTALL_ENVIRONMENT -> At(
            Phase.PREFIX_INITIALIZATION, Job.INSTALL_BASE_GAME, null,
            EnvironmentSetup(EnvironmentPart.WEB_COMPONENTS, EnvironmentPart.entries),
        )
        INSTALL, INSTALL_STOPPED, INSTALL_FAILED -> At(
            Phase.BASE_GAME_DOWNLOAD, Job.INSTALL_BASE_GAME,
            Progress(71, 1_181_116_006, 1_554_530_117, "bytes", "1.1 GB of 1.4 GB · 3.2 MB/s · about 2 minutes left"),
        )
        INSTALL_LOD -> At(
            Phase.EXPANSION_DOWNLOAD, Job.INSTALL_BASE_GAME,
            Progress(40, 214L shl 20, 530L shl 20, "bytes", "214 MB of 530 MB · 3.0 MB/s · about 2 minutes left"),
        )
        INSTALL_PD2 -> At(Phase.CLIENT_DOWNLOAD, Job.INSTALL_PROJECT_DIABLO, Progress(40, 120, 300, "files", "120 of 300 files"))
        INSTALL_PD2_INSTALLER -> At(Phase.CLIENT_DOWNLOAD, Job.INSTALL_PROJECT_DIABLO, Progress(75, 3, 4, "stages", null))
        else -> null
    }

    /** Install's preview [name] over [session]; null when [name] is not one. */
    fun install(name: String, session: SessionStatus): SessionStatus? {
        val status = when (name) {
            INSTALL_READY -> null
            INSTALL_ENVIRONMENT, INSTALL, INSTALL_LOD, INSTALL_PD2, INSTALL_PD2_INSTALLER -> OperationStatus.RUNNING
            INSTALL_STOPPED -> OperationStatus.CANCELLED
            INSTALL_FAILED -> OperationStatus.FAILED
            else -> return null
        }
        val snapshot = session.snapshot ?: return session
        val running = status == OperationStatus.RUNNING
        val at = at(name)
        val pd2 = at?.job == Job.INSTALL_PROJECT_DIABLO
        val operation = status?.let {
            Operation(
                phase = at!!.phase,
                status = it,
                title = "",
                detail = "",
                progress = at.progress,
                mutating = running,
                failure = if (it == OperationStatus.FAILED) "installer-timeout" else null,
                job = at.job,
                environment = at.environment,
            )
        }
        return session.copy(
            snapshot = snapshot.copy(
                operation = operation,
                activeJob = if (running) at?.job else null,
                installation = when {
                    pd2 -> Installation.NEEDS_CLIENT
                    name == INSTALL_LOD -> Installation.NEEDS_EXPANSION
                    else -> Installation.NEEDS_BASE_GAME
                },
                installer = Installer.BlizzardDownload,
                step = if (pd2) Step.INSTALL_PD2 else Step.INSTALL_D2,
                // Every preview but the first task's has the game environment set up.
                environmentReady = name != INSTALL_READY && name != INSTALL_ENVIRONMENT,
                stopResult = null,
                result = null,
            ),
            gameEnvironmentFailure = null,
        )
    }

    /** The run Install's preview [name] shows: a minute into its part, or where it ended. */
    fun installRun(name: String, session: SessionStatus, now: Long): InstallRun? {
        val at = at(name) ?: return null
        val segments = InstallPlan.segments(SourceChoice.BLIZZARD)
        val segment = InstallPlan.segmentOf(segments, at.phase, at.job) ?: return null
        val ended = name == INSTALL_STOPPED || name == INSTALL_FAILED
        val run = InstallRun(
            SourceChoice.BLIZZARD, startedAt = now - 360_000, failureAtStart = null,
            segment = segment, segmentSince = now - 60_000, sawJob = true, ended = ended,
            part = at.environment?.part, partSince = now - 12_000,
        )
        return if (run.ended) run else InstallProgress.track(run, session, SourceChoice.BLIZZARD, now)
    }

    /** The screen [name] stands for, from the [real] device check and session; null if unknown. */
    fun screen(name: String, real: DeviceCheckState, session: SessionStatus, notificationsOff: Boolean): Screen? {
        val done = real as? DeviceCheckState.Done ?: return null
        val facts = done.facts
        fun device(injected: DeviceFacts) = DeviceCheckState.Done(injected, DeviceCheck.evaluate(injected) { null }, done.androidRelease)
        return when (name) {
            // An Adreno 505 on Android 10 with a 4.14 kernel: refused, with the kernel warning.
            NOT_SUPPORTED -> Onboarding.screen(
                device(facts.copy(gpu = KgslGpu(0x05000500, 5), sdk = 29, kernelRelease = "4.14.190-perf")),
                session, notificationsOff,
            )
            OLD_KERNEL -> Onboarding.screen(device(facts.copy(kernelRelease = "5.4.210-qgki")), session, notificationsOff)
            CHECK -> Onboarding.screen(done, session, notificationsOff)
            CHECK_GRANTS -> Onboarding.screen(done, session, notificationsOff = true, batteryRestricted = true)
            CHECK_PASSED -> Onboarding.screen(done, session, notificationsOff = false, batteryRestricted = false)
            DOWNLOAD -> Onboarding.download(download(session))
            DOWNLOAD_STOPPED -> Onboarding.download(download(session, OperationStatus.CANCELLED))
            DOWNLOAD_FAILED -> Onboarding.download(download(session, OperationStatus.FAILED))
            UNPACK -> Onboarding.screen(
                done,
                SessionStatus(userland = SessionStatus.Userland.Unpacking(253L shl 20, 602L shl 20)),
                notificationsOff,
            )
            else -> null
        }
    }
}
