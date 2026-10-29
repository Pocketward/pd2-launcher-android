package com.pd2launcher.app

import androidx.annotation.StringRes
import com.pd2launcher.app.control.Host
import com.pd2launcher.app.control.Installation
import com.pd2launcher.app.control.Installer
import com.pd2launcher.app.control.Job
import com.pd2launcher.app.control.OperationStatus
import com.pd2launcher.app.control.Phase
import com.pd2launcher.app.control.Snapshot
import com.pd2launcher.app.control.Step
import com.pd2launcher.app.ui.NetworkState
import com.pd2launcher.app.ui.Play
import com.pd2launcher.app.ui.TaskState
import com.pd2launcher.app.ui.networkText
import com.pd2launcher.app.control.Finding as HostFinding

/** What the first-start device check has found so far. */
sealed interface DeviceCheckState {
    data object Running : DeviceCheckState

    /** [androidRelease] is Android's own version name ("13"); the facts carry only the SDK. */
    data class Done(val facts: DeviceFacts, val result: DeviceCheckResult, val androidRelease: String) : DeviceCheckState

    /** The check itself failed to run. */
    data class Failed(val reason: String) : DeviceCheckState
}

/** One line of text as a string resource and its arguments. */
data class Line(@param:StringRes val id: Int, val args: List<Any>) {
    constructor(@StringRes id: Int, vararg args: Any) : this(id, args.toList())
}

/** How a check row stands; the screen shows each with a glyph and words, not color alone. */
enum class Tone { PENDING, WORKING, OK, WARNING, ERROR }

/** One row of step 1: what is checked, how it stands, and what to say about it. */
data class CheckRow(
    @param:StringRes val label: Int,
    val tone: Tone,
    val lines: List<Line>,
    /** 0..1 while [Tone.WORKING] has a known amount; null otherwise. */
    val progress: Float? = null,
    /** What the row's button grants in place; null: nothing to grant. */
    val grant: Grant? = null,
)

/**
 * What a Check row can ask Android for: its own prompt where there is one, else its settings page
 * (for [NETWORK], the app's own page, where Wi-Fi and mobile data are allowed).
 */
enum class Grant { NOTIFICATIONS, BATTERY, NETWORK }

/** The first-run pages, in the order the player goes through them. */
enum class FlowPage { CHECK, SOURCE, DOWNLOAD, INSTALL, KEYS }

/**
 * Where the player is in the first run and how they got there: going on pushes a page, Back pops
 * it. The choices themselves live in the activity and the engine, so going back loses none.
 */
data class FlowNav(val stack: List<FlowPage> = listOf(FlowPage.CHECK)) {
    companion object {
        /**
         * Where the first run opens: an install started and not finished ([SetupRecord.STARTED])
         * on Install, with Back to the source and Check as after a finished download; else Check.
         */
        fun opening(record: SetupRecord): FlowNav =
            if (record == SetupRecord.STARTED) FlowNav(listOf(FlowPage.CHECK, FlowPage.SOURCE, FlowPage.INSTALL)) else FlowNav()

        /**
         * Where the activity opens on each onCreate: while the first run goes on, the pages [saved]
         * before Android made the activity again ([restored]); else [opening]. An installed game
         * opens on Play ([Onboarding.installed]) and keeps no saved pages. [record] is the one
         * [SetupRecordStore.load] read on this start: a new process has none before that load.
         */
        fun start(saved: List<String>?, record: SetupRecord): FlowNav =
            restored(saved)?.takeIf { record != SetupRecord.INSTALLED } ?: opening(record)

        /**
         * The pages [saved] when Android made the activity again (for example, the process ended
         * under the folder picker, or "Don't keep activities" is on); null for none, or for a
         * stack that is not one: no Check at its root, or a page this version does not know.
         */
        fun restored(names: List<String>?): FlowNav? {
            val pages = names?.map { name -> FlowPage.entries.firstOrNull { it.name == name } ?: return null }
            return pages?.takeIf { it.firstOrNull() == FlowPage.CHECK }?.let(::FlowNav)
        }
    }

    val page: FlowPage get() = stack.last()

    /** The pages as names, for the activity's saved state ([restored]). */
    fun saved(): ArrayList<String> = stack.mapTo(ArrayList()) { it.name }

    fun open(next: FlowPage): FlowNav = if (next == page) this else copy(stack = stack + next)

    /** The previous page; Check stays (Back there leaves the app). */
    fun back(): FlowNav = if (stack.size > 1) copy(stack = stack.dropLast(1)) else this

    /**
     * On from the source: Download, its progress at once (Continue started it; no page asks
     * first), or Install at once when nothing is left to download.
     */
    fun afterSource(installReady: Boolean): FlowNav = open(if (installReady) FlowPage.INSTALL else FlowPage.DOWNLOAD)

    /**
     * On from the source with a download of Diablo II: the key page at once, over Install, while
     * Download runs ([KeysWhileDownloading]). Agree and install goes back to Install, Back too.
     */
    fun keysWhileDownloading(): FlowNav = open(FlowPage.INSTALL).open(FlowPage.KEYS)

    /** The download finished while Download was shown: Install follows. */
    fun downloadDone(): FlowNav = if (page == FlowPage.DOWNLOAD) open(FlowPage.INSTALL) else this

    /** The install started from the key page: its progress shows at once, on Install. */
    fun installStarted(): FlowNav = if (page == FlowPage.KEYS) back() else this

    /**
     * Start over (Repair, during setup) reset an interrupted stage: straight to the key page, or
     * Install's start button when the source needs no keys (an installed Diablo II is imported).
     */
    fun afterStartOver(needsKeys: Boolean): FlowNav = when {
        needsKeys -> open(FlowPage.KEYS)
        page == FlowPage.KEYS -> back()
        else -> open(FlowPage.INSTALL)
    }
}

/** Step 1's one main action (the bottom-right corner, `ui/Shell.kt`) and why it waits while disabled. */
data class MainAction(val kind: Kind, val enabled: Boolean, val hint: Line) {
    /**
     * [REPAIR]: the installation itself is broken; Repair's Start over is the way on. [TRY_AGAIN]:
     * the engine or the game support files failed ([Onboarding.failed]); it starts them again.
     */
    enum class Kind { CONTINUE, CHECK_AGAIN, REPAIR, TRY_AGAIN }
}

/** The native screen the app shows, derived from the device check and the session. */
sealed interface Screen {
    /** The first-start check runs (before anything is unpacked). */
    data object CheckingDevice : Screen

    /** The device check refused this device; [problems] say why, [warnings] what else it found. */
    data class NotSupported(val problems: List<Line>, val warnings: List<Line>) : Screen

    /**
     * Step 1, Check: the device, notifications, the App userland ("Game support files") and the
     * Launcher's host check ("Can run the game"), in plain words.
     */
    data class Check(
        val device: CheckRow,
        val notifications: CheckRow,
        /** Whether Android's battery optimisation may pause the work in the background; never blocks. */
        val battery: CheckRow,
        val userland: CheckRow,
        val host: CheckRow,
        val action: MainAction,
        /** Whether Blizzard's and Project Diablo 2's servers can be reached ([NetworkCheck]); never blocks. */
        val network: CheckRow? = null,
    ) : Screen {
        val rows: List<CheckRow> get() = listOfNotNull(device, notifications, battery, network, userland, host)

        /** The rows that passed, folded into one line ("3 checks passed") the player can open. */
        val passed: List<CheckRow> get() = rows.filter { it.tone == Tone.OK }

        /** The rows that stay open: what needs the player (an ask, a warning, a problem) and what still runs. */
        val open: List<CheckRow> get() = rows.filter { it.tone != Tone.OK }

        /** The asks left: rows the player may grant in place (notifications, battery); none blocks. */
        val optionalAsks: Int get() = rows.count { it.grant != null && it.grant != Grant.NETWORK && it.tone == Tone.WARNING }

        /** The servers cannot be reached: not optional, as the downloads need them, though Check goes on. */
        val connectionProblem: Boolean get() = network?.tone == Tone.WARNING

        /** How Check stands, for its title and intro: a problem first, then work still running, then a warning. */
        val tone: Tone get() {
            val tones = rows.map { it.tone }
            return when {
                Tone.ERROR in tones -> Tone.ERROR
                Tone.PENDING in tones || Tone.WORKING in tones -> Tone.PENDING
                Tone.WARNING in tones -> Tone.WARNING
                else -> Tone.OK
            }
        }

        /** A warning the player cannot grant away, apart from the connection's: the device itself is untested. */
        private val untested: Boolean get() = rows.any { it !== network && it.tone == Tone.WARNING && it.grant == null }

        /**
         * The page's title: "Ready" only when nothing but optional asks remain ("Ready — 2 optional
         * settings"), never beside a warning about the device itself.
         */
        val title: Line get() = when (tone) {
            Tone.ERROR -> Line(R.string.check_title_problem)
            Tone.PENDING, Tone.WORKING -> Line(R.string.check_title)
            Tone.OK -> Line(R.string.check_title_ready)
            Tone.WARNING -> when {
                untested -> Line(R.string.check_title_untested)
                connectionProblem -> Line(R.string.check_title_connection)
                optionalAsks == 1 -> Line(R.string.check_title_optional_one)
                else -> Line(R.string.check_title_optional, optionalAsks)
            }
        }

        @get:StringRes
        val intro: Int get() = when (tone) {
            Tone.ERROR -> R.string.check_intro_problem
            Tone.PENDING, Tone.WORKING -> R.string.check_intro
            Tone.OK -> R.string.check_intro_ready
            Tone.WARNING -> when {
                untested -> R.string.check_intro_untested
                connectionProblem -> R.string.check_intro_connection
                else -> R.string.check_intro_optional
            }
        }
    }

    /** Step 2, Diablo II source (ui/SourceScreen.kt shows it from [DiabloSource]). */
    data object Source : Screen

    /** Step 4, Install: the four tasks and their progress (ui/InstallScreen.kt, [com.pd2launcher.app.ui.InstallProgress]). */
    data object Install : Screen

    /** Install's CD key page, before Install Diablo II from Blizzard's installers. */
    data object Keys : Screen

    /**
     * Step 3, Download: the installers the install needs, each
     * waiting, downloading, done, stopped or failed, and how far the one downloading now is.
     */
    data class Download(
        val state: DownloadState,
        val items: List<DownloadRow>,
        /** The item downloading now: 0..1 when its amount is known. */
        val progress: Float?,
        /** Its bytes done of the total, and the engine's speed and time left, when known. */
        val amount: Line?,
        val summary: String?,
        val hint: Line,
    ) : Screen {
        /** The item downloading (or stopped, or failed) now. */
        val current: DownloadRow? get() = items.firstOrNull { it.state != TaskState.WAITING && it.state != TaskState.DONE }

        /**
         * The one main action: Stop safely, on to Install, start again, or nothing yet. The source's
         * Continue starts the download, so [DownloadState.READY] shows only when that start did not
         * come through (refused, or no engine then); its Download starts it here.
         */
        val action: DownloadAction get() = when (state) {
            DownloadState.READY -> DownloadAction.DOWNLOAD
            DownloadState.STOPPED, DownloadState.FAILED -> DownloadAction.DOWNLOAD_AGAIN
            DownloadState.RUNNING -> DownloadAction.STOP_SAFELY
            DownloadState.DONE -> DownloadAction.CONTINUE
            DownloadState.WAITING, DownloadState.STOPPING -> DownloadAction.NONE
        }

        val tone: Tone get() = when (state) {
            DownloadState.DONE -> Tone.OK
            DownloadState.FAILED -> Tone.ERROR
            DownloadState.STOPPED -> Tone.WARNING
            DownloadState.RUNNING, DownloadState.STOPPING -> Tone.WORKING
            DownloadState.READY, DownloadState.WAITING -> Tone.PENDING
        }
    }
}

/**
 * What the app shows while its record says the game is installed ([SetupRecord.INSTALLED]):
 * Play or Repair, never Check or the install flow.
 */
sealed interface Installed {
    /**
     * Play. [waiting]: why it cannot start yet, said where "Ready to play" goes (the device check,
     * an unpack after an app update, the engine's first audit); [problem]: what stopped it. Play
     * starts only without [waiting].
     */
    data class Play(val waiting: Line? = null, val problem: Line? = null) : Installed

    /**
     * The audit found the installation broken (files missing, needs_runtime included: Start over
     * is the way on; not needs_client alone, which Play's Check files brings in line, except before
     * the first Ready after an update in place), its folder gone or the device unable to run it; or
     * Start over runs.
     */
    data object Repair : Installed

    /** The device check refused this device (rare once the game is installed). */
    data class Refused(val screen: Screen.NotSupported) : Installed
}

enum class DownloadState { WAITING, READY, RUNNING, STOPPING, STOPPED, FAILED, DONE }

/**
 * What Download fetches, in the engine's order: Blizzard's two installers (the small programs
 * that download the game in Install) and Project Diablo 2 (its
 * installer and files). The game environment's pieces ship in the app, so none is listed.
 */
enum class DownloadItem(@param:StringRes val label: Int, val phase: Phase) {
    D2_INSTALLER(R.string.download_item_d2, Phase.BASE_GAME_DOWNLOAD),
    LOD_INSTALLER(R.string.download_item_lod, Phase.EXPANSION_DOWNLOAD),
    PROJECT_DIABLO(R.string.download_item_pd2, Phase.CLIENT_DOWNLOAD),
}

data class DownloadRow(val item: DownloadItem, val state: TaskState)

enum class DownloadAction { NONE, DOWNLOAD, DOWNLOAD_AGAIN, STOP_SAFELY, CONTINUE }

/**
 * Maps what the app knows onto its native screens. Pure, so every state is unit tested; the
 * Compose screens only render the result. Step 1 shows the engine's snapshot as it is: the host
 * findings come from the snapshot, never from the app's own guesses.
 */
object Onboarding {
    /**
     * [page]: where the player went on to from Check ([FlowNav]). [batteryRestricted]: Android's
     * battery optimisation applies to the app (a warning on Check, never a blocker). [network]: the
     * last network check (a warning on Check, never a blocker); null shows none.
     */
    fun screen(
        device: DeviceCheckState,
        session: SessionStatus,
        notificationsOff: Boolean,
        page: FlowPage = FlowPage.CHECK,
        batteryRestricted: Boolean = false,
        network: NetworkState? = null,
    ): Screen {
        refusal(device)?.let { return it }
        // An install started and not finished opens on Install ([FlowNav.opening]), which waits meanwhile.
        if (device !is DeviceCheckState.Done) return if (page == FlowPage.INSTALL) Screen.Install else Screen.CheckingDevice
        // Once on, a page stays while the engine audits or restarts; a host found unsupported
        // goes back to Check.
        val check = check(device, session, notificationsOff, batteryRestricted, network)
        if (check.host.tone == Tone.ERROR) return check
        return when (page) {
            FlowPage.CHECK -> check
            FlowPage.SOURCE -> Screen.Source
            FlowPage.DOWNLOAD -> download(session)
            // Install waits for what Download fetches (after a change of source, say). The key page
            // goes on while it runs, also after it stopped or failed (its line says so). While an
            // install job runs, Install shows it, not the key page.
            FlowPage.INSTALL, FlowPage.KEYS -> when {
                session.snapshot?.activeJob in INSTALL_JOBS -> Screen.Install
                page == FlowPage.KEYS && KeysWhileDownloading.downloadsFirst(session.snapshot) -> Screen.Keys
                beforeInstall(session) -> download(session)
                page == FlowPage.INSTALL -> Screen.Install
                else -> Screen.Keys
            }
        }
    }

    /** The device check refused this device, or could not run; null when it runs or passed. */
    private fun refusal(device: DeviceCheckState): Screen.NotSupported? = when (device) {
        is DeviceCheckState.Failed -> Screen.NotSupported(listOf(Line(R.string.device_check_failed, device.reason)), emptyList())
        is DeviceCheckState.Done -> if (device.result.passed) {
            null
        } else {
            Screen.NotSupported(
                device.result.findings.filter { it.kind.blocks }.map { deviceFinding(it, device.facts) },
                device.result.findings.filterNot { it.kind.blocks }.map { deviceFinding(it, device.facts) },
            )
        }
        DeviceCheckState.Running -> null
    }

    /**
     * Where the app stands once its record says the game is installed ([SetupRecord.INSTALLED]):
     * Play from the first frame, waiting in place ("Getting ready…", "Updating game files… 40%")
     * while the device check, an unpack after an app update and the engine's first audit run, and
     * startable only once that audit says Ready; with Check files in Play's place when only Project
     * Diablo 2's files differ from the Launcher's record ([Play.clientDiffers]); Repair when the
     * audit finds the installation broken otherwise (or the device unable to run it), and while
     * Start over resets it. Never Check or the install flow: the engine's placeholder before its
     * audit is not evidence ([audited]). [updatedInPlace]: an update in place moved the game and
     * the installation was not Ready since ([LegacyPrefix.UPDATED], the managed game folder there):
     * any audit short of Ready is Repair's, with "Use your game" first ([UpgradeImport.offersImport]).
     */
    fun installed(device: DeviceCheckState, session: SessionStatus, updatedInPlace: Boolean = false): Installed {
        if (device == DeviceCheckState.Running) return Installed.Play(waiting = Line(R.string.play_getting_ready))
        refusal(device)?.let { return Installed.Refused(it) }
        val userland = session.userland
        val snapshot = session.snapshot
        return when {
            userland is SessionStatus.Userland.Failed ->
                Installed.Play(Line(R.string.play_not_ready), Line(R.string.play_files_failed, userland.reason))
            session.engine == SessionStatus.Engine.FAILED -> Installed.Play(Line(R.string.play_not_ready), Line(R.string.host_engine_failed))
            userland is SessionStatus.Userland.Unpacking -> Installed.Play(
                waiting = if (userland.total > 0) {
                    Line(R.string.play_updating_files_percent, (userland.done * 100 / userland.total).toInt())
                } else {
                    Line(R.string.play_updating_files)
                },
            )
            session.engine != SessionStatus.Engine.CONNECTED || snapshot == null || !audited(snapshot) ->
                Installed.Play(waiting = Line(R.string.play_getting_ready))
            // Start over runs; its job ends a moment before the audit after it lands.
            snapshot.activeJob == Job.RESET || snapshot.operation?.let { it.phase == Phase.RESET && it.status in RUNNING } == true ->
                Installed.Repair
            snapshot.host == Host.SUPPORTED && snapshot.installation == Installation.READY -> Installed.Play()
            // Only Project Diablo 2's files differ from the Launcher's record (the official PD2
            // launcher's update, say): Play, whose Check files brings them in line ([Play.clientDiffers]);
            // before the first Ready after an update in place, Repair ([updatedInPlace]).
            !updatedInPlace && Play.clientDiffers(snapshot) -> Installed.Play()
            // Anything else is broken for an installed game, needs_runtime too: nothing else is set
            // up in place on Android, Repair's Start over is the way on (an APK that adds a part to
            // the game environment sends installed players there).
            else -> Installed.Repair
        }
    }

    /**
     * Settings' back label: to the Game view it opened over ([gameView]), to Repair when Repair
     * shows under it ([repairShown]: the preview, opened from Play or held on screen), else to Play.
     */
    @StringRes
    fun settingsBack(gameView: Boolean, repairShown: Boolean): Int = when {
        gameView -> R.string.back_to_game
        repairShown -> R.string.back_to_repair
        else -> R.string.back_to_play
    }

    /**
     * Repair shows Settings and Share logs and Save logs for an installed game ([record]): held on
     * screen (its Back leaves the app), opened from Play, and through Check again alike.
     */
    fun repairLinks(record: SetupRecord): Boolean = record == SetupRecord.INSTALLED

    /** Step 3, Download, from the engine's snapshot. */
    fun download(session: SessionStatus): Screen.Download {
        val snapshot = session.snapshot
        // The download's own operation, or an install that failed for want of a download (Blizzard's
        // downloader, which Install Diablo II runs): Install falls back here and says why.
        val operation = snapshot?.operation?.takeIf {
            it.job == Job.DOWNLOAD || it.job in INSTALL_JOBS && it.status in ENDED_UNFINISHED
        }
        val state = when {
            snapshot == null || session.engine != SessionStatus.Engine.CONNECTED -> DownloadState.WAITING
            operation?.status == OperationStatus.SAFE_STOPPING -> DownloadState.STOPPING
            operation != null && operation.status in RUNNING -> DownloadState.RUNNING
            snapshot.downloads.complete -> DownloadState.DONE
            operation?.status == OperationStatus.CANCELLED || snapshot.stopResult != null -> DownloadState.STOPPED
            operation?.status == OperationStatus.FAILED || operation?.status == OperationStatus.INDETERMINATE -> DownloadState.FAILED
            else -> DownloadState.READY
        }
        val working = state == DownloadState.RUNNING || state == DownloadState.STOPPING
        val progress = operation?.progress.takeIf { working }
        val amount = progress?.let { p ->
            val completed = p.completed
            val total = p.total
            if (p.unit == "bytes" && completed != null) {
                if (total != null) Line(R.string.download_amount, completed shr 20, total shr 20) else Line(R.string.download_amount_open, completed shr 20)
            } else {
                null
            }
        }
        val hint = when (state) {
            DownloadState.WAITING -> Line(R.string.download_hint_waiting)
            DownloadState.READY -> Line(R.string.download_hint_ready)
            DownloadState.RUNNING -> Line(R.string.download_hint_running)
            DownloadState.STOPPING -> Line(R.string.download_hint_stopping)
            DownloadState.STOPPED -> Line(R.string.download_hint_stopped)
            DownloadState.FAILED -> Failures.line(operation?.failure, operation?.job)
            DownloadState.DONE -> Line(R.string.download_hint_done, snapshot?.downloads?.totalBytes?.shr(20) ?: 0L)
        }
        // Where the download is: the item of the operation's phase (none yet while it checks
        // what is already there); the ones before it are done.
        val at = operation?.phase?.let { phase -> DownloadItem.entries.firstOrNull { it.phase == phase } }
        val here = when (state) {
            DownloadState.RUNNING, DownloadState.STOPPING -> TaskState.RUNNING
            DownloadState.STOPPED -> TaskState.STOPPED
            DownloadState.FAILED -> TaskState.FAILED
            DownloadState.DONE -> TaskState.DONE
            DownloadState.WAITING, DownloadState.READY -> null
        }
        val items = snapshot?.let(::downloadItems).orEmpty().map { item ->
            DownloadRow(
                item,
                when {
                    here == TaskState.DONE -> TaskState.DONE
                    here == null || at == null || item > at -> TaskState.WAITING
                    item < at -> TaskState.DONE
                    else -> here
                },
            )
        }
        return Screen.Download(
            state = state,
            items = items,
            progress = progress?.percent?.let { it.coerceIn(0, 100) / 100f },
            amount = amount,
            summary = progress?.summary,
            hint = hint,
        )
    }

    /**
     * What Download fetches for the installation: Blizzard's installers for what Diablo II still
     * lacks (none for an imported Diablo II), then Project Diablo 2.
     */
    fun downloadItems(snapshot: Snapshot): List<DownloadItem> {
        val blizzard = when {
            snapshot.installer is Installer.Existing -> emptyList()
            snapshot.installation == Installation.NEEDS_BASE_GAME -> listOf(DownloadItem.D2_INSTALLER, DownloadItem.LOD_INSTALLER)
            snapshot.installation == Installation.NEEDS_EXPANSION -> listOf(DownloadItem.LOD_INSTALLER)
            else -> emptyList()
        }
        return blizzard + DownloadItem.PROJECT_DIABLO
    }

    /**
     * Continue on the source starts (or resumes) the download at once: not while one runs or
     * stops, not before the engine is connected, and not when it is done, unless [sourceChanges]
     * (what was downloaded served the previous source).
     */
    fun startsDownload(session: SessionStatus, sourceChanges: Boolean): Boolean = when (download(session).state) {
        DownloadState.READY, DownloadState.STOPPED, DownloadState.FAILED -> true
        DownloadState.DONE -> sourceChanges
        DownloadState.WAITING, DownloadState.RUNNING, DownloadState.STOPPING -> false
    }

    /**
     * The installation stands before Install (Download is not done) and no job runs, by the
     * engine's audit: its placeholder before (step device) says nothing yet.
     */
    fun beforeInstall(session: SessionStatus): Boolean = session.snapshot?.let {
        audited(it) && it.step in setOf(Step.DEVICE, Step.SETUP, Step.DOWNLOAD) && it.activeJob == null
    } == true

    private val RUNNING = setOf(
        OperationStatus.QUEUED, OperationStatus.PLANNING, OperationStatus.RUNNING,
        OperationStatus.VERIFYING, OperationStatus.COMMITTING,
    )

    private val INSTALL_JOBS = setOf(Job.INSTALL_BASE_GAME, Job.INSTALL_PROJECT_DIABLO)

    private val ENDED_UNFINISHED = setOf(OperationStatus.FAILED, OperationStatus.INDETERMINATE)

    /**
     * Whether the session service may start: the device passed. On Android 13 its notification
     * channel brings Android's own notification prompt (at targetSdk 28 nothing else can ask), over
     * Check, whose notifications row then follows the answer and never blocks.
     */
    fun sessionAllowed(device: DeviceCheckState): Boolean =
        device is DeviceCheckState.Done && device.result.passed

    private fun check(
        device: DeviceCheckState.Done,
        session: SessionStatus,
        notificationsOff: Boolean,
        batteryRestricted: Boolean,
        network: NetworkState?,
    ): Screen.Check =
        Screen.Check(
            device = deviceRow(device),
            notifications = if (notificationsOff) {
                CheckRow(R.string.row_notifications, Tone.WARNING, listOf(Line(R.string.notifications_off_warning)), grant = Grant.NOTIFICATIONS)
            } else {
                CheckRow(R.string.row_notifications, Tone.OK, listOf(Line(R.string.notifications_on)))
            },
            battery = if (batteryRestricted) {
                CheckRow(R.string.row_battery, Tone.WARNING, listOf(Line(R.string.battery_restricted_warning)), grant = Grant.BATTERY)
            } else {
                CheckRow(R.string.row_battery, Tone.OK, listOf(Line(R.string.battery_unrestricted)))
            },
            userland = userlandRow(session.userland),
            host = hostRow(session),
            action = action(session),
            network = network?.let(::networkRow),
        )

    /**
     * Whether the servers can be reached: checking (the first time only: a check run again shows
     * the last result meanwhile, so the page does not flicker), then all right or why not, with
     * Open settings when Android blocks the app's network. A warning at most: Import needs no
     * Blizzard.
     */
    fun networkRow(network: NetworkState): CheckRow {
        val report = network.report ?: return CheckRow(R.string.row_network, Tone.WORKING, listOf(Line(R.string.network_checking)))
        val result = report.result
        return CheckRow(
            R.string.row_network,
            if (result == NetworkCheck.Result.OK) Tone.OK else Tone.WARNING,
            listOf(Line(networkText(result))),
            grant = Grant.NETWORK.takeIf { result == NetworkCheck.Result.BLOCKED },
        )
    }

    /** "Adreno 610 graphics · Android 14", then a warning for each finding that lets the app go on. */
    fun deviceRow(device: DeviceCheckState.Done): CheckRow {
        val facts = device.facts
        val gpu = facts.gpu?.name
        val summary = if (gpu.isNullOrBlank()) {
            Line(R.string.device_summary_android, device.androidRelease)
        } else {
            Line(R.string.device_summary, gpu, device.androidRelease)
        }
        val warnings = device.result.findings.filterNot { it.kind.blocks }.map { deviceFinding(it, facts) }
        return CheckRow(R.string.row_device, if (warnings.isEmpty()) Tone.OK else Tone.WARNING, listOf(summary) + warnings)
    }

    fun userlandRow(userland: SessionStatus.Userland): CheckRow = when (userland) {
        SessionStatus.Userland.Waiting, SessionStatus.Userland.Checking ->
            CheckRow(R.string.row_userland, Tone.PENDING, listOf(Line(R.string.userland_checking)))
        is SessionStatus.Userland.Unpacking -> {
            val fraction = if (userland.total > 0) userland.done.toFloat() / userland.total else 0f
            CheckRow(
                R.string.row_userland, Tone.WORKING,
                listOf(Line(R.string.userland_unpacking, (fraction * 100).toInt(), userland.done shr 20, userland.total shr 20)),
                progress = fraction,
            )
        }
        // Its version is a runtime build id: About shows it, Check does not.
        is SessionStatus.Userland.Ready -> CheckRow(R.string.row_userland, Tone.OK, listOf(Line(R.string.userland_ready)))
        is SessionStatus.Userland.Failed -> CheckRow(R.string.row_userland, Tone.ERROR, listOf(Line(R.string.userland_failed, userland.reason)))
    }

    /** The Launcher's step 1 host check, from the engine's snapshot. */
    fun hostRow(session: SessionStatus): CheckRow {
        val snapshot = session.snapshot
        val pending = { id: Int -> CheckRow(R.string.row_host, Tone.PENDING, listOf(Line(id))) }
        return when {
            session.engine == SessionStatus.Engine.FAILED -> CheckRow(R.string.row_host, Tone.ERROR, listOf(Line(R.string.host_engine_failed)))
            session.engine == SessionStatus.Engine.RESTARTING -> pending(R.string.host_engine_restarting)
            session.userland !is SessionStatus.Userland.Ready -> pending(R.string.host_after_unpack)
            snapshot == null -> pending(R.string.host_engine_starting)
            snapshot.host == Host.CHECKING || auditing(snapshot) -> pending(R.string.host_checking)
            snapshot.host == Host.SUPPORTED -> CheckRow(R.string.row_host, Tone.OK, listOf(Line(R.string.host_supported)))
            gameFolderGone(snapshot) -> CheckRow(R.string.row_host, Tone.ERROR, listOf(Line(R.string.host_game_folder_gone)))
            snapshot.missing.isNotEmpty() -> CheckRow(
                R.string.row_host, Tone.ERROR,
                listOf(Line(R.string.host_missing)) + snapshot.missing.map { Line(hostFinding(it)) },
            )
            // Unsupported with no finding: the check itself failed, and its code says why.
            else -> CheckRow(
                R.string.row_host, Tone.ERROR,
                listOf(Line(R.string.host_check_failed, snapshot.operation?.failure ?: snapshot.result?.message ?: "unknown")),
            )
        }
    }

    /**
     * The Diablo II folder the installation uses in place is gone (the engine's
     * `game-folder-unavailable`): nothing about the game can be checked until Start over.
     */
    private fun gameFolderGone(snapshot: Snapshot): Boolean = snapshot.gameFolder?.available == false

    /**
     * The engine gave up restarting or the session could not start (the game support files
     * failed): nothing starts it again by itself, so the screens offer Try again.
     */
    fun failed(session: SessionStatus): Boolean =
        session.engine == SessionStatus.Engine.FAILED || session.userland is SessionStatus.Userland.Failed

    fun action(session: SessionStatus): MainAction {
        val snapshot = session.snapshot
        return when {
            failed(session) -> MainAction(MainAction.Kind.TRY_AGAIN, true, Line(R.string.hint_try_again))
            snapshot?.host == Host.UNSUPPORTED && !auditing(snapshot) && session.engine == SessionStatus.Engine.CONNECTED ->
                if (gameFolderGone(snapshot)) {
                    MainAction(MainAction.Kind.REPAIR, true, Line(R.string.hint_game_folder_gone))
                } else {
                    MainAction(MainAction.Kind.CHECK_AGAIN, true, Line(R.string.hint_missing))
                }
            snapshot?.host == Host.SUPPORTED && !auditing(snapshot) ->
                MainAction(MainAction.Kind.CONTINUE, true, Line(R.string.hint_continue_to_download))
            else -> MainAction(MainAction.Kind.CONTINUE, false, Line(R.string.hint_wait_for_check))
        }
    }

    /**
     * The host check still runs: the engine can send `unsupported` before the audit has
     * listed its findings, so the result counts only once the audit ended.
     */
    private fun auditing(snapshot: Snapshot): Boolean {
        val operation = snapshot.operation ?: return false
        return operation.phase == Phase.HOST_AUDIT &&
            operation.status in setOf(OperationStatus.QUEUED, OperationStatus.PLANNING, OperationStatus.RUNNING)
    }

    /**
     * The engine's audit has ended, so [snapshot] is evidence. Its first snapshots are a
     * placeholder under the running host audit (host checking, installation needs_base_game, step
     * device) that says nothing about the installation; nothing routes or acts on it.
     */
    fun audited(snapshot: Snapshot): Boolean = snapshot.host != Host.CHECKING && !auditing(snapshot)

    fun deviceFinding(finding: Finding, facts: DeviceFacts): Line = when (finding.kind) {
        Finding.Kind.GPU_NOT_SUPPORTED ->
            if (finding.subject.isEmpty()) Line(R.string.device_gpu_unknown) else Line(R.string.device_gpu_not_supported, finding.subject)
        Finding.Kind.GPU_UNKNOWN_TO_DRIVER -> Line(R.string.device_gpu_unknown_to_driver, finding.subject)
        Finding.Kind.PAGE_SIZE -> Line(R.string.device_page_size, facts.pageSize)
        Finding.Kind.DATA_FOLDER -> Line(R.string.device_data_folder)
        Finding.Kind.NO_SPACE -> Line(R.string.device_no_space, facts.freeBytes shr 20, facts.requiredBytes shr 20)
    }

    /** What a host finding means, in plain words (the desktop's step 1 list, worded for Android). */
    @StringRes
    fun hostFinding(finding: HostFinding): Int = when (finding) {
        HostFinding.DISPLAY_SESSION -> R.string.missing_display_session
        HostFinding.FEX -> R.string.missing_fex
        HostFinding.FEX_ROOT_FS -> R.string.missing_fex_root_fs
        HostFinding.PAGE_SIZE -> R.string.missing_page_size
        HostFinding.WINE_CANNOT_RUN -> R.string.missing_wine_cannot_run
        HostFinding.IA32_EXECUTION -> R.string.missing_ia32_execution
        HostFinding.NO_EXEC -> R.string.missing_no_exec
        HostFinding.I386_LOADER -> R.string.missing_i386_loader
        HostFinding.LIBRARIES -> R.string.missing_libraries
        HostFinding.WINE_LIBRARY_X11 -> R.string.missing_wine_library_x11
        HostFinding.WINE_LIBRARY_XEXT -> R.string.missing_wine_library_xext
        HostFinding.WINE_LIBRARY_FREE_TYPE -> R.string.missing_wine_library_free_type
        HostFinding.WINE_LIBRARY_FONTCONFIG -> R.string.missing_wine_library_fontconfig
        HostFinding.WINE_LIBRARY_OPEN_GL -> R.string.missing_wine_library_open_gl
        HostFinding.LIBUNWIND -> R.string.missing_libunwind
        HostFinding.X_WAYLAND -> R.string.missing_xwayland
        HostFinding.VULKAN_LOADER -> R.string.missing_vulkan_loader
        HostFinding.VULKAN_DRIVER -> R.string.missing_vulkan_driver
        HostFinding.HARDWARE_GRAPHICS -> R.string.missing_hardware_graphics
        HostFinding.LAUNCHER_BUILD -> R.string.missing_launcher_build
        HostFinding.ARCHITECTURE -> R.string.missing_architecture
        HostFinding.HOST_INFORMATION -> R.string.missing_host_information
        HostFinding.UNCERTIFIED_BUILD -> R.string.missing_uncertified_build
        HostFinding.UNKNOWN -> R.string.missing_unknown
    }
}
