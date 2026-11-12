package com.pd2launcher.app

import android.app.ActivityManager
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.graphics.drawable.Icon
import android.net.ConnectivityManager
import android.net.NetworkCapabilities
import android.os.Build
import android.os.IBinder
import android.os.PowerManager
import android.os.SystemClock
import android.system.ErrnoException
import android.system.Os
import android.system.OsConstants
import android.util.Log
import android.view.InputDevice
import com.pd2launcher.app.control.Command
import com.pd2launcher.app.control.ControlChannel
import com.pd2launcher.app.control.DisplaySize
import com.pd2launcher.app.control.EngineMessage
import com.pd2launcher.app.control.Game
import com.pd2launcher.app.control.HostFacts
import com.pd2launcher.app.control.Installation
import com.pd2launcher.app.control.Operation
import com.pd2launcher.app.control.OperationStatus
import com.pd2launcher.app.control.Renderer
import com.pd2launcher.app.control.Settings
import com.pd2launcher.app.control.Snapshot
import com.pd2launcher.app.control.collect
import com.pd2launcher.app.input.ControlsStore
import com.pd2launcher.app.ui.InstallProgress
import com.pd2launcher.app.ui.Play
import java.io.File
import java.io.IOException
import java.util.concurrent.CountDownLatch
import java.util.concurrent.ExecutorService
import java.util.concurrent.Executors
import java.util.concurrent.RejectedExecutionException
import java.util.concurrent.TimeUnit
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.filterNotNull
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.getAndUpdate
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.isActive
import kotlinx.coroutines.job
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeoutOrNull

/** What the session runs now, for the screens. */
data class SessionStatus(
    val engine: Engine = Engine.STOPPED,
    val gameEnvironment: Boolean = false,
    /** Why the game environment did not start for the last Play or install; null once it did. */
    val gameEnvironmentFailure: String? = null,
    val snapshotSeq: Long? = null,
    val userland: Userland = Userland.Waiting,
    /** The engine's newest snapshot while it is connected. */
    val snapshot: Snapshot? = null,
    /** The engine did not take the player's last command; null once another is sent or dismissed. */
    val refusal: Refusal? = null,
    /** An update finished and the engine's check after it has not said yet ([Play.recheck]). */
    val updateRecheck: Play.Recheck? = null,
    /**
     * When a Play or official PD2 launcher request was taken (elapsed real time), until its game
     * or session shows or it fails; the game folder takes no other app's changes meanwhile
     * ([DocumentPaths.gameStarting]).
     */
    val launchRequestedAt: Long? = null,
    /**
     * When a job command was sent ([PrefixStateKeeper.requestsJob]; elapsed real time), until a
     * snapshot shows a job, the engine refuses the command or the engine ends: no capture of the
     * prefix state runs meanwhile ([PrefixStateKeeper.wineRuns]).
     */
    val jobRequestedAt: Long? = null,
    /** The `seq` of the job command that [jobRequestedAt] counts, until the engine refuses it or the engine ends. */
    val jobRequestSeq: Long? = null,
    /**
     * The official PD2 launcher's session runs a game it started (Game.exe among the app's
     * processes, [LauncherGame]): the Game view shows the game's menu and controls, not the
     * launcher's.
     */
    val launcherGame: Boolean = false,
    /** A press of Play's look for a new Project Diablo 2 version, until the game starts or Play shows again ([PlayCheck]). */
    val playCheck: PlayCheck.State? = null,
    /**
     * The route whose graphics driver failed in the last game or game environment start
     * ([PhoneDriverFailure.tells]): Play's error line names its driver, until the next game starts
     * ([launch]) or the pick changes.
     */
    val driverFailed: GraphicsRoute? = null,
    /** The running engine's route of the game's graphics ([GraphicsRoute]), for shared logs; null before an engine started or where none can run. */
    val graphicsRoute: GraphicsRoute? = null,
    /** The Game view shows the card that asks if the game works ([com.pd2launcher.app.ui.RouteCard], [PhoneDriverFailure.asks]). */
    val routeQuestion: Boolean = false,
    /** [graphicsRoute] is the pick in Settings → Display → GPU ([DriverPage.route]), for shared logs. */
    val routeChosen: Boolean = false,
    /** The engine of this install can run the Software driver (its welcome's `gdi_adjmouse`, [DriverPage.softwareRuns]; kept in [RouteState.engineSoftware]). */
    val softwareDriver: Boolean = false,
    /**
     * When the official PD2 launcher's session started (elapsed real time; [LauncherStart.since]),
     * while it runs: the Game view counts the seconds of its start from it.
     */
    val launcherSince: Long? = null,
) {
    enum class Engine { STOPPED, STARTING, CONNECTED, RESTARTING, FAILED }

    /**
     * This status without what the commands of an engine that is gone left in it: the next engine
     * counts its commands from 1 again, so no refusal, update recheck, launch or job of the old
     * one may stay ([SessionService.forgetEngineCommands]).
     */
    fun withoutEngineCommands(): SessionStatus =
        copy(refusal = null, updateRecheck = null, launchRequestedAt = null, jobRequestedAt = null, jobRequestSeq = null)

    companion object {
        /**
         * A new session's status, before its engine says more: Play shows "Getting ready…", and
         * an app update's Install waits ([AppUpdate.waitFor]). Nothing of a session before it stays.
         */
        val STARTING = SessionStatus(engine = Engine.STARTING)
    }

    /** The App userland unpack. */
    sealed interface Userland {
        data object Waiting : Userland
        data object Checking : Userland
        /** [done] of [total] file bytes written. */
        data class Unpacking(val done: Long, val total: Long) : Userland
        data class Ready(val version: String) : Userland
        data class Failed(val reason: String) : Userland
    }
}

/**
 * The service that owns every process: a foreground service, with its notification, only while
 * something must go on in the background ([BackgroundWork.keeps], [followForeground]); the
 * notification's Close stops what runs safely and closes the app ([close]). It owns
 * the Launcher engine always, the Android game environment on demand, and the cleanup after
 * an engine exit (`wineserver -k`, then SIGTERM and SIGKILL after 5 s for every other
 * process of the app's uid) before the engine starts again, after a wait that grows with each
 * failure in a row until it gives up ([RestartPolicy]). Every engine start first ends every other
 * process of the app's uid with SIGKILL, then checks the App userland ([startSession]).
 *
 * Every process-model step runs on one worker thread; reaper threads only hand over to it.
 */
class SessionService : Service() {
    private lateinit var paths: SessionPaths
    /** The device's GPU, probed once: Automatic's graphics choices and the engine's host values follow it. */
    private val gpu by lazy { HostGpu.probe() }
    /** The capture of the prefix state ([keepPrefixState]); it remembers one that Wine's run put off. */
    private val prefixStateKeeper by lazy {
        PrefixStateKeeper(
            paths.managedRoot, ::log,
            ready = { _status.value.snapshot?.installation == Installation.READY },
            wineRuns = { PrefixStateKeeper.wineRuns(_status.value, SystemClock.elapsedRealtime()) },
        )
    }
    private lateinit var serviceLog: RotatingLog
    /** `official-launcher.log`, which [launcherLog] writes. */
    private lateinit var launcherRunLog: RotatingLog
    /** The official PD2 launcher's runs, from a press of its button to its session's end ([OfficialLauncherLog]). */
    private lateinit var launcherLog: OfficialLauncherLog
    private lateinit var starter: ProcessStarter
    private lateinit var game: GameEnvironment
    private val worker: ExecutorService = Executors.newSingleThreadExecutor { Thread(it, "pd2-session") }
    /** The save syncs that wait for nothing ([syncSavesSoon]): off the worker, so a slow folder holds no other step. */
    private val syncWorker: ExecutorService = Executors.newSingleThreadExecutor { Thread(it, "pd2-save-sync") }
    /** Awake while a job downloads or installs ([WorkLocks]); changed on the worker only. */
    private val workLocks by lazy { WorkLocks(this) }
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)

    // Worker thread only.
    private var engine: OwnedProcess? = null
    private var channel: ControlChannel? = null
    private var snapshots: Job? = null
    private var stopping = false
    /** How long the engine waits before it starts again after an unexpected end, or whether not at all. */
    private val restartPolicy = RestartPolicy()
    /** The engine's start after [RestartPolicy]'s wait, while it waits. */
    private var restartJob: Job? = null
    /** Looks for the official PD2 launcher's game while its session runs ([watchLauncherGame]). */
    private var launcherGameWatch: Job? = null
    /** The end of PD2Launcher.exe beside its game ([endLauncher]), while it runs. Worker thread only. */
    private var launcherEnd: Job? = null
    /** The older game folder of a failed update went to the engine as the existing Diablo II ([UpgradeImport]). */
    @Volatile private var upgradeImportSent = false
    /** Keeps the userland's resolv.conf on the default network's DNS servers. */
    private var dns: Dns? = null
    /** The install job sent, owned until the engine settles its outcome ([InstallWork], [InstallProgress]). Worker thread only. */
    private val installWork = InstallWork()
    /** The running X screen's size, sent with `LaunchGame` as D2GL's viewport. */
    private var xScreen: Pair<Int, Int>? = null
    /** The X screen's size came from `--es xscreen` (debug builds), so Play and the official launcher keep it. */
    private var xScreenGiven = false
    /** The `seq` of the install start the game environment was started for, until it is refused or another starts. */
    private var installStart: Long? = null
    /**
     * The official PD2 launcher's command sent ([OpenOfficialLauncher][Command.OpenOfficialLauncher]
     * `seq`, the newest snapshot before it and that snapshot's operation), until its session shows,
     * the engine refuses it, or a snapshot shows that it ended with no snapshot of it running
     * ([Play.officialLauncherSettled]). Worker thread only.
     */
    private var launcherSent: LauncherSent? = null
    private var pendingStart: OperationStart? = null
    private val sessionSnapshots = SessionSnapshots()

    /**
     * The game environment runs for an install start, until it stops ([sendInstallStart]): also
     * between Install Diablo II and Install Project Diablo 2, so the session keeps it
     * ([BackgroundWork.keeps], [SessionIdle.idle]). Set on the worker, read by the session's looks too.
     */
    @Volatile private var installEnvironment = false
    /**
     * The `seq` of the last `LaunchGame` or `OpenOfficialLauncher` sent and when, until the engine
     * shows the game or the session, or refuses it ([Play.launchBusy]). Worker thread only: a
     * snapshot that shows the launch clears it there, after the launch has set it.
     */
    private var launchSent: Pair<Long, Long>? = null
    /** When the Game client began running ([InstallerDownloads.played]); the snapshot collection only. */
    private var gameRunningSince: Long? = null
    /** The player pressed Quit on the card ([quitRoute]): the game's end tells nothing of the driver. Worker thread only. */
    private var gameQuit = false
    /** The present counter when the app's own game started on a route that the app follows ([PhoneDriverFailure.failed]). */
    private var presentsAtGameStart: Long? = null
    /** The player or the app stopped the running game ([stopInGameView]): its end is no driver failure. */
    @Volatile private var gameStoppedByApp = false
    /** The system-driver server's lost contexts when the app's own game started ([PhoneDriverFailure.failed]). */
    private var contextsLostAtGameStart = 0
    /** Whether the game folder was there when the Files roots were last told ([followGameFolder]); worker thread only. */
    private var gameFolderShown: Boolean? = null
    /** The newest start request's id, which an idle end names ([stopWhenIdle]). */
    @Volatile private var lastStartId = 0
    /** An idle end has asked Android to stop this service, and Android took it ([stopWhenIdle]); worker thread only. */
    private var stopAsked = false
    /** The device's memory lines while the game runs ([MemoryLog.Watch]); worker thread only. */
    private val memoryWatch = MemoryLog.Watch()
    /** The engine's snapshots in the service log, a line for each change ([SnapshotLines]): the snapshot collection, Share logs and the service's end. */
    private val snapshotLines = SnapshotLines({ log(it) })
    /** The game's frames in the service log ([FrameWatch]); the frame look only. */
    private val frameWatch = FrameWatch()
    /** The game's frames as the direct present shows them, in the service log ([PresentWatch]); the frame look only. */
    private val presentWatch = PresentWatch()
    /** Detailed logging: D2GL's settings beside the pointer split, in the service log ([D2glWatch]); the frame look only. */
    private val d2glWatch = D2glWatch()
    /** The long folders the logs name by short names ([ProcessStarter.shortened]); set in onCreate. */
    private lateinit var shortFolders: List<Pair<String, String>>
    /** The graphics, Wine and Box64 environment of each Wine program of the game, once ([ProgramEnvironments]). */
    private val programEnvironments by lazy { ProgramEnvironments({ pid -> File("/proc/$pid/environ").readBytes() }, shortFolders) }
    /**
     * The notification's Close runs ([close]): nothing new starts, and the session ends once what
     * ran has stopped. Set on the main thread at the tap, read on every thread.
     */
    @Volatile private var closing = false
    /** [close] has begun; worker thread only. */
    private var closeStarted = false
    /** The service is in the foreground with its notification ([followForeground]); worker thread only. */
    private var inForeground = false
    /** The notification shows Close's progress ([closing]); worker thread only. */
    private var notificationClosing = false
    /**
     * The engine of this start has the player's source of Diablo II: chosen in this start, or sent
     * again after it ([resendChosen]). Cleared at each engine start; set on the worker, read by the
     * snapshot collection too.
     */
    @Volatile private var sourceSent = false
    /** The `seq` of the last copied folder sent to the engine ([SourceImport.BUSY]). */
    @Volatile private var selectSent: Long? = null
    /** Android is ending the service ([onDestroy]): a session start under way does not start the engine. */
    @Volatile private var destroyed = false
    /** Held while Close runs, so its waits go on with the screen off ([close], [finishClose]). */
    private var closeWakeLock: PowerManager.WakeLock? = null

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onCreate() {
        super.onCreate()
        PhoneDriver.init(this)
        PhoneGles.init(this)
        // The records of the removed automatic fallback go (RouteState.OLD_KEYS).
        RouteState.dropOldRecords(this)
        paths = SessionPaths(DeviceCheck.dataDir(this))
        paths.createFolders()
        serviceLog = RotatingLog(paths.logs, "service")
        launcherRunLog = RotatingLog(paths.logs, OfficialLauncherLog.NAME, OfficialLauncherLog.LIMIT, OfficialLauncherLog.KEEP)
        launcherLog = OfficialLauncherLog(launcherRunLog::line, SystemClock::elapsedRealtime)
        // The start lines name the userland's, the APK's and the data folder by short names.
        shortFolders = listOf(paths.userland.path to "<userland>", applicationInfo.nativeLibraryDir to "<apk>", paths.data to "<data>")
        starter = ProcessStarter(paths.logs, serviceLog, ::seccompHarness, shortFolders)
        game = GameEnvironment(paths, starter, ::log, this) { engineRoute }
        SetupRecordStore.load(this)
        // The first notification channel triggers Android 13's permission prompt at targetSdk 28:
        // it comes over Check, once the device check passed (MainActivity). The notification
        // itself shows only while something must go on in the background (followForeground).
        createChannel()
        // Nothing of the session before stays: the one stopped last may not have cleared its status yet.
        _status.value = startingStatus()
        running = this
        log("service started (pid ${Os.getpid()}, uid ${Os.getuid()}, received ${ReceivedBytes.current() ?: "uncounted"} bytes so far)")
        try {
            paths.removeOldRuntimeFiles().takeIf { it.isNotEmpty() }?.let { log("removed what older releases left in ${paths.runtimeDir}: ${it.joinToString()}") }
        } catch (error: Exception) {
            log("older releases' files in ${paths.runtimeDir}: $error")
        }
        // The save backups of releases up to 0.3.4 into Save/Backups, once; what does not move stays where it was.
        try {
            SaveKeeper.moveOldBackups(paths, ::log)
        } catch (error: Exception) {
            log("the older save backups in ${paths.oldSaveBackups} did not all move: $error")
        }
        // How the app's earlier processes ended: a kill for low memory shows in the logs.
        scope.launch(Dispatchers.IO) { logProcessExits() }
        // While the game runs: the device's memory at its start and stop, and when it moves (MemoryLog).
        scope.launch {
            while (true) {
                delay(MemoryLog.POLL_MS)
                post { followMemory() }
            }
        }
        // While the game runs: its frames (FrameWatch, PresentWatch), from its present counter,
        // mapped while it runs, and from the direct present's counts.
        scope.launch(Dispatchers.IO) {
            var counter: PresentCounter.Reader? = null
            while (true) {
                delay(FrameWatch.POLL_MS)
                val status = _status.value
                val running = status.snapshot?.game == Game.RUNNING || status.launcherGame
                if (!running) counter = null
                if (!running && !frameWatch.active && !presentWatch.active) continue
                if (running && counter == null) counter = PresentCounter.reader(paths.presentCounter)
                val settings = AppSettingsStore.current.value
                val rate = settings.frameRate
                val now = SystemClock.elapsedRealtime()
                frameWatch.next(
                    now, running, counter?.presents(), appForeground,
                    if (rate == AppSettings.FULL_FRAME_RATE) "Full" else rate.toString(), ScreenRate.full.value,
                )?.let(::log)
                val server = PresentServer.current.value
                presentWatch.next(
                    now, running, server?.counts(), counter?.ticks(), appForeground, settings.detailedLogging,
                    server?.split(settings.detailedLogging && running),
                ).forEach(::log)
                // Detailed logging: D2GL's settings, read only, at the game's start and at each change.
                if (running && settings.detailedLogging) {
                    d2glWatch.next {
                        GameFilesProvider.gameFolder(paths, status.snapshot?.gameFolder?.path)
                            ?.let { File(it, "${LauncherFiles.FOLDER}/${D2glSettings.FILE}") }
                    }?.let(::log)
                } else {
                    d2glWatch.reset()
                }
            }
        }
        // While the game runs on a route of the frozen-picture rule (FrozenPicture.ROUTES).
        scope.launch {
            while (true) {
                delay(FrozenPicture.LOOK_MS)
                post { followPicture() }
            }
        }
        // While a job downloads or installs: the app's received bytes, for the engine's stall rule.
        scope.launch(Dispatchers.IO) {
            while (true) {
                if (workLocks.held) ReceivedBytes.current()?.let { ReceivedBytes.write(paths.receivedBytes, it) }
                delay(ReceivedBytes.INTERVAL_MS)
            }
        }
        // In the foreground with the notification while something must go on in the background:
        // each change of the session, the folder copy or the app update.
        scope.launch {
            combine(_status, SourceImport.copy, AppUpdater.work) { status, copy, update ->
                BackgroundWork.keeps(
                    status, installEnvironment, copy is SourceCopy.Copying, BackgroundWork.appUpdating(update), SystemClock.elapsedRealtime(),
                )
            }.distinctUntilChanged().collect { post { followForeground() } }
        }
        // Play's look for a new version follows the engine: each snapshot, a refusal, its end.
        scope.launch {
            _status.map { Triple(it.engine, it.snapshotSeq, it.refusal) }.distinctUntilChanged().collect { post { followPlayCheck() } }
        }
        followRoutePick()
        // The session ends by itself once nothing runs and the app's screens went away: at once
        // after the player closed the app, 10 s after it only left the screen (SessionIdle).
        scope.launch {
            var since: Long? = null
            while (true) {
                delay(SessionIdle.POLL_MS)
                // Also when a launch's wait ends, which no change of the session shows.
                post { followForeground() }
                val now = SystemClock.elapsedRealtime()
                val gone = away
                since = SessionIdle.idleSince(since, idle(now), now, gone)
                val idleSince = since ?: continue
                if (SessionIdle.due(idleSince, now, gone)) {
                    since = null
                    post { stopWhenIdle() }
                }
            }
        }
    }

    /**
     * The player swiped the app away while the session runs: when nothing runs that must go on in
     * the background, the session ends at once and the notification goes; the game, the official
     * PD2 launcher or a job runs on, and the session ends once it stops.
     */
    override fun onTaskRemoved(rootIntent: Intent?) {
        super.onTaskRemoved(rootIntent)
        log("the app's task was removed (swiped away from the recent apps)")
        appClosed()
    }

    /**
     * Android's memory pressure on the app's process: the level and the device's memory go to the
     * service log ([MemoryLog]). Nothing is freed for it, and the app keeps its place for Android's
     * memory killer.
     */
    override fun onTrimMemory(level: Int) {
        super.onTrimMemory(level)
        log("onTrimMemory ${MemoryLog.trimLevel(level)}: ${MemoryLog.available(MemoryLog.read())}")
    }

    /** Whether the session may end now ([SessionIdle.idle]). */
    private fun idle(now: Long): Boolean = SessionIdle.idle(
        _status.value, appForeground, installEnvironment, SourceImport.copy.value is SourceCopy.Copying,
        SessionIdle.updating(AppUpdater.check.value, AppUpdater.work.value), now,
    )

    /**
     * On the worker: the session ends while it is still idle; the activity starts it again when
     * the player comes back ([resume]). Only the newest start request may end it: a start that
     * came meanwhile keeps the session. The start id is read first, then [ending] is set, then
     * the session looks again: so a return in between either sees [ending] and starts the session
     * again (Android then refuses this end, or makes a new service), or this look sees the app in
     * front.
     */
    private fun stopWhenIdle() {
        // Once Android took an end, a later look must not clear [ending]: the end goes on. Close
        // ends the session by itself.
        if (stopping || stopAsked || closing) return
        val id = lastStartId
        ending = true
        if (!idle(SystemClock.elapsedRealtime())) {
            ending = false
            return
        }
        log("nothing runs and the app is not on the screen: the session ends")
        if (stopSelfResult(id)) stopAsked = true else ending = false
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        // Started in the foreground ([send]'s fallback): Android needs the notification now; the
        // session leaves the foreground at once when nothing must go on in the background.
        if (intent?.getBooleanExtra(EXTRA_FOREGROUND, false) == true) {
            startForeground(NOTIFICATION_ID, notification(closing))
            post {
                // Again on the worker, which owns the foreground: a look there may have left it meanwhile.
                startForeground(NOTIFICATION_ID, notification(closing))
                inForeground = true
                notificationClosing = closing
                followForeground()
            }
        }
        // The notification's Close: never a start of the session. At once, also while the worker
        // is busy (a session start, a cleanup): nothing new starts, and the notification says that
        // the app closes; the worker does the rest ([close]).
        if (intent?.action == ACTION_CLOSE) {
            if (!closing) {
                closing = true
                startForeground(NOTIFICATION_ID, notification(closing = true))
                post { close() }
            }
            return START_NOT_STICKY
        }
        if (closing) {
            log("${intent?.action} not taken: the session is closing")
            return START_NOT_STICKY
        }
        lastStartId = startId
        ending = false
        // The player's Try again: the failures so far no longer count, and it starts now.
        if (intent?.action == ACTION_RETRY) {
            worker.execute {
                restartPolicy.reset()
                restartJob?.cancel()
            }
        }
        // Any intent starts a session that is not running, but not ahead of a restart's wait; one
        // that starts it after the restarts gave up counts the failures afresh.
        worker.execute {
            if (engine == null && !stopping && !closing && restartJob?.isActive != true) {
                restartPolicy.started()
                startSession()
            }
        }
        when (intent?.action) {
            ACTION_GAME_START -> {
                val size = intent.getStringExtra(EXTRA_X_SCREEN)?.let(::parseSize)
                worker.execute { startGameEnvironment(size) }
            }
            ACTION_GAME_STOP -> worker.execute { stopInGameView() }
            ACTION_PLAY -> worker.execute { play() }
            ACTION_PLAY_WITHOUT_CHECK -> worker.execute { launch() }
            ACTION_OFFICIAL_LAUNCHER -> worker.execute { officialLauncher() }
            ACTION_CHECK_AGAIN -> worker.execute { sendCommand(Command.CheckAgain) }
            ACTION_IMPORT_UPDATED_GAME -> worker.execute { importUpdatedGame() }
            ACTION_USE_BLIZZARD_DOWNLOAD -> worker.execute { sendCommand(Command.UseBlizzardDownload) }
            ACTION_DOWNLOAD -> worker.execute { sendCommand(Command.DownloadAll) }
            ACTION_STOP_SAFELY -> worker.execute { sendCommand(Command.RequestSafeStop) }
            ACTION_INSTALL_PD2 -> worker.execute { installProjectDiablo(press = true) }
            ACTION_SYNC_SAVES -> syncSavesSoon(intent.getStringExtra(EXTRA_SYNC_WHY) ?: "Sync now")
            ACTION_ROUTE_WORKS -> worker.execute { confirmRoute() }
            ACTION_ROUTE_QUIT -> worker.execute { quitRoute() }
        }
        return START_NOT_STICKY
    }

    /**
     * A debug build's forced route ([SystemDriver.forced]), read at each use: a test that adds or
     * removes the file while the app runs gets it at the next Play or official PD2 launcher
     * ([driverChanges]).
     */
    private fun forcedRoute(): GraphicsRoute? = SystemDriver.forced(filesDir)

    /**
     * The phone driver's report where the app may need it ([HostGpu.phoneDriver], whose probe runs
     * once per process): where the built-in driver supports the GPU, only where the player picked
     * the Phone driver ([phonePicked]), or in a debug build (its forced route); [DriverChoice.readsPhoneDriver].
     */
    private fun phoneDriverReport(): SystemDriverGpu? = HostGpu.phoneDriver(
        gpu, forced = DriverChoice.readsPhoneDriver(forced = forcedRoute() != null, BuiltInDriver.debugRules(filesDir), phonePicked()),
    )

    /**
     * The phone's OpenGL ES driver for the VirGL route ([HostGpu.phoneGles], whose probe runs once
     * per process): where the built-in driver supports the GPU, on the same terms as
     * [phoneDriverReport].
     */
    private fun phoneGlesReport(): PhoneGlesDriver? = HostGpu.phoneGles(
        gpu, forced = DriverChoice.readsPhoneDriver(forced = forcedRoute() == GraphicsRoute.VIRGL, BuiltInDriver.debugRules(filesDir), phonePicked()),
    )

    /** The player picked the Phone driver on Settings → Display → GPU ([AppSettings.driverPick]): the app reads the phone's own drivers. */
    private fun phonePicked(): Boolean = AppSettingsStore.load(this).driverPick == PlayerDriver.PHONE

    /** [driverOptions] and whether the phone's own drivers were read for them (for a Phone driver pick, [phonePicked]). */
    private var driverOptionsRead: Pair<Boolean, DriverOptions>? = null

    /**
     * Which routes can run the game here ([DriverChoice.options]); a forced route goes past them
     * ([wantedRoute]). The unpack's skipped files follow them ([otherGpuDriver]). Made once per
     * service, and once more after a pick of the Phone driver, which adds the routes to the phone's
     * own driver.
     */
    private val driverOptions: DriverOptions
        @Synchronized get() {
            val probed = phonePicked()
            driverOptionsRead?.takeIf { it.first == probed }?.let { return it.second }
            return readDriverOptions().also { driverOptionsRead = probed to it }
        }

    private fun readDriverOptions(): DriverOptions =
        DriverChoice.options(
            gpu, phoneDriverReport(), SystemDriver.inApk(this),
            gles = phoneGlesReport(), glServerInApk = SystemDriver.glServerInApk(applicationInfo.nativeLibraryDir),
            hybrisInApk = SystemDriver.hybrisInApk(this),
            // RADV: on an Xclipse where it is offered (to players, or a debug build's file, read
            // once per process) and the APK carries the part it is in; presentDriverOptions checks
            // that the unpacked userland has it.
            radvAllowed = Radv.allowed(filesDir), radvFiles = UserlandAssets.carries(this, UserlandAssets.SYSTEM_DRIVER),
            // mali-panvk on a job-manager Mali, the same way.
            maliPanvkAllowed = MaliPanvk.allowed(filesDir), maliPanvkFiles = UserlandAssets.carries(this, UserlandAssets.SYSTEM_DRIVER),
        )
            .also { options ->
                log("graphics routes that can run the game: $options -> ${options.route ?: "none"}")
                options.orderReason?.let { log("graphics route order: $it") }
            }

    /**
     * [driverOptions] with Venus only while the unpacked App userland has the mode's files
     * ([SystemDriver.filesPresent]; [ensureUserland] writes back those an earlier unpack left out
     * where Venus needs them now, so this only holds where that unpack failed); the same for the
     * libhybris route's, RADV's and mali-panvk's files. [unpackWrites]: the player picked the Phone
     * driver, so the engine's next start writes Venus's files first ([otherGpuDriver]): they count
     * as there.
     */
    private fun presentDriverOptions(unpackWrites: Boolean = phonePicked()): DriverOptions {
        var options = if (driverOptions.venus && !unpackWrites && !SystemDriver.filesPresent(paths.userland, systemDriverZink)) {
            driverOptions.copy(venus = false)
        } else {
            driverOptions
        }
        // The same for the libhybris route's files (DriverChoice.skippedFiles), for RADV's and for mali-panvk's.
        if (options.libhybris && !SystemDriver.hybrisFilesPresent(paths.userland)) options = options.copy(libhybris = false)
        if (options.radv && !Radv.filesPresent(paths.userland)) options = options.copy(radv = false)
        if (options.maliPanvk && !MaliPanvk.filesPresent(paths.userland)) options = options.copy(maliPanvk = false)
        return options
    }

    /** The engine of this install can run the Software driver ([DriverPage.softwareRuns], [RouteState.engineSoftware]; review MED-3). */
    private fun softwareRuns(): Boolean = RouteState.engineSoftware(this, PhoneDriver.installKey)

    /**
     * The route of the pick on Settings → Display → GPU ([AppSettings.driverPick],
     * [DriverPage.route]); null: no route of the pick here, the app's own order.
     */
    private fun chosenRoute(): GraphicsRoute? =
        AppSettingsStore.load(this).let { DriverPage.route(presentDriverOptions(), it.driverPick, it.driverWay, software = softwareRuns()) }

    /**
     * The route of the game's graphics now ([DriverChoice.route]): the forced one, the pick's, or
     * the first that can run. No failure changes it.
     */
    private fun wantedRoute(): GraphicsRoute? = DriverChoice.route(presentDriverOptions(), forcedRoute(), chosenRoute())

    /**
     * The route of the running engine, fixed at its start ([startEngine]): the engine passes the
     * game's Vulkan and GL values to Wine from its own environment, so a change of route starts
     * it again ([applyGraphicsRoute]). The game environment (the system-driver server) follows it.
     * Null: no route can run (the device check refuses such a device), the built-in values then.
     */
    @Volatile
    private var engineRoute: GraphicsRoute? = null

    /**
     * The pick is [engineRoute] now ([chosenRoute]; no debug build's forced route), for the shared
     * logs. Read at its use, not at the engine's start (bug B1: a pick of the same route does not
     * start the engine again, so a value fixed at the start goes stale).
     */
    private fun routeChosen(): Boolean = PhoneDriverFailure.chosen(engineRoute, chosenRoute(), forced = forcedRoute() != null)

    /**
     * The running engine's route is one whose failures the app follows ([PhoneDriverFailure]): a
     * route to the phone's own driver, or the built-in one; not the Software driver, which has no
     * GPU driver.
     */
    private val routeFollowed: Boolean get() = engineRoute != null && engineRoute != GraphicsRoute.SOFTWARE

    /**
     * The route the game needs ([wantedRoute]) is not the running engine's (a route picked in
     * Settings, which the session follows at once, [followRoutePick]; a debug build's forced route
     * added or removed while the app runs, at the next Play or official launcher): when nothing runs, the game environment and the
     * engine start again with it; while something runs (the game, a job, a job command that no
     * snapshot shows yet, a launch, or Play's update check, its question or its update), it waits
     * for that to end ([routeChangePending]). Not while the session closes ([RouteChange]).
     */
    private fun applyGraphicsRoute() {
        // No engine yet: the next one starts on the route that the game needs then ([startEngine]).
        if (engine == null) return
        val wanted = wantedRoute()
        val check = _status.value.playCheck
        // A Close ends the session: no new engine for it ([close]).
        val change = RouteChange.of(wanted, engineRoute, ending = closing || stopping) {
            launchBusy("the graphics route change") || (check != null && check !is PlayCheck.State.Starting) ||
                PrefixStateKeeper.jobRequested(_status.value, SystemClock.elapsedRealtime())
        }
        if (change == RouteChange.NONE) return
        if (change == RouteChange.WAIT) {
            routeChangePending = true
            // A job command that no snapshot shows yet: in an idle session no snapshot may come, so
            // the change also looks again when its wait is over (review F1).
            PrefixStateKeeper.jobRequestLeft(_status.value, SystemClock.elapsedRealtime())?.let { left ->
                log("graphics route change: waits up to $left ms for the job command that no snapshot shows yet")
                scope.launch {
                    delay(left)
                    post { if (routeChangePending) applyGraphicsRoute() }
                }
            }
            return
        }
        routeChangePending = false
        log("graphics route change: ${engineRoute ?: "none"} -> ${wanted ?: "none"}; the engine starts again with it")
        stopGameEnvironment()
        stopEngine()
        forgetEngineCommands()
        _status.update { it.copy(engine = SessionStatus.Engine.STARTING, snapshotSeq = null, snapshot = null, playCheck = null) }
        startSession()
    }

    /** A route change waits for what runs to end ([applyGraphicsRoute]); applied once nothing runs. */
    private var routeChangePending = false

    /**
     * A pick in Settings → Display → GPU ([AppSettings.driverPick], [AppSettings.driverWay]) goes to
     * the engine at once where nothing runs, else once what runs ends ([applyGraphicsRoute]): when
     * the player comes back to Play, the engine runs the new route, and one press of Play starts the
     * game on it (the owner, 2026-10-29: the first Play only started the engine again). A pick of
     * the Phone driver first reads the phone's own drivers, for the session and for the screens
     * ([DeviceCheckRun.readPhoneDrivers]; read once per process).
     */
    private fun followRoutePick() {
        scope.launch {
            // Review LOW: the first value is the stored pick, not the flow's default before the first load.
            var before = AppSettingsStore.load(this@SessionService).let { it.driverPick to it.driverWay }
            AppSettingsStore.current.map { it.driverPick to it.driverWay }.distinctUntilChanged().collect { picked ->
                val pick = picked.first
                val clears = DriverPage.pickClearsNotice(before, picked)
                before = picked
                post {
                    // A new pick: the games with no frame of the route that the player leaves count no more (review LOW-1).
                    if (clears) RouteState.framesShown(this@SessionService)
                    // A new pick: Play's error line of the route that the player leaves goes (a phone check).
                    if (clears && _status.value.driverFailed != null) {
                        RouteState.clearNotice(this@SessionService)
                        _status.update { it.copy(driverFailed = null) }
                    }
                    if (pick == PlayerDriver.PHONE) DeviceCheckRun.readPhoneDrivers()
                    applyGraphicsRoute()
                    // Review LOW-2: the shared logs' field follows a pick of the route already in use too.
                    if (engine != null) _status.update { it.copy(routeChosen = routeChosen()) }
                }
            }
        }
    }

    /**
     * The route the game needs is not the running engine's (its change has not reached the
     * session yet): [what] does not start, and the engine starts again with the route now; Play
     * shows "Getting ready…" until it is back.
     */
    private fun driverChanges(what: String): Boolean {
        if (wantedRoute() == engineRoute) return false
        log("$what not started: the graphics route changes first")
        applyGraphicsRoute()
        return true
    }

    /**
     * The Zink that the system-driver mode uses here ([SystemDriverGpu.zink]): the product Mesa's
     * where the phone's driver meets its needs, else the older one (also when no report answers).
     */
    private val systemDriverZink: SystemDriver.Zink get() = phoneDriverReport()?.zink ?: SystemDriver.Zink.OLDER

    /** The engine's environment for this GPU and the running engine's route ([SessionPaths.engineEnvironmentOn]). */
    private fun engineEnvironment(): List<String> = paths.engineEnvironmentOn(
        gpu, engineRoute, systemDriverZink, Build.VERSION.SDK_INT,
        forcedLibhybrisValues = if (BuildConfig.DEBUG && forcedRoute() == GraphicsRoute.LIBHYBRIS) forcedRouteValues() else emptyList(),
        read = { VulkanDriverProbe.read(paths.vulkanProbeMarker) },
    )

    /**
     * Debug builds: the `NAME=value` lines of the forced route's debug file
     * ([SystemDriver.forceFile]); on the libhybris route they go to the game's Wine processes.
     */
    private fun forcedRouteValues(): List<Pair<String, String>> {
        val route = forcedRoute() ?: return emptyList()
        val file = File(filesDir, SystemDriver.forceFile(route))
        return if (file.isFile) SessionPaths.parseEnvironment(file.readText()) else emptyList()
    }

    /** The frozen-picture rule of the running game ([followPicture]); worker thread only. */
    private val frozenPicture = FrozenPicture()

    /** Whether the game of the last Play draws with GDI, from the engine's line at its start ([GdiGame]); the watches below read it. */
    private val gdiGame = GdiGame()

    /** The RADV route's failure lines in the engine's output since the last Play ([RadvFailureWatch]). */
    private val radvFailure = RadvFailureWatch(gdiGame)

    /** The mali-panvk route's failure lines in the engine's output since the last Play ([MaliPanvkFailureWatch]). */
    private val maliPanvkFailure = MaliPanvkFailureWatch(gdiGame)

    /** The built-in route's failure lines in the engine's output since the last Play ([BuiltInFailureWatch]). */
    private val builtInFailure = BuiltInFailureWatch(gdiGame)

    /** The running game's picture froze while frames came ([FrozenPicture]); read at the game's end. */
    @Volatile private var pictureFroze = false

    /**
     * One look of the frozen-picture rule ([FrozenPicture]) while the app's game runs on the VirGL, the libhybris, the
     * mali-panvk or the built-in route: the present counter and a hash of the game's picture (the app's buffer on the screen
     * or the X screen, [GameEnvironment.pictureHash]). A frozen picture counts as a failure of the route's driver at the game's
     * end, also when a Stop ends it. The built-in route counts only a picture that is still frozen at the
     * game's end, and looks only while the app shows the game. Worker thread only.
     */
    private fun followPicture() {
        val lenient = engineRoute == GraphicsRoute.BUILT_IN
        if (engineRoute !in FrozenPicture.ROUTES || _status.value.snapshot?.game != Game.RUNNING || (pictureFroze && !lenient)) return
        // No one sees the game: the mark goes, and the rule finds a real freeze again once the game shows.
        if (pictureFroze && !appForeground) {
            pictureFroze = false
            frozenPicture.reset()
            return
        }
        // The shown frame not ready to read yet: no look this time, and the rule keeps its time.
        var notReady = false
        val picture = game.pictureHash { notReady = true }
        if (notReady) return
        if (pictureFroze) {
            if (frozenPicture.moved(picture)) {
                pictureFroze = false
                log("the game's picture moved again: no failure of ${driverName(engineRoute)}")
            }
            return
        }
        // A look without a picture starts the rule's time again.
        val shown = if (lenient && !appForeground) null else picture
        if (frozenPicture.feed(SystemClock.elapsedRealtime(), PresentCounter.read(paths.presentCounter), shown)) {
            pictureFroze = true
            log(
                "the game's picture has not changed for ${FrozenPicture.FROZEN_MS / 1000} s while frames came: " +
                    "${driverName(engineRoute)} failed",
            )
        }
    }

    /**
     * Holds (and renews) or releases [workLocks] on the worker, never once the service stops, and
     * says so in the service log when that changes.
     */
    private fun holdWorkLocks(wanted: Boolean) {
        val want = wanted && !stopping
        val changed = workLocks.held != want
        workLocks.hold(want)
        if (changed) {
            val received = ReceivedBytes.current() ?: "uncounted"
            log(if (want) "work locks held: a job downloads or installs (received $received bytes so far)" else "work locks released (received $received bytes so far)")
        }
    }

    /** The processes stop on the worker thread, not the main one (the engine may take 10 s). */
    override fun onDestroy() {
        destroyed = true
        running = null
        log("service stopping")
        // The snapshot collection posts to the worker; it ends before the worker shuts down.
        scope.cancel()
        worker.execute {
            stopping = true
            launcherLog.end("the app's session service stopped")
            holdWorkLocks(false)
            closeWakeLock?.let { if (it.isHeld) it.release() }
            // As in stopGameEnvironment: Wine's processes outlive Xvfb otherwise.
            if (game.running) wineserverKill()
            game.stop()
            stopEngine()
            dns?.stop(this)
            // A service started again meanwhile owns the status.
            if (running == null) _status.value = SessionStatus()
            // The snapshots counted since the last snapshot line, before the service's last line.
            snapshotLines.checkpoint()
            serviceLog.line("service stopped")
            serviceLog.close()
            launcherRunLog.close()
        }
        stoppingWorker = worker
        worker.shutdown()
        // A sync already asked for still runs; none starts after it.
        syncWorker.shutdown()
        super.onDestroy()
    }

    /** [task] on the worker thread; dropped once the service stopped (its worker is shut down). */
    private fun post(task: () -> Unit) {
        try {
            worker.execute(task)
        } catch (_: RejectedExecutionException) {
        }
    }

    /**
     * Waits for a service stopped just before this one ([awaitPreviousService]), ends every other
     * process of the app's user ID ([endOtherProcesses]), puts the way to the App userland right
     * ([UserlandInstaller.repairPlaces]), unpacks the app runtime if needed, makes the App
     * userland's version current with its critical files as the APK has them ([ensureUserland]),
     * then starts the engine; every engine start, a restart after an unexpected end too, goes
     * through here. From the end of the others to the engine's start no other program runs, so
     * none can change a checked file before the engine loads it.
     */
    private fun startSession() {
        try {
            awaitPreviousService()
            // A Close before this session kept new folder copies from starting; this one takes them.
            SourceImport.allowCopies()
            endOtherProcesses()
            // Before anything is unpacked through a link in place of `files`; the folders it took
            // are made again.
            val repaired = UserlandInstaller(paths.userlandRoot, OwnedProcesses) { log("userland: $it") }.repairPlaces()
            if (repaired != null) paths.createFolders()
            // A folder copy that the app's end cut short is never used, and its space comes back.
            try {
                SourceImport.removePartialCopies(paths, ::log)
            } catch (error: Exception) {
                log("folder copies cut short: $error")
            }
            ensureAppRuntime()
            ensureUserland()
            // Android ends the service, or Close runs: the engine does not start for nothing, and
            // the next session does not wait for it.
            if (destroyed || closing) {
                log("the session ends: the engine does not start")
                return
            }
            // Started once; on every later start the servers go in again: an unpack puts back the
            // packed resolv.conf, which names none.
            val known = dns
            if (known != null) {
                known.rewrite()
            } else {
                val emptyForTest = BuildConfig.DEBUG && paths.emptyDnsForTest.exists()
                dns = Dns(listOf(paths.resolvConf), ::log, emptyForTest).also { it.start(this) }
            }
            startEngine()
        } catch (error: Exception) {
            log("session start failed: $error")
            _status.update {
                val userland = it.userland
                it.copy(
                    engine = SessionStatus.Engine.FAILED,
                    userland = if (userland is SessionStatus.Userland.Ready || userland is SessionStatus.Userland.Failed) {
                        userland
                    } else {
                        SessionStatus.Userland.Failed(error.message ?: error.toString())
                    },
                )
            }
        }
    }

    /**
     * A service stopped just before this one stops its processes on its own worker ([onDestroy]),
     * which this process's SIGKILL to the others does not reach: left running, it could start a
     * program after this start's check, or end the new engine in its cleanup. This start waits for
     * it, with a fresh status meanwhile, and fails when it has not finished within
     * [PREVIOUS_STOP_TIMEOUT_MS].
     */
    private fun awaitPreviousService() {
        val previous = stoppingWorker
        if (previous == null || previous === worker || previous.isTerminated) return
        log("waiting for the previous session to stop")
        _status.value = startingStatus()
        if (!previous.awaitTermination(PREVIOUS_STOP_TIMEOUT_MS, TimeUnit.MILLISECONDS)) {
            throw IOException("the previous session did not stop within $PREVIOUS_STOP_TIMEOUT_MS ms")
        }
    }

    /**
     * SIGKILL to every other process of the app's user ID, before the userland is put right and
     * checked: one left running (a guest that left its process group, a program a stop did not end)
     * could change a checked file between the check and the engine's start. `kill(-1)` reaches them all at once,
     * so none dodges it by forking, and spares the calling process: the app runs in this one
     * process (the activity, this service and the documents provider; the manifest names no
     * other). The session keeps nothing running here: the game environment needs a running engine,
     * and the engine's exit ended what it started. Then whatever /proc still lists is killed again
     * until none is left and every process the service started has been reaped, for at most
     * [SWEEP_TIMEOUT_MS]: one still listed then fails the session start. One only not reaped yet
     * is gone already, so it is logged and the start goes on; the installer does not switch
     * `current` while the service counts it as running, which fails the start where a switch is
     * needed ([UserlandInstaller.prepare]).
     */
    private fun endOtherProcesses() {
        val scan = { UidCleanup.scan(File("/proc"), Os.getuid(), Os.getpid(), ::owner) }
        scan().takeIf { it.isNotEmpty() }?.let { others ->
            log("before the userland check: SIGKILL to every other process of the uid: ${others.joinToString { "${it.pid} ${it.name}" }}")
        }
        try {
            Os.kill(-1, OsConstants.SIGKILL)
        } catch (error: ErrnoException) {
            // ESRCH: no other process to signal.
            if (error.errno != OsConstants.ESRCH) log("SIGKILL to every other process: ${error.message}")
        }
        val left = UidCleanup(scan, ::sendSignal, ::log, now = SystemClock::elapsedRealtime)
            .killAll(SWEEP_TIMEOUT_MS) { !OwnedProcesses.anyRunning() }
        if (left.isNotEmpty()) {
            val reason = "other processes of the app did not end: ${left.joinToString { "${it.pid} ${it.name}" }}"
            setUserland(SessionStatus.Userland.Failed(reason))
            throw IOException(reason)
        }
        if (OwnedProcesses.anyRunning()) log("/proc lists no other process of the uid, but the service has not seen every end yet")
    }

    /**
     * The drivers this device never loads, which the unpack leaves out ([DriverChoice.skippedFiles]);
     * for a pick of the Phone driver, those of its routes are written.
     */
    private fun otherGpuDriver(): Set<String> = DriverChoice.skippedFiles(
        gpu,
        // Review LOW-1: only where the pick really is a route of the Phone driver.
        driverOptions.let { if (chosenRoute()?.player == PlayerDriver.PHONE) DriverPage.withoutApp(it) else it },
        phoneDriverReport(), BuiltInDriver.debugRules(filesDir),
    )

    /**
     * The App userland's version from the APK, current and its critical files as the APK has them
     * ([UserlandInstaller.prepare]: unpacked, or unpacked again, as needed); anything else fails
     * the session start, and the engine does not start.
     */
    private fun ensureUserland() {
        // A file an earlier unpack left out that a route needs now (an app update, a new probe
        // result) makes the version incomplete: it is unpacked again here, before the engine starts.
        val installer = UserlandInstaller(
            paths.userlandRoot, OwnedProcesses, skip = otherGpuDriver(), skippable = DriverChoice.skippableFiles,
        ) { log("userland: $it") }
        val manifest = UserlandAssets.manifest(this)
        // A restart's check stays out of sight while it finds the version as it was.
        if (_status.value.userland != SessionStatus.Userland.Ready(manifest.version)) setUserland(SessionStatus.Userland.Checking)
        val started = SystemClock.elapsedRealtime()
        var shownPercent = -1L
        val result = try {
            // Shown as unpacking from the start of each unpack (Play: "Updating game files…"), not
            // once the first file is written after what an older version left has gone.
            installer.prepare(manifest, { UserlandAssets.openTar(this) }) { done, total ->
                // One screen update per percent.
                val percent = if (total > 0) done * 100 / total else 0
                if (percent != shownPercent) {
                    shownPercent = percent
                    setUserland(SessionStatus.Userland.Unpacking(done, total))
                }
            }
        } catch (error: Exception) {
            log("userland ${manifest.version} not ready, so the engine does not start: $error")
            setUserland(SessionStatus.Userland.Failed(error.message ?: error.toString()))
            throw error
        }
        if (result != null) log("userland ${result.outcome} ${result.version} in ${SystemClock.elapsedRealtime() - started} ms")
        setUserland(SessionStatus.Userland.Ready(manifest.version))
    }

    /**
     * The runtime the app provides (the ARM64 Wine, its prefix template and the official launcher's
     * .NET), unpacked once into the Launcher's managed root before the engine starts (a prefix of
     * an older runtime or template generation moved over to it); shown as part of the userland
     * unpack.
     */
    private fun ensureAppRuntime() {
        val (version, bytes) = AppRuntime.info(this) ?: run {
            log("app runtime: none in the APK")
            return
        }
        val runtime = AppRuntime(
            paths.managedRoot, paths.data, ::log,
            keptGame = { paths.upgradeGame },
            userSave = paths.userSave,
            failUpdateForTest = BuildConfig.DEBUG && paths.failUpdateForTest.exists(),
            failUpdateLateForTest = BuildConfig.DEBUG && paths.failUpdateLateForTest.exists(),
        )
        // A failed update's Import that a stop left before the record changed: Install still.
        val setAsideForInstall = {
            if (File(paths.managedRoot, "${LegacyPrefix.OLD}/${LegacyPrefix.SET_ASIDE}").exists() &&
                SetupRecordStore.load(this) == SetupRecord.INSTALLED
            ) {
                SetupRecordStore.setUpAgain(this)
            }
        }
        setAsideForInstall()
        // Repair's Import of the updated game that a stop left part way: finished before the engine starts.
        val legacy = legacyPrefix()
        if (legacy.takingOut()) {
            log("the Import of the updated game was stopped part way; it goes on")
            tookOutUpdatedGame(legacy, legacy.takeOut(paths.upgradeGame))
        }
        if (runtime.isCurrent(version)) {
            // Before the engine starts: what a stop left part way, a copy of an older template in
            // place, or a prefix that is not whole, moves now (AppRuntime.updatePrefixAtStart).
            // Under the prefix lock, which it takes (PrefixState.locked): a capture of the prefix
            // state that an earlier session left on the sync thread never runs during a rebuild.
            runtime.updatePrefixAtStart()?.let {
                log("the update in place failed; Install imports the older game folder $it")
                SetupRecordStore.setUpAgain(this)
            }
            setAsideForInstall()
            return
        }
        val ready = _status.value.userland
        val started = SystemClock.elapsedRealtime()
        var shownPercent = -1L
        // As unpacking from the start: the older version's trees are removed first, which takes
        // seconds on a phone, and Play says "Updating game files…" through it.
        setUserland(SessionStatus.Userland.Unpacking(0, bytes))
        // The unpack ends with the rebuild, under the prefix lock (AppRuntime.install).
        val kept = AppRuntime.openTar(this).use { tar ->
            runtime.install(version, tar) { done ->
                val percent = if (bytes > 0) done * 100 / bytes else 0
                if (percent != shownPercent) {
                    shownPercent = percent
                    setUserland(SessionStatus.Userland.Unpacking(done, bytes))
                }
            }
        }
        log("app runtime $version in ${SystemClock.elapsedRealtime() - started} ms")
        kept?.let {
            log("the update in place failed; Install imports the older game folder $it")
            SetupRecordStore.setUpAgain(this)
        }
        setAsideForInstall()
        setUserland(ready)
    }

    /**
     * The prefix's Windows Installer cache ([InstallerCache]) goes once the game is installed.
     * Run only as the installation's state changes ([AppRuntime.sweep]): never before the
     * engine's audit ends, nor while a job or the game runs.
     */
    private fun keepRuntimeArchives(sweep: AppRuntime.Sweep) {
        try {
            if (sweep == AppRuntime.Sweep.REMOVE) {
                InstallerCache.remove(paths.managedRoot).takeIf { it > 0 }?.let { log("Windows Installer cache: $it .msi files removed") }
            }
        } catch (error: Exception) {
            log("Windows Installer cache: $error")
        }
        // Ready again after an update in place that failed: the older prefix, and the older game
        // folder unless the installation uses it.
        try {
            if (sweep == AppRuntime.Sweep.REMOVE) {
                val inUse = listOf(_status.value.snapshot?.gameFolder?.path, GameFilesProvider.recordedGameFolder(paths))
                UpgradeImport.cleanUp(paths, inUse, ::log)
            }
        } catch (error: Exception) {
            log("after the update's Import: $error")
        }
    }

    /**
     * The "PD2 Launcher" root of Android's Files app is there only while there is a game folder
     * ([GameFilesProvider.queryRoots]); when that changes, the Files app and the file pickers are
     * told to read the roots again, or they keep the old list (no root after an install or an
     * import, until the app restarts).
     */
    private fun followGameFolder() {
        val shown = try {
            GameFilesProvider.documents(this).available()
        } catch (error: Exception) {
            log("game folder: $error")
            return
        }
        if (shown == gameFolderShown) return
        if (gameFolderShown != null) log(if (shown) "the game folder is there: the Files roots change" else "the game folder is gone: the Files roots change")
        gameFolderShown = shown
        GameFilesProvider.rootsChanged(this)
    }

    /**
     * Repair's first choice after an update in place whose moved game the engine rejected
     * ([UpgradeImport.offersImport]): the game folder goes from the prefix to the waiting folder
     * ([LegacyPrefix.takeOut]), the app opens on Install, and the engine checks the installation
     * again; once it finds nothing set up, the waiting folder goes to it as the existing Diablo II
     * (the snapshot collector), so one press of Install imports it.
     */
    private fun importUpdatedGame() {
        val snapshot = _status.value.snapshot
        if (snapshot == null || snapshot.activeJob != null || snapshot.game == Game.RUNNING) return
        try {
            val legacy = legacyPrefix()
            if (tookOutUpdatedGame(legacy, legacy.takeOut(paths.upgradeGame))) sendCommand(Command.CheckAgain)
        } catch (error: Exception) {
            log("Import of the updated game: $error")
        }
    }

    /**
     * After [LegacyPrefix.takeOut] put the game folder [kept] in the waiting folder (null: there was
     * none): the player chose it, the app opens on Install, and the take-out's mark goes last.
     * Whether a folder waits.
     */
    private fun tookOutUpdatedGame(legacy: LegacyPrefix, kept: File?): Boolean {
        if (kept != null) {
            File(kept.parentFile, UpgradeImport.DECLINED).delete()
            upgradeImportSent = false
            SetupRecordStore.setUpAgain(this)
        }
        legacy.tookOut()
        return kept != null
    }

    /**
     * On the worker, so the player's own choices sent before it come first: the copied folder the
     * player chose for Import ([SourceImport.chosen]) goes to an engine started since the choice
     * that has not got it ([SourceImport.resends]), once in that engine's start ([sourceSent], set
     * as it is sent).
     */
    private fun resendChosen() {
        val chosen = SourceImport.chosen ?: return
        val snapshot = _status.value.snapshot ?: return
        if (!SourceImport.resends(snapshot, chosen, chosen.isDirectory, sourceSent)) return
        log("the copied Diablo II folder goes to the engine again after its start: $chosen")
        sendCommand(DiabloSource.selectCommand(chosen.path))
    }

    private fun legacyPrefix() =
        LegacyPrefix(paths.managedRoot, File(paths.managedRoot, AppRuntime.PREFIX_TEMPLATE), paths.userSave, ::log)

    private fun setUserland(userland: SessionStatus.Userland) {
        _status.update { it.copy(userland = userland) }
    }

    /**
     * A player command for the engine; dropped (and logged) while it is not connected, and while
     * the session is closing, but for the safe stop Close sends itself. Returns its `seq` when sent.
     */
    private fun sendCommand(command: Command): Long? {
        val connection = channel
        if (connection == null || connection.state.value !is ControlChannel.State.Connected) {
            log("command $command not sent: the engine is not connected")
            return null
        }
        if (closing && command !is Command.RequestSafeStop) {
            log("command $command not sent: the session is closing")
            return null
        }
        if (command is Command.StartOver && !keepSaves()) return null
        return try {
            connection.send(command).also {
                log("command seq $it: $command")
                if (command is Command.SelectExistingInstall) selectSent = it
                // A job asked for: no capture of the prefix state until a snapshot shows it.
                val job = PrefixStateKeeper.requestsJob(command)
                _status.update { status ->
                    status.copy(
                        refusal = null,
                        jobRequestedAt = if (job) SystemClock.elapsedRealtime() else status.jobRequestedAt,
                        jobRequestSeq = if (job) it else status.jobRequestSeq,
                    )
                }
                // No paused Install Diablo II goes on after Start over or an import.
                if (HeldKeys.forgets(command)) heldKeys.clear()
                noteSourceChoice(command)
            }
        } catch (error: Exception) {
            log("command $command failed: $error")
            null
        }
    }

    /**
     * The older game folder of a failed update ([UpgradeImport]) after [command]: Start over sets
     * nothing up, so it goes to the engine again; another source the player chose stands
     * ([UpgradeImport.DECLINED]).
     */
    private fun noteSourceChoice(command: Command) {
        SourceImport.chosenAfter(command)
        if (command is Command.SelectExistingInstall || command is Command.UseBlizzardDownload) sourceSent = true
        val kept = paths.upgradeGame
        if (command is Command.StartOver) upgradeImportSent = false
        if (!kept.isDirectory || !UpgradeImport.declines(command, kept)) return
        try {
            File(kept.parentFile, UpgradeImport.DECLINED).createNewFile()
            log("another source than the older game folder of the update: it is not offered again")
        } catch (error: IOException) {
            log("the update's Import: $error")
        }
    }

    /** Both CD keys for the engine; cleared whether sent or not, and never logged. Returns its `seq` when sent. */
    private fun sendAuthorize(d2Key: CharArray, lodKey: CharArray): Long? {
        try {
            val connection = channel
            if (connection == null || connection.state.value !is ControlChannel.State.Connected) {
                log("command authorize not sent: the engine is not connected")
                return null
            }
            if (closing) {
                log("command authorize not sent: the session is closing")
                return null
            }
            return connection.sendAuthorize(d2Key, lodKey).also {
                log("command seq $it: authorize")
                _status.update { status -> status.copy(refusal = null) }
            }
        } catch (error: Exception) {
            log("command authorize failed: ${error.javaClass.simpleName}")
            return null
        } finally {
            d2Key.fill('\u0000')
            lodKey.fill('\u0000')
        }
    }

    /**
     * The engine did not take command [error]'s `seq`: the screens show why, and an install start
     * the game environment was started for stops it again, so nothing idles ([Refusals]).
     */
    private fun onCommandError(error: EngineMessage.CommandError) {
        if (pendingStart?.clearCommand == error.seq) cancelOperationStart()
        // The job asked for does not come: a capture of the prefix state that it put off runs now.
        // Not while a job runs: then the refusal was an earlier command's (review F4, the InstallWork.refused rule).
        if (PrefixStateKeeper.jobRefused(_status.value, error.seq)) {
            _status.update { it.copy(jobRequestedAt = null, jobRequestSeq = null) }
            keepPutOffPrefixState()
        }
        log("engine command error: seq ${error.seq}: ${error.reason}")
        launcherLog.refused(error.seq, error.reason)
        // Play's renderer command was refused: the launch goes on at once with the stored renderer ([GlideFirst]).
        if (error.seq in glideSeqs && glideAskedAt != null) {
            glideRefused = true
            glideAnswered()
        }
        // The removal for a picked folder was refused (another job runs): the downloads' files
        // stay, and the player asked for nothing that did not start.
        if (error.reason == DiabloSource.DISCARD_REFUSED) return
        // The engine took no folder while a job ran: the copied folder goes again once no job
        // runs ([resendChosen]); the screens wait for it.
        if (error.seq == selectSent && error.reason == SourceImport.BUSY) {
            selectSent = null
            sourceSent = false
            log("the copied Diablo II folder waits until no job runs, then goes to the engine again")
            return
        }
        _status.update { it.copy(refusal = Refusal(error.seq, error.reason)) }
        if (Refusals.stopsGameEnvironment(error.seq, installStart, _status.value.snapshot)) {
            log("install start seq ${error.seq} refused; stopping the game environment started for it")
            stopGameEnvironment()
        }
        // A refused install owns nothing: its environment no longer keeps the session busy. Not
        // while its job runs: then the refusal was an earlier command's ([InstallWork.refused]).
        if (installWork.refused(error.seq, _status.value.snapshot)) log("install seq ${error.seq} refused; it owns nothing now")
        if (error.seq == installStart && installWork.sequence != error.seq) installStart = null
        if (error.seq == launcherSent?.seq) launcherSent = null
        if (error.seq == launchSent?.first) {
            launchSent = null
            launchRequested(null)
        }
    }

    private fun startEngine() {
        // The last engine's snapshots counted since its last line; the new one's first gets a line.
        snapshotLines.restart()
        _status.update { it.copy(engine = SessionStatus.Engine.STARTING) }
        // The graphics driver this engine passes on to the game, fixed for its life.
        val forced = forcedRoute()
        val picked = AppSettingsStore.load(this).driverPick
        val chosen = chosenRoute()
        engineRoute = wantedRoute()
        val routeChosen = routeChosen()
        _status.update { it.copy(graphicsRoute = engineRoute, routeChosen = routeChosen) }
        if (picked == PlayerDriver.PHONE && chosen == null) {
            log("graphics: the Phone driver picked in Settings cannot run here (${presentDriverOptions().routes}): the app's order instead")
        }
        // How the route came: the debug file, the pick on Settings → Display → GPU, or none (the app's order).
        val how = when {
            forced != null -> ", forced by the debug file"
            routeChosen -> ", the pick in Settings: ${picked?.name?.lowercase()} driver"
            else -> ""
        }
        when (engineRoute) {
            GraphicsRoute.VENUS -> log(
                "graphics: route VENUS, the phone's own Vulkan driver (system-driver mode, Zink ${systemDriverZink.name.lowercase()})" +
                    "$how: ${phoneDriverReport() ?: gpu ?: "no GPU named"}",
            )
            GraphicsRoute.VIRGL -> log(
                "graphics: route VIRGL, the phone's own OpenGL ES driver (system-driver mode, Mesa's virgl)" +
                    "$how: ${phoneGlesReport() ?: gpu ?: "no GPU named"}",
            )
            GraphicsRoute.LIBHYBRIS -> log(
                "graphics: route LIBHYBRIS, the phone's own Vulkan driver in the game's process (libhybris, Zink on libvulkan_pd2hybris.so, " +
                    "API level ${Build.VERSION.SDK_INT})" +
                    "$how: ${phoneDriverReport() ?: gpu ?: "no GPU named"}",
            )
            GraphicsRoute.RADV -> log(
                "graphics: route RADV, our RADV for Samsung Xclipse (radv-xclipse, Zink current, the GPU's DRM node, no server)" +
                    "${if (DriverChoice.RADV_FOR_PLAYERS) "" else ", offered by the debug file files/${Radv.DEBUG_FILE}"}$how: " +
                    "${phoneDriverReport() ?: gpu ?: "no GPU named"}",
            )
            GraphicsRoute.MALI_PANVK -> log(
                "graphics: route MALI_PANVK, mali-panvk on the Mali's kbase driver (Zink current, /dev/mali0, no server)" +
                    "${if (DriverChoice.MALI_PANVK_FOR_PLAYERS) "" else ", offered by the debug file files/${MaliPanvk.DEBUG_FILE}"}$how: " +
                    "${MaliPanvk.gpu(gpu, phoneDriverReport()) ?: gpu ?: "no GPU named"}",
            )
            GraphicsRoute.BUILT_IN -> log("graphics: route BUILT_IN, the built-in driver$how")
            GraphicsRoute.SOFTWARE -> log("graphics: route SOFTWARE, no GPU driver for the game (DirectDraw with GDI)$how")
            null -> log("graphics: no route can run the game here; the built-in driver's values")
        }
        val enginePath = File(applicationInfo.nativeLibraryDir, ENGINE_LIBRARY)
        if (!enginePath.isFile) throw IOException("the engine is not in the APK ($enginePath)")
        val loader = paths.loader.path
        log("UBWC flag hint: ${UbwcFlagHint.choice(gpu, read = { VulkanDriverProbe.read(paths.vulkanProbeMarker) }).describeOn(engineRoute)}")
        val pair = Spawner.socketPair()
        val process = try {
            // Started through the userland's loader.
            starter.start(
                "engine", loader, listOf(loader, enginePath.path, "--engine"),
                engineEnvironment() + gameSessionEnvironment(), pair.child,
                // Each line also to the official PD2 launcher's run while one is open (with its
                // time, as in engine.log); debug builds: the engine's own lines
                // (`pd2-launcher: …`) also in logcat.
                mirror = { line ->
                    if (BuildConfig.DEBUG && line.startsWith(ENGINE_LINE)) Log.i(ENGINE_TAG, line)
                    launcherLog.engineLine(line)
                    // DirectDraw draws with GDI in this game: no GPU, so no failure line below counts.
                    gdiGame.feed(line)
                    // The RADV route: Zink found no RADV device or could not make one (strong
                    // evidence for the next route at the game's end).
                    if (engineRoute == GraphicsRoute.RADV && radvFailure.feed(line)) {
                        post { log("RADV could not make its device: ${radvFailure.failure}") }
                    }
                    // The mali-panvk route: Zink found no device on it, or the GPU faulted (strong evidence too).
                    if (engineRoute == GraphicsRoute.MALI_PANVK && maliPanvkFailure.feed(line)) {
                        post { log("mali-panvk failed in the game: ${maliPanvkFailure.failure}") }
                    }
                    // The built-in route: Zink found or made no device, the device was lost, or our PanVK's queue group failed (strong evidence too).
                    if (engineRoute == GraphicsRoute.BUILT_IN && builtInFailure.feed(line)) {
                        post { log("the built-in driver failed in the game: ${builtInFailure.failure}") }
                    }
                },
            ) { exited, status -> post { onEngineExit(exited, status) } }
        } catch (error: IOException) {
            pair.app.close()
            throw error
        } finally {
            pair.child.close()
        }
        engine = process

        val connection = ControlChannel.overSocket(pair.app) { error -> post { onCommandError(error) } }
        channel = connection
        try {
            val welcome = connection.handshake(appVersion(), HostFacts.collect(this), HANDSHAKE_TIMEOUT_MS)
            log("engine pid ${process.pid} welcome: protocol ${welcome.protocol}, launcher ${welcome.launcherVersion}, capabilities ${welcome.capabilities}")
            // Review MED-3: the Software driver only with an engine that can run it; kept for the next start.
            val software = DriverPage.softwareRuns(welcome)
            if (software != softwareRuns()) {
                RouteState.engineSoftware(this, PhoneDriver.installKey, software)
                log("graphics: the engine ${if (software) "can" else "cannot"} run the Software driver; the route is read again")
                post { applyGraphicsRoute() }
            }
            _status.update { it.copy(softwareDriver = software) }
            // Only an engine that answered proves the userland works; every older version goes now.
            UserlandInstaller(paths.userlandRoot, OwnedProcesses) { log("userland: $it") }.markStarted()
        } catch (error: Exception) {
            // An engine that hangs before its welcome would never exit by itself: it and what it
            // started go, and its exit runs the cleanup and the restart (or gives up).
            log("engine pid ${process.pid} handshake failed: $error; SIGKILL to its process group")
            connection.close()
            killProcessGroup(process.pid)
            _status.update { it.copy(engine = SessionStatus.Engine.RESTARTING) }
            return
        }
        restartPolicy.connected(SystemClock.elapsedRealtime())
        // A new engine has no source the player chose before its start, unless it kept it.
        sourceSent = false
        _status.update { it.copy(engine = SessionStatus.Engine.CONNECTED) }
        // The reader closes the channel on the engine's end or on a message it cannot decode.
        scope.launch {
            connection.state.first { it is ControlChannel.State.Closed }
            post { onChannelClosed(connection, process) }
        }
        snapshots?.cancel()
        snapshots = scope.launch {
            connection.snapshot.filterNotNull().collect { message ->
                post { applySnapshot(connection, message) }
            }
        }
    }

    /**
     * What belonged to an engine that is gone ([onEngineExit], [applyGraphicsRoute]): the next
     * engine counts its commands from 1 again, so no install start, owned install, launch,
     * refusal or update recheck of the old one may match one of its commands.
     */
    private fun forgetEngineCommands() {
        installStart = null
        installEnvironment = false
        installWork.clear()
        launcherSent = null
        launchSent = null
        // Play's wait for the renderer belonged to that engine: no game starts later from it
        // without a new press of Play (review of 9686c1f2, LOW 2).
        glideAskedAt = null
        glideSeqs = emptyList()
        glideRefused = false
        _status.update { it.withoutEngineCommands() }
    }

    private fun applySnapshot(connection: ControlChannel, message: EngineMessage.SnapshotMessage) {
        val next = message.snapshot
        val now = SystemClock.elapsedRealtime()
        val before = _status.value
        val previous = before.snapshot
        val installOwned = installWork.sequence != null
        // The official launcher's session sent started and ended between two snapshots: no
        // snapshot showed it running, and this one shows its end.
        val launcherSettled = launcherSent?.let { Play.officialLauncherSettled(it.afterSnapshot, it.before, message.seq, next) } == true
        var continueInstall = false
        val applied = sessionSnapshots.apply(
            connection, channel?.takeIf { it.state.value is ControlChannel.State.Connected }, message.seq,
            cleanup = {
                // Install Diablo II done: Install Project Diablo 2 follows; after the last
                // install job the game environment stops.
                continueInstall = installWork.shouldContinueToPd2(message.seq, next, ::stopGameEnvironment)
                if (Play.gameEnded(previous?.game, next.game) && EarlyGameEnd.early(gameRunningSince, now)) {
                    logEarlyGameEnd(now - gameRunningSince!!)
                }
                // Read before the game environment goes: the phone's own driver in the game that ended.
                if (Play.gameEnded(previous?.game, next.game)) followPhoneDriverAtGameEnd(now)
                if (Play.gameEnded(previous?.game, next.game) ||
                    Play.officialLauncherEnded(previous?.activeJob, next.activeJob) ||
                    (launcherSettled && game.running && installWork.sequence == null)) {
                    stopGameEnvironment()
                }
            },
            publish = {
                _status.update { it.copy(
                    snapshotSeq = message.seq, snapshot = next, updateRecheck = Play.recheck(it.updateRecheck, it.snapshot, next),
                    playCheck = PlayCheck.observe(it.playCheck, next),
                    launcherSince = LauncherStart.since(it.launcherSince, it.snapshot?.activeJob, next.activeJob, now),
                    // The job asked for shows (in the same update, so no status shows neither).
                    jobRequestedAt = it.jobRequestedAt.takeUnless { next.activeJob != null },
                ) }
            },
        )
        if (!applied) return
        snapshotLines.next(summary(message), message.seq, System.currentTimeMillis())
        launcherLog.snapshot(next, ::otherProcesses)
        // The engine's check after a finished update shows as Checking at once; were that
        // snapshot missed (and the check found Available), Play waits at most this long.
        if (before.updateRecheck == null && _status.value.updateRecheck == Play.Recheck.WAITING) {
            scope.launch {
                delay(RECHECK_WAIT_MS)
                _status.update { if (it.updateRecheck == Play.Recheck.WAITING) it.copy(updateRecheck = null) else it }
            }
        }
        // How far the installation got, by the app's own record: where the next start lands
        // (SetupRecord). Written once the screens have the snapshot: written first, the new
        // record met the snapshot before it for a frame (installed, with the install job
        // still running: Repair); the other way round both route to the same screen.
        SetupRecordStore.follow(this@SessionService, next)?.let { record ->
            log("setup record: $record")
            // Start over took the installation: the downloads notice waits for the next one.
            if (record == SetupRecord.NONE) {
                AppSettingsStore.startedOver(this@SessionService)
                // Try anyway is asked again before the new setup.
                DeviceCheckRun.startedOver()
            }
        }
        post { applyBackgroundSound() }
        post { holdWorkLocks(WorkLocks.wanted(next)) }
        // An install or an import makes the game folder, and Start over removes it.
        if (previous == null || previous.installation != next.installation || previous.gameFolder != next.gameFolder ||
            (previous.activeJob == null) != (next.activeJob == null)
        ) {
            post { followGameFolder() }
        }
        // An install, a Project Diablo 2 install or a repair ran an installer: what it wrote outside
        // the game folder is kept at the next Ready (PrefixState, the source Blizzard's installers).
        if (previous?.activeJob in INSTALLER_JOBS && next.activeJob != previous?.activeJob) {
            onSyncWorker { runCatching { PrefixState(paths.managedRoot, ::log).markCaptureDue() }.onFailure { log("prefix state: $it") } }
        }
        AppRuntime.sweep(previous, next)?.let { sweep ->
            post { keepRuntimeArchives(sweep) }
            if (sweep == AppRuntime.Sweep.REMOVE) onSyncWorker { keepPrefixState() }
        }
        // After an update in place that failed: the older game folder, for Install to import;
        // once a session, again at the next start, until the player chooses another source.
        val kept = paths.upgradeGame
        val declined = File(kept.parentFile, UpgradeImport.DECLINED)
        if (!upgradeImportSent && UpgradeImport.selects(next, kept, kept.isDirectory, declined.exists())) {
            upgradeImportSent = true
            log("the older game folder of the update goes to the engine for Import: $kept")
            post { sendCommand(DiabloSource.selectCommand(kept.path)) }
        }
        // The folder the player copied in for Import, which the engine holds in memory
        // only until its Download: an engine started since (after the session ended in
        // the background) gets it again ([resendChosen]).
        if (SourceImport.chosen != null && !sourceSent) post { resendChosen() }
        // Diablo II and Lord of Destruction are in, or their install failed: the held keys are done with.
        if (HeldKeys.spent(previous, next)) heldKeys.clear()
        // The launch sent has shown: Play and the official PD2 launcher take presses again, and
        // the running game or session holds the game folder from here (GameFilesProvider).
        // On the worker: a snapshot quicker than the launch's own return would otherwise be
        // cleared first and the launch's seq set after it, holding the buttons back.
        if (next.game == Game.RUNNING || next.activeJob == com.pd2launcher.app.control.Job.OFFICIAL_LAUNCHER) {
            launchSent = null
            if (next.activeJob == com.pd2launcher.app.control.Job.OFFICIAL_LAUNCHER) launcherSent = null
            if (installWork.sequence == null) installStart = null
            launchRequested(null)
        }
        // The launcher's session ended unseen (its game environment stopped in the cleanup above):
        // Play and the official launcher take presses again, and the Save syncs as after a session.
        if (launcherSettled) {
            log("official PD2 launcher: its session (seq ${launcherSent?.seq}) started and ended between two snapshots")
            launcherSent = null
            launchSent = null
            if (installWork.sequence == null) installStart = null
            launchRequested(null)
            post { syncSavesSoon("after the official launcher") }
        }
        if (Play.gameStarted(previous?.game, next.game)) {
            gameRunningSince = now
            presentsAtGameStart = if (PhoneDriverFailure.countsFrames(engineRoute, next.settings.renderer)) {
                PresentCounter.read(paths.presentCounter)
            } else {
                null
            }
            // Without a present counter It works could never count ([PhoneDriverFailure.answerCounts]): no card then.
            val asks = presentsAtGameStart != null && routeAsks(next.settings.renderer)
            if (asks) log("the card asks if the game works on route $engineRoute (the first game on it since another route ran)")
            _status.update { it.copy(routeQuestion = asks) }
            contextsLostAtGameStart = game.systemDriverContextsLost()
            frozenPicture.reset()
            pictureFroze = false
            post { followMemory() }
        }
        // The Game client exited: the game environment goes with it.
        if (Play.gameEnded(previous?.game, next.game)) {
            // The device's memory after the game's end: Game.exe has exited, and after Stop
            // in the Game view the game environment has stopped too.
            post { followMemory() }
            // The game environment stopped in the cleanup above: what the game saved goes to the sync folder.
            post { syncSavesSoon("after the game") }
            // A game session that ran: Play's one-time notice may show from now on
            // (InstallerDownloads.showNotice); one that ended at once does not count.
            if (InstallerDownloads.played(gameRunningSince, now)) post { AppSettingsStore.gamePlayed(this@SessionService) }
            gameRunningSince = null
        }
        // So does the official PD2 launcher's session; its run in the log waits a while for the operation's end.
        if (Play.officialLauncherEnded(previous?.activeJob, next.activeJob)) {
            post { syncSavesSoon("after the official launcher") }
            scope.launch {
                delay(OfficialLauncherLog.OPERATION_END_MS)
                launcherLog.settle()
            }
        }
        // While it runs, whether it has started the game.
        post { watchLauncherGame(next.activeJob == com.pd2launcher.app.control.Job.OFFICIAL_LAUNCHER) }
        // The install job this snapshot settled no longer holds launches back.
        if (installOwned && installWork.sequence == null) installStart = null
        if (continueInstall) installProjectDiablo(press = false)
        // A picked folder the engine accepted (in time, late, or sent again): the import really
        // starts, so what the Launcher's own downloads made goes, once for each copy.
        if (SourceImport.acceptedFirst(next.installer)) {
            log("the copied Diablo II folder is accepted: the downloads' files are removed")
            sendCommand(Command.DiscardDownloads)
        }
        // The keys joined Download and no install follows it (it failed or stopped, or no keys
        // wait any more): the game environment started for them stops, and the install it was
        // started for owns nothing.
        if (KeysWhileDownloading.noInstallFollows(next) && installEnvironment && installWork.sequence != null) {
            log("the Download that the keys joined ended without the install; stopping the game environment")
            installWork.clear()
            installStart = null
            stopGameEnvironment()
        }
        val pending = pendingStart
        if (pending != null && pending.observe(connection, message.seq, next)) pendingStart = null
        followPlayCheck()
        // Play's wait for Glide: this snapshot may have it.
        glideAnswered(next.settings)
        // A graphics driver change that waited for what ran: now, once nothing runs.
        if (next.game != Game.RUNNING && next.activeJob == null) post { if (routeChangePending) applyGraphicsRoute() }
    }

    /**
     * The app's own game ended ([applySnapshot], before the cleanup stops the game environment,
     * whose values these are): whether the route's graphics driver (the phone's own, RADV,
     * mali-panvk, or the built-in Turnip or PanVK) failed in it ([PhoneDriverFailure];
     * [phoneDriverFailed] runs after the cleanup), or showed frames ([RouteState.framesShown]; the
     * route is then confirmed, [RouteState.confirm]: no card on it in the next game). After the
     * card's Quit nothing counts: the player picks another driver on the GPU page. Worker thread only.
     */
    private fun followPhoneDriverAtGameEnd(now: Long) {
        val quit = gameQuit
        gameQuit = false
        _status.update { it.copy(routeQuestion = false) }
        val since = gameRunningSince
        val lost = game.systemDriverContextsLost() - contextsLostAtGameStart
        val ran = since?.let { now - it } ?: 0L
        val presentsAtEnd = PresentCounter.read(paths.presentCounter)
        val froze = pictureFroze
        // The libhybris route: the ICD's word on the phone's driver in the game ([LibhybrisStatus]).
        val hybrisLines = if (engineRoute == GraphicsRoute.LIBHYBRIS) readHybrisStatus() else null
        val gdi = gdiGame.gdi
        if (gdi) log("this game drew with GDI (no GPU): failed OpenGL or Vulkan starts in it are no driver failure")
        val hybrisFailure = LibhybrisStatus.gameFailure(hybrisLines, gdi)
        hybrisLines?.takeIf { it.isNotBlank() }?.let { text ->
            log("libhybris route, this game: ${text.trim().lines().joinToString(" | ")}")
        }
        // The RADV route: Zink found no RADV device or could not make one ([RadvFailureWatch]).
        val radvFailed = engineRoute == GraphicsRoute.RADV && radvFailure.failure != null
        // The mali-panvk route: Zink found no device on it, or the GPU faulted ([MaliPanvkFailureWatch]).
        val maliPanvkFailed = engineRoute == GraphicsRoute.MALI_PANVK && maliPanvkFailure.failure != null
        // The built-in route: Zink found or made no device, the device was lost, or our PanVK's
        // queue group failed ([BuiltInFailureWatch]); in a debug build also its debug file.
        val builtInOnPurpose = engineRoute == GraphicsRoute.BUILT_IN && BuiltInDriver.failsOnPurpose(filesDir)
        val builtInFailed = engineRoute == GraphicsRoute.BUILT_IN && builtInFailure.failure != null
        val launcherFailed = radvFailed || maliPanvkFailed || builtInFailed || builtInOnPurpose
        if (quit) {
            log("the game ended after Quit on the card: no driver change (the GPU page opened)")
        } else if (hybrisFailure != null) {
            post { phoneDriverFailed("the phone's Vulkan driver failed in the game: $hybrisFailure", strong = true) }
        } else if (PhoneDriverFailure.failed(routeFollowed, presentsAtGameStart, presentsAtEnd, lost, gameStoppedByApp, ran, froze || launcherFailed)) {
            val strong = lost > 0 || froze || launcherFailed
            val reason = when {
                lost > 0 -> "its server gave up $lost context(s) of the game"
                radvFailed -> "RADV could not make its device (${radvFailure.failure})"
                maliPanvkFailed -> "mali-panvk failed in the game (${maliPanvkFailure.failure})"
                builtInFailed -> "the built-in driver failed in the game (${builtInFailure.failure})"
                builtInOnPurpose -> "the debug file files/${BuiltInDriver.DEBUG_FILE} makes the built-in route fail"
                froze -> "the game's picture froze while frames came"
                else -> "the game ended without a frame"
            }
            post { phoneDriverFailed(reason, strong) }
        } else if (routeFollowed && presentsAtGameStart != null && presentsAtEnd != null && presentsAtGameStart != presentsAtEnd) {
            // Frames on the route: earlier games with no frame are no longer in a row, and the route is
            // confirmed: no card on it in the next game (the owner, 2026-11-04, as the old rescue rule).
            val route = engineRoute
            post {
                RouteState.framesShown(this@SessionService)
                if (route != null && forcedRoute() == null) RouteState.confirm(this@SessionService, route)
            }
        }
        presentsAtGameStart = null
    }

    /** Closing the channel makes the engine stop safely and exit; SIGKILL if it does not in time. */
    private fun stopEngine() {
        cancelOperationStart()
        val process = engine ?: return
        channel?.close()
        if (!awaitExit(process, ENGINE_STOP_TIMEOUT_MS)) {
            log("engine pid ${process.pid} did not exit within $ENGINE_STOP_TIMEOUT_MS ms; SIGKILL")
            sendSignal(process.pid, UidCleanup.SIGKILL)
        }
        engine = null
    }

    /**
     * [connection] closed while [process] still counts as the engine and the service did not close
     * it (that closes or forgets the channel first): no command reaches the engine any more, so
     * the screens stop showing it as connected at once, and an engine that does not exit by
     * itself within [ENGINE_STOP_TIMEOUT_MS] goes with its process group; its exit then runs the
     * cleanup and the restart as any other.
     */
    private fun onChannelClosed(connection: ControlChannel, process: OwnedProcess) {
        if (stopping || channel !== connection || engine?.pid != process.pid) return
        cancelOperationStart()
        val cause = (connection.state.value as? ControlChannel.State.Closed)?.cause
        log("the Control channel closed (${cause ?: "the engine ended it"}); engine pid ${process.pid} has $ENGINE_STOP_TIMEOUT_MS ms to exit")
        launcherLog.note("the engine's Control channel closed (${cause ?: "the engine ended it"})")
        gameQuit = false
        _status.update { it.copy(engine = SessionStatus.Engine.RESTARTING, snapshotSeq = null, snapshot = null, routeQuestion = false) }
        // No session without the engine: no launcher's game to look for.
        watchLauncherGame(false)
        scope.launch {
            delay(ENGINE_STOP_TIMEOUT_MS)
            post {
                if (engine?.pid != process.pid) return@post
                log("engine pid ${process.pid} did not exit after its channel closed; SIGKILL to its process group")
                killProcessGroup(process.pid)
            }
        }
    }

    private fun onEngineExit(process: OwnedProcess, status: ExitStatus) {
        if (engine?.pid != process.pid) return
        engine = null
        cancelOperationStart()
        snapshots?.cancel()
        channel?.close()
        channel = null
        if (stopping) return
        val ended = SystemClock.elapsedRealtime()
        log("engine pid ${process.pid} ended unexpectedly (${ProcessStarter.describe(status)}); cleaning up before a restart")
        launcherLog.end("the engine ended unexpectedly (${ProcessStarter.describe(status)})")
        gameQuit = false
        _status.update { it.copy(engine = SessionStatus.Engine.RESTARTING, snapshotSeq = null, snapshot = null, routeQuestion = false) }
        watchLauncherGame(false)
        holdWorkLocks(false)
        // Awake for the cleanup: with the screen off the device suspends between polls, and the
        // 5 s grace is counted in elapsed real time (which includes suspend).
        val wakeLock = getSystemService(PowerManager::class.java).newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "pd2:cleanup")
        wakeLock.acquire(CLEANUP_WAKE_MS)
        try {
            wineserverKill()
            uidCleanup()
        } finally {
            if (wakeLock.isHeld) wakeLock.release()
        }
        game.forget()
        xScreen = null
        forgetEngineCommands()
        _status.update { it.copy(gameEnvironment = false) }
        keepPutOffPrefixState()

        // Close ends the session: no new engine.
        if (closing) {
            log("the session is closing: the engine does not start again")
            return
        }
        val wait = restartPolicy.failed(ended)
        if (wait == null) {
            log("engine ended ${RestartPolicy.MAX_FAILURES + 1} times in a row; not restarting until the player tries again")
            _status.update { it.copy(engine = SessionStatus.Engine.FAILED) }
            return
        }
        log("restarting the engine in ${wait / 1000} s")
        restartJob = scope.launch {
            delay(wait)
            // The whole session start: the userland's critical files are checked again first.
            post { if (engine == null && !stopping && !closing) startSession() }
        }
    }

    /**
     * `wineserver -k` for the Managed prefix: the engine binary's `--stop-wine` runs Wine-GE's
     * wineserver through App-provided FEX with the game's Wine environment, as the engine runs
     * its guests, so it works whether or not an engine is running. Waits at most
     * [WINESERVER_TIMEOUT_MS]; after that, what it started goes (its process group), and so does
     * every other process of the app's user ID but the engine ([uidCleanup]): the wineserver and
     * the Wine processes it failed to end would outlive it otherwise. It starts from the App
     * userland as it is, without a check of its own: Wine's processes still run then, so a check
     * could not keep them from changing a file before the start. It never gets the CD keys, and
     * whatever it leaves running ends before the next engine start's check ([endOtherProcesses]).
     */
    private fun wineserverKill() {
        val enginePath = File(applicationInfo.nativeLibraryDir, ENGINE_LIBRARY)
        val loader = paths.loader.path
        try {
            val process = starter.start(
                "stop-wine", loader, listOf(loader, enginePath.path, "--stop-wine"), engineEnvironment(),
            )
            if (!awaitExit(process, WINESERVER_TIMEOUT_MS)) {
                log("wineserver -k did not finish within $WINESERVER_TIMEOUT_MS ms; SIGKILL to its process group, then the uid's others")
                killProcessGroup(process.pid)
                uidCleanup(except = engine?.pid)
            }
        } catch (error: IOException) {
            log("wineserver -k failed to start: ${error.message}")
        }
    }

    /**
     * Ends every other process of the app's user ID but [except] ([UidCleanup]: SIGTERM, then
     * SIGKILL after its grace).
     */
    private fun uidCleanup(except: Int? = null) {
        UidCleanup(
            list = { UidCleanup.scan(File("/proc"), Os.getuid(), Os.getpid(), ::owner).filter { it.pid != except } },
            signal = ::sendSignal,
            log = ::log,
            now = SystemClock::elapsedRealtime,
        ).run()
    }

    /**
     * The engine's `--remove-downloads` removes the installer downloads ([InstallerDownloads]);
     * refused while the game, an install or a repair runs (the engine also refuses while the
     * installation is in use). Waits at most [REMOVE_DOWNLOADS_TIMEOUT_MS].
     */
    private fun removeInstallerDownloads(): InstallerDownloads.Outcome {
        if (closing) {
            log("installer downloads not removed: the session is closing")
            return InstallerDownloads.Outcome.Failed("the session is closing")
        }
        InstallerDownloads.refusal(_status.value.snapshot)?.let {
            log("installer downloads not removed: $it")
            return InstallerDownloads.Outcome.Refused(it)
        }
        val enginePath = File(applicationInfo.nativeLibraryDir, ENGINE_LIBRARY)
        val loader = paths.loader.path
        val exited = CountDownLatch(1)
        var code: Int? = null
        val process = try {
            starter.start(
                "remove-downloads", loader, listOf(loader, enginePath.path, "--remove-downloads"),
                engineEnvironment(),
            ) { _, status ->
                code = status.code.takeIf { status.signal == null }
                exited.countDown()
            }
        } catch (error: IOException) {
            log("installer downloads not removed: ${error.message}")
            return InstallerDownloads.Outcome.Failed(error.message ?: error.toString())
        }
        if (!exited.await(REMOVE_DOWNLOADS_TIMEOUT_MS, TimeUnit.MILLISECONDS)) {
            log("--remove-downloads did not finish within $REMOVE_DOWNLOADS_TIMEOUT_MS ms; SIGKILL")
            sendSignal(process.pid, UidCleanup.SIGKILL)
        }
        return InstallerDownloads.outcome(code).also { log("installer downloads: $it") }
    }

    /**
     * At the game resolution's X screen size ([gameScreenSize]) unless [size] is given; [given]: the
     * size came from `--es xscreen` (debug builds), so Play and the official launcher keep it.
     */
    private fun startGameEnvironment(size: Pair<Int, Int>?, given: Boolean = size != null) {
        if (engine == null) {
            log("game environment not started: the engine is not running")
            return
        }
        if (closing) {
            log("game environment not started: the session is closing")
            return
        }
        // Every X screen: an even width, lines a multiple of 4 (a debug size too); stored and compared so.
        val (width, height) = GameResolution.xScreen(size ?: gameScreenSize(), ::log)
        try {
            game.start(width, height)
            if (xScreen == null) {
                xScreen = width to height
                xScreenGiven = given
            }
            _status.update { it.copy(gameEnvironment = true, gameEnvironmentFailure = null) }
        } catch (error: Exception) {
            _status.update { it.copy(gameEnvironment = false, gameEnvironmentFailure = error.message ?: error.toString()) }
            if (error is SystemDriverStartFailed) post { phoneDriverFailed("its server did not start: ${error.message}", strong = true) }
        }
    }

    /** The running game's route shows the card that asks if the game works: the first game on a new driver ([PhoneDriverFailure.asks]). */
    private fun routeAsks(renderer: Renderer): Boolean =
        PhoneDriverFailure.asks(engineRoute, RouteState.confirmed(this), forced = forcedRoute() != null, renderer = renderer)

    /**
     * The card's It works: the card goes, and the route is confirmed ([RouteState.confirm]) where
     * the game sent a frame ([PhoneDriverFailure.answerCounts]); else the card comes back in the
     * next game. Worker thread only.
     */
    private fun confirmRoute() {
        val route = engineRoute
        if (!_status.value.routeQuestion || route == null) return
        if (PhoneDriverFailure.answerCounts(presentsAtGameStart, PresentCounter.read(paths.presentCounter))) {
            log("the card: the player answered It works on route $route")
            RouteState.confirm(this, route)
        } else {
            log("the card: the player answered It works on route $route before the game's first frame: no record, the card comes back in the next game")
        }
        _status.update { it.copy(routeQuestion = false) }
    }

    /**
     * The card's Quit: the game stops as with the in-game menu's Stop, and the screens open
     * Settings → Display → GPU (the Game view's [com.pd2launcher.app.ui.GameView] `onRouteQuit`).
     * No driver changes, and the game's end counts nothing ([followPhoneDriverAtGameEnd]). Worker
     * thread only.
     */
    private fun quitRoute() {
        if (!_status.value.routeQuestion) return
        log("the card: the player answered Quit on route $engineRoute")
        gameQuit = true
        _status.update { it.copy(routeQuestion = false) }
        stopInGameView()
    }

    /** The driver of [route] for the log: the built-in one, RADV, mali-panvk, or the phone's own. */
    private fun driverName(route: GraphicsRoute?): String = when (route) {
        GraphicsRoute.BUILT_IN -> "the built-in driver"
        GraphicsRoute.RADV -> "RADV"
        GraphicsRoute.MALI_PANVK -> "mali-panvk"
        else -> "the phone's own graphics driver"
    }

    /** A session's first status: starting, with Play's error line of the last failure if no game started since ([RouteState.keepNotice]). */
    private fun startingStatus(): SessionStatus =
        SessionStatus.STARTING.copy(driverFailed = RouteState.notice(this, RouteState.APP_VERSION), softwareDriver = softwareRuns())

    /** The libhybris route's status file of this game environment ([SessionPaths.hybrisStatus]); null when there is none. */
    private fun readHybrisStatus(): String? = try {
        paths.hybrisStatus.takeIf { it.isFile }?.readText()
    } catch (_: IOException) {
        null
    }

    /**
     * The route's graphics driver failed ([PhoneDriverFailure]): Play says which driver had an
     * error, with a button to Settings → Display → GPU ([PhoneDriverFailure.tells]: on [strong]
     * evidence at once, the server gave up a game context or did not start, a route's failure watch
     * saw its line, the picture froze; else after [PhoneDriverFailure.NO_FRAME_GAMES_TO_TELL] games
     * in a row with no frame). The route and the pick stay: no failure changes the driver (the owner,
     * 2026-11-04). A debug build's forced route shows nothing.
     */
    private fun phoneDriverFailed(reason: String, strong: Boolean) {
        val forced = forcedRoute() != null
        val route = engineRoute
        val noFrameGames = if (!forced && !strong) RouteState.noFrameGame(this, RouteState.APP_VERSION) else 0
        val tells = route != null && PhoneDriverFailure.tells(forced, strong, noFrameGames)
        log(
            "${driverName(route)} failed on route ${route ?: "none"}: $reason; the route stays; " +
                (if (tells) "Play tells of the error" else if (forced) "forced by the debug file: Play shows nothing" else "Play shows nothing yet") +
                (if (noFrameGames > 0) "; games with no frame in a row: $noFrameGames of ${PhoneDriverFailure.NO_FRAME_GAMES_TO_TELL}" else ""),
        )
        if (!tells || route == null) return
        // The line says which driver; the server's own error text is not for the player. Kept until
        // the next game: a session that starts again shows it too ([startingStatus]).
        RouteState.keepNotice(this, route, RouteState.APP_VERSION)
        _status.update { it.copy(driverFailed = route, gameEnvironmentFailure = null) }
    }

    /**
     * Whether a launch is under way or something runs ([Play.launchBusy]): then Play and the
     * official PD2 launcher send nothing, and the game environment stays as it is (stopped or
     * started again at another size, it would take the running game or session with it).
     */
    private fun launchBusy(what: String): Boolean =
        // An owned install holds launches until its end; a launch only for its own time (Play.launchBusy).
        (pendingStart != null || installWork.sequence != null || Play.launchBusy(_status.value.snapshot, launchSent?.second, SystemClock.elapsedRealtime())).also { busy ->
            if (busy) log("$what not started: the game, a job or a launch already runs; the game environment stays as it is")
        }

    /**
     * Play's main button: with the Launcher's update checks on, the engine looks for a new Project
     * Diablo 2 version first ([PlayCheck]: Check again, then [followPlayCheck] starts the game or
     * asks); with them off, or no network, the game starts at once ([launch]).
     */
    private fun play() {
        // A change of the graphics driver first: Play's check runs on the engine that starts the game.
        if (launchBusy("Play") || driverChanges("Play")) return
        val current = _status.value.playCheck
        if (current != null && current !is PlayCheck.State.Starting) {
            log("Play not taken: its update check, question or update runs ($current)")
            return
        }
        val snapshot = _status.value.snapshot
        if (!PlayCheck.checks(snapshot)) {
            log("Play: game update checks are off; the game starts as installed")
            launch()
            return
        }
        if (!online()) {
            log("Play: Android has no network; the game starts without the update check")
            startGame(PlayCheck.Note.NO_NETWORK)
            return
        }
        if (BuildConfig.DEBUG) {
            val folder = GameFilesProvider.gameFolder(paths, snapshot?.gameFolder?.path)
            PlayCheck.debugNewVersion(File(paths.files, PlayCheck.DEBUG_MARKER), folder?.let(PlayCheck::record))?.let(::log)
        }
        // Checking before the send: every snapshot after it counts ([PlayCheck.observe]).
        val sentAt = SystemClock.elapsedRealtime()
        setPlayCheck(PlayCheck.State.Checking(null, sentAt, before = snapshot?.update))
        val seq = sendCommand(Command.CheckAgain)
        if (seq == null) {
            log("Play: the update check was not sent; the game starts without it")
            startGame(PlayCheck.Note.NO_ANSWER)
            return
        }
        log("Play: looking for a new Project Diablo 2 version first (Check again, seq $seq)")
        _status.update { status -> (status.playCheck as? PlayCheck.State.Checking)?.let { status.copy(playCheck = it.copy(seq = seq)) } ?: status }
        followLater(PlayCheck.WAIT_MS)
    }

    /** [followPlayCheck] again after [delayMs], also when no change of the session comes. */
    private fun followLater(delayMs: Long) {
        scope.launch {
            delay(delayMs)
            post { followPlayCheck() }
        }
    }

    /**
     * The game starts after Play's check ([note]: why without its answer, shown while it starts).
     * The note goes once the launch shows, fails or is refused ([launchRequested]), at the latest
     * after [Play.LAUNCH_PENDING_MS], so Play never stays held by it.
     */
    private fun startGame(note: PlayCheck.Note?) {
        val starting = PlayCheck.State.Starting(note)
        setPlayCheck(starting)
        launch()
        scope.launch {
            delay(Play.LAUNCH_PENDING_MS)
            _status.update { if (it.playCheck === starting) it.copy(playCheck = null) else it }
        }
    }

    /** Whether Android has a network with internet now; true when it cannot tell (the check's wait covers that). */
    private fun online(): Boolean = try {
        val manager = getSystemService(ConnectivityManager::class.java)
        manager == null || manager.getNetworkCapabilities(manager.activeNetwork)?.hasCapability(NetworkCapabilities.NET_CAPABILITY_INTERNET) == true
    } catch (error: Exception) {
        true
    }

    private fun setPlayCheck(state: PlayCheck.State?) {
        _status.update { it.copy(playCheck = state) }
        // A route change that waited for Play's check ([applyGraphicsRoute]).
        if (state == null && routeChangePending) post { if (routeChangePending) applyGraphicsRoute() }
    }

    /**
     * On the worker: Play's look for a new version after a change of the session ([PlayCheck]):
     * the game starts once the check says so (or cannot say), the question opens on a new
     * version, and the game starts after an update the question started, while the app is on the
     * screen (in the background the update only ends, and Play waits for the player).
     */
    private fun followPlayCheck() {
        val status = _status.value
        val now = SystemClock.elapsedRealtime()
        when (val state = status.playCheck) {
            is PlayCheck.State.Checking -> when (val outcome = PlayCheck.next(state, status, now)) {
                is PlayCheck.Outcome.Wait -> if (outcome.state != state) setPlayCheck(outcome.state)
                is PlayCheck.Outcome.Play -> if (appForeground) {
                    log("Play: ${outcome.note?.let { "no answer from the update check ($it)" } ?: "the update check found no new version"}; the game starts")
                    startGame(outcome.note)
                } else {
                    log("Play: the update check ended with the app in the background; the game does not start")
                    setPlayCheck(null)
                }
                PlayCheck.Outcome.Ask -> {
                    log("Play: the update check found a new Project Diablo 2 version; asking")
                    setPlayCheck(PlayCheck.State.Found)
                }
                PlayCheck.Outcome.Drop -> {
                    log("Play: the update check ended without a start (engine ${status.engine}, refusal ${status.refusal?.reason})")
                    setPlayCheck(null)
                }
            }
            PlayCheck.State.Found -> if (!PlayCheck.stillFound(status)) {
                log("Play: the new version's question closes: the session changed")
                setPlayCheck(null)
            }
            is PlayCheck.State.Updating -> when (val outcome = PlayCheck.afterUpdate(state, status, now)) {
                is PlayCheck.Outcome.Wait -> if (outcome.state != state) setPlayCheck(outcome.state)
                is PlayCheck.Outcome.Play -> if (appForeground) {
                    log("Play: the update succeeded; the game starts")
                    startGame(null)
                } else {
                    log("Play: the update succeeded with the app in the background; the game does not start")
                    setPlayCheck(null)
                }
                else -> {
                    log("Play: the update did not succeed; the game does not start")
                    setPlayCheck(null)
                }
            }
            is PlayCheck.State.Starting -> if (PlayCheck.startingEnds(status)) setPlayCheck(null)
            null -> Unit
        }
    }

    /** The question's answer ([PlayCheck.State.Found]): Update starts the engine's update, Cancel closes it. */
    private fun answerNewVersion(update: Boolean) {
        if (_status.value.playCheck != PlayCheck.State.Found) return
        if (!update) {
            log("Play: the new version's question was cancelled")
            setPlayCheck(null)
            return
        }
        beginOperation("Update", cancelled = { setPlayCheck(null) }) {
            val after = _status.value.snapshotSeq ?: -1
            val seq = sendCommand(Command.ApplyUpdate)
            log("Play: the new version's question: Update${seq?.let { " (seq $it); the game starts when it succeeds" } ?: " not sent"}")
            setPlayCheck(seq?.let { PlayCheck.State.Updating(it, SystemClock.elapsedRealtime(), afterSnapshot = after) })
            // An update that never shows is dropped after the check's wait, also with no other snapshot.
            if (seq != null) followLater(PlayCheck.WAIT_MS)
        }
    }

    /**
     * The game: the game environment (if not running yet, or again when the game resolution
     * changed its X screen's size), then `LaunchGame` with the X screen's size, the graphics driver,
     * and D2GL's graphics profile ([GraphicsProfiles.forPlay]) in the app's settings now; nothing
     * while a launch is under way or something runs ([launchBusy]).
     */
    private fun launch() {
        if (launchBusy("Play") || driverChanges("Play")) {
            // The launch under way keeps its time (or the engine starts again with the player's
            // graphics driver); only Play's note goes.
            _status.update { if (it.playCheck is PlayCheck.State.Starting) it.copy(playCheck = null) else it }
            return
        }
        // The driver's renderer for every Play (GlideFirst): Glide, or DirectDraw with GDI on the
        // Software driver; stored settings that differ go first, and Play waits for the engine.
        val now = SystemClock.elapsedRealtime()
        val software = engineRoute == GraphicsRoute.SOFTWARE
        // Debug builds: files/debug/renderer sets the renderer over the driver's (the matrix test); the pick stays.
        val debugRenderer = GlideFirst.debugRenderer(filesDir)
        when (GlideFirst.step(_status.value.snapshot?.settings, glideAskedAt, now, glideRefused, software, debugRenderer)) {
            GlideFirst.Step.SET -> {
                val commands = GlideFirst.commands(_status.value.snapshot?.settings, software, debugRenderer)
                val seqs = commands.mapNotNull { sendCommand(it) }
                if (seqs.size == commands.size) {
                    val wanted = when {
                        debugRenderer == GlideFirst.DebugRenderer.DDRAW -> "debug file's (${GlideFirst.DEBUG_FILE}: DirectDraw with OpenGL)"
                        debugRenderer == GlideFirst.DebugRenderer.GLIDE -> "debug file's (${GlideFirst.DEBUG_FILE}: Glide)"
                        software -> "Software driver's (DirectDraw with GDI)"
                        else -> "GPU driver's (Glide)"
                    }
                    log("Play: the stored renderer is not the $wanted; set it first (seq ${seqs.joinToString()}), the game starts once the engine has it")
                    glideAskedAt = now
                    glideSeqs = seqs
                    glideRefused = false
                    launchRequested(now)
                    scope.launch {
                        delay(GlideFirst.WAIT_MS)
                        post { glideAnswered() }
                    }
                    return
                }
                log("Play: the driver's renderer could not be sent; the game starts as it is")
            }
            GlideFirst.Step.WAIT -> return
            GlideFirst.Step.LAUNCH -> if (glideAskedAt != null) {
                val why = when {
                    GlideFirst.commands(_status.value.snapshot?.settings, software, debugRenderer).isEmpty() -> "the engine has the driver's renderer"
                    glideRefused -> "the engine refused the driver's renderer"
                    else -> "no answer for the driver's renderer in ${GlideFirst.WAIT_MS} ms"
                }
                log("Play: $why; the game starts")
            }
        }
        glideAskedAt = null
        glideSeqs = emptyList()
        glideRefused = false
        debugRenderer?.let { log("Play: a debug build's ${GlideFirst.DEBUG_FILE} sets the renderer: ${it.name.lowercase()} (the pick stays)") }
        _status.update { it.copy(driverFailed = null) }
        RouteState.clearNotice(this)
        // On the worker, as a Stop: a new game's end is a driver failure until a Stop says otherwise.
        gameStoppedByApp = false
        gameQuit = false
        // A new game: RADV's, mali-panvk's and the built-in driver's failure lines count from here (they can come before the game's start shows),
        // and the GDI fact goes until the engine's line of this game's start ([GdiGame]).
        gdiGame.reset()
        radvFailure.reset()
        maliPanvkFailure.reset()
        builtInFailure.reset()
        launchRequested(now)
        val settings = AppSettingsStore.load(this)
        if (game.serverLost()) {
            log("the system-driver server ended; restarting the game environment")
            stopGameEnvironment()
        }
        stopAtOtherSize()
        if (!game.running) startGameEnvironment(null)
        val (width, height) = xScreen?.takeIf { game.running } ?: run {
            log("game not launched: the game environment is not running")
            launchRequested(null)
            return
        }
        // No other sync from the Save's link to the command: one that waited sees the launch after it.
        SaveSyncFolder.locked {
            linkSaves()
            syncSaves("before Play", forLaunch = true)
            log("Play: settings: ${settingsLine(settings)}")
            log("Play: ${inputLine(settings)}")
            launchSent = sendCommand(
                Command.LaunchGame(
                    display = DisplaySize(width.toLong(), height.toLong()),
                    graphicsDriver = settings.graphicsDriver,
                    graphicsProfile = GraphicsProfiles.forPlay(settings.graphicsProfile, gpu),
                    strictMemoryOrder = false,
                    detailedLogging = settings.detailedLogging,
                ),
            )?.let { it to SystemClock.elapsedRealtime() }
            // The engine prepares the game's files once it has the command: the wait counts from the
            // send, and a command not sent holds nothing.
            launchRequested(launchSent?.second)
            // The game environment is this launch's now: a late end of an earlier official
            // launcher session must not stop it ([launcherSent]).
            if (launchSent != null) launcherSent = null
        }
    }

    /** When Play asked the engine for the driver's renderer first ([GlideFirst]); null: no such wait. Worker thread only. */
    private var glideAskedAt: Long? = null

    /** The seqs of those commands, and whether the engine refused one ([onCommandError]): then Play goes on at once. */
    private var glideSeqs: List<Long> = emptyList()
    private var glideRefused = false

    /**
     * Play's wait for the driver's renderer ([GlideFirst]): once the engine's snapshot has it, or
     * the wait is over, the launch goes on. Worker thread only.
     */
    private fun glideAnswered(settings: Settings? = _status.value.snapshot?.settings) {
        if (glideAskedAt == null) return
        val software = engineRoute == GraphicsRoute.SOFTWARE
        val debugRenderer = GlideFirst.debugRenderer(filesDir)
        if (GlideFirst.step(settings, glideAskedAt, SystemClock.elapsedRealtime(), glideRefused, software, debugRenderer) == GlideFirst.Step.WAIT) return
        // The launch that waited holds launchRequestedAt: free it for launch() itself.
        launchRequested(null)
        launch()
    }

    /**
     * The status's [SessionStatus.launchRequestedAt]: set from a launch request until its game or
     * session shows, it is refused, it is not sent or the engine ends.
     */
    private fun launchRequested(at: Long?) {
        // A launch that shows, is refused or is not sent ends Play's note.
        _status.update { it.copy(launchRequestedAt = at, playCheck = it.playCheck.takeUnless { at == null && it is PlayCheck.State.Starting }) }
    }

    /**
     * Before a Play or the official PD2 launcher: a running game environment whose X screen is not
     * the game resolution's ([gameScreenSize]) stops, so that it starts again at that size; one at
     * a size from `--es xscreen` (debug builds) stays.
     */
    private fun stopAtOtherSize() {
        if (!game.running || xScreenGiven) return
        val wanted = gameScreenSize()
        if (xScreen != wanted) {
            log("the game resolution wants a ${wanted.first} x ${wanted.second} X screen; restarting the game environment")
            stopGameEnvironment()
        }
    }

    /**
     * The official PD2 launcher: the game environment at the game resolution, as for Play (started
     * again when it runs at another size, [stopAtOtherSize]): a game the launcher's own Play starts
     * runs on the launcher's X screen, so it shows and runs as one from the app's Play. Then
     * `OpenOfficialLauncher`. A refusal stops the game environment started for it, as for an
     * install start. Nothing while a launch is under way or something runs ([launchBusy]). Each
     * press is a run of the official launcher's log ([beginLauncherRun]), which says how far it got.
     */
    private fun officialLauncher() {
        if (launchBusy("official launcher")) {
            // A run still open goes on; else the press is a short section of its own.
            if (launcherLog.running) {
                launcherLog.note("one more press, not taken: the game, a job or a launch already runs")
            } else {
                launcherLog.notTaken("the game, a job or a launch already runs")
            }
            return
        }
        if (driverChanges("official launcher")) {
            if (launcherLog.running) {
                launcherLog.note("one more press, not taken: the graphics driver changes first")
            } else {
                launcherLog.notTaken("the graphics driver changes first")
            }
            return
        }
        val settings = AppSettingsStore.load(this)
        beginLauncherRun(settings.detailedLogging)
        launchRequested(SystemClock.elapsedRealtime())
        if (game.serverLost()) {
            log("the system-driver server ended; restarting the game environment")
            stopGameEnvironment()
        }
        stopAtOtherSize()
        val started = !game.running
        if (started) startGameEnvironment(null)
        if (!game.running) {
            log("official launcher not opened: the game environment is not running")
            launcherLog.end("not opened: the game environment is not running (${_status.value.gameEnvironmentFailure ?: "no reason given"})")
            launchRequested(null)
            return
        }
        val (width, height) = GameResolution.display(this)
        val screen = xScreen
        // As Settings names the game resolution (GameResolution.label): Custom, the game's own size, or
        // the display's full size where the display does not hold the game's own size.
        val resolution = when {
            xScreenGiven -> "a debug size"
            GameResolution.allowedCustom(width, height, settings.customSize) != null -> "Custom"
            GameResolution.holdsOwnSize(width, height) -> "the game's own size"
            else -> "the display's full size"
        }
        launcherLog.note(
            "X screen ${screen?.let { (w, h) -> "$w x $h" } ?: "of an unknown size"} on a $width x $height display " +
                "(Game resolution: $resolution); " +
                "game environment ${if (started) "started for it" else "already running"}",
        )
        // What the engine showed before the send: a session that starts and ends between two
        // snapshots shows only by its end ([launcherSent]).
        val sentAfter = _status.value.snapshotSeq ?: -1
        val before = _status.value.snapshot?.operation
        // Its Play starts the game: the characters as for the app's own Play, with no other sync
        // from the Save's link to the command (one that waited sees the launch after it).
        val seq = SaveSyncFolder.locked {
            linkSaves()
            syncSaves("before the official launcher", forLaunch = true)
            log("official launcher: settings: ${settingsLine(settings)}")
            log("official launcher: ${inputLine(settings)}")
            launcherLog.note("settings: ${settingsLine(settings)}")
            sendCommand(Command.OpenOfficialLauncher(strictMemoryOrder = false, detailedLogging = settings.detailedLogging))
                ?.also { launchRequested(SystemClock.elapsedRealtime()) }
        }
        if (seq == null) {
            launcherLog.end("not sent to the engine (service.log says why)")
        } else {
            launcherLog.sent(seq)
            // A session that never shows ends the run all the same, also when the engine sends no other snapshot.
            scope.launch {
                delay(OfficialLauncherLog.NO_SESSION_MS)
                launcherLog.noSession(seq, _status.value.snapshot)
            }
        }
        if (seq == null && started) {
            log("official launcher not sent; stopping the game environment started for it")
            stopGameEnvironment()
            launchRequested(null)
            return
        }
        installStart = seq?.takeIf { started }
        launcherSent = seq?.let { LauncherSent(it, sentAfter, before) }
        launchSent = seq?.let { it to SystemClock.elapsedRealtime() }
        launchRequested(launchSent?.second)
    }

    /**
     * A run of the official PD2 launcher's log ([OfficialLauncherLog.begin]) for a press of its
     * button: the app, the installation, the game folder and the x86 .NET runtime, and the
     * launcher's files in the game folder's ProjectD2; with [detailed] logging, all its engine lines.
     */
    private fun beginLauncherRun(detailed: Boolean) {
        val snapshot = _status.value.snapshot
        val project = GameFilesProvider.gameFolder(paths, snapshot?.gameFolder?.path)?.let { File(it, LauncherFiles.FOLDER) }
        launcherLog.begin(
            listOf(
                "app ${appVersion()}; installation ${snapshot?.installation ?: "unknown"}, update ${snapshot?.update ?: "unknown"}",
                "game folder: ${project?.parentFile?.let { if (it == paths.managedGame) "the app's own" else "a folder copy" } ?: "none"}",
                "x86 .NET: ${LauncherFiles.dotnet(paths.managedRoot)}",
            ),
            project,
            snapshot?.operation,
            snapshot?.result,
            detailed,
        )
    }

    /**
     * Start over takes the Managed prefix and forgets a folder copy used in place: before it, the
     * characters of each game folder's Save move to the player's own folder ([SaveKeeper.keepAll]);
     * the prefix's Save becomes a link, so Start over removes only the link, and a copy's goes
     * without one, so the copy can be imported again (the launch links it). False (the Start over
     * is not sent, and the player sees why) when that failed. Skipped while the game or a job
     * runs, when the engine refuses Start over anyway.
     */
    private fun keepSaves(): Boolean {
        val snapshot = _status.value.snapshot
        if (snapshot?.activeJob != null || snapshot?.game == Game.RUNNING) return true
        return try {
            // Not during a save sync: both write the player's Save.
            SaveSyncFolder.locked { SaveKeeper.keepAll(SessionPaths(DeviceCheck.dataDir(this)), log = ::log) }
            true
        } catch (error: Exception) {
            log("Start over not sent: the saves could not be moved to the player's folder: $error")
            _status.update { it.copy(refusal = Refusal(-1, Refusals.SAVES_NOT_KEPT)) }
            false
        }
    }

    /**
     * Before a launch (Play, the official PD2 launcher) the game folder's Save links to the
     * player's own folder ([SaveKeeper.link]), so the game reads and writes the characters there;
     * only for an installed game nothing runs in. Only a ready installation is linked: a folder copy
     * still being installed must stay free of links, as the engine refuses a copy that holds one
     * when it checks it again. The game folder is the one the engine names, else the Launcher's
     * record: the engine names a folder an install starts to use in place only from its next start.
     */
    private fun linkSaves() {
        val paths = SessionPaths(DeviceCheck.dataDir(this))
        val snapshot = _status.value.snapshot ?: return
        if (snapshot.installation != Installation.READY) return
        val folder = snapshot.gameFolder
        if (folder?.available == false || snapshot.activeJob != null || snapshot.game == Game.RUNNING) return
        val save = SaveKeeper.liveSave(paths, folder?.path ?: GameFilesProvider.recordedGameFolder(paths))
        if (save.parentFile?.isDirectory != true) return
        try {
            SaveKeeper.link(save, paths.userSave, paths.saveBackups, log = ::log)
        } catch (error: Exception) {
            // Nothing is lost: what was not moved is still in the game's Save; the next launch tries again.
            log("Save not linked to the player's folder: $error")
        }
    }

    /**
     * The save sync ([SaveSyncFolder.run]) when the setting is on, named [why] in the log:
     * before a launch ([forLaunch], on the worker, after [linkSaves] and before the launch is
     * sent, whatever the folder's state: the launch goes on), else only while the game, a launch,
     * the official PD2 launcher (and a game it started), an install and every job are not running,
     * as seen once the sync has its turn: else Sync now's line says so. One sync at a time.
     */
    private fun syncSaves(why: String, forLaunch: Boolean = false) {
        if (AppSettingsStore.load(this).syncFolder == null) return
        val busy = {
            val status = _status.value
            // A launch counts from its request until its game or session shows, it fails or it
            // is refused (launchRequested), or at most SYNC_LAUNCH_WAIT_MS.
            val launching = status.launchRequestedAt?.let { SystemClock.elapsedRealtime() - it < SYNC_LAUNCH_WAIT_MS } == true
            status.snapshot?.game == Game.RUNNING || status.snapshot?.activeJob != null || status.launcherGame ||
                installEnvironment || launching
        }
        SaveSyncFolder.run(this, paths, why, AppSettingsStore.load(this).detailedLogging, busy.takeUnless { forLaunch }, ::log)
    }

    /** [syncSaves] on the sync's own thread: after the game or the official launcher, and from Settings. */
    private fun syncSavesSoon(why: String) {
        try {
            syncWorker.execute { syncSaves(why) }
        } catch (_: RejectedExecutionException) {
        }
    }

    /** [work] on the sync's own thread, after what waits there (one at a time: the files it reads stay still). */
    private fun onSyncWorker(work: () -> Unit) {
        try {
            syncWorker.execute(work)
        } catch (_: RejectedExecutionException) {
        }
    }

    /**
     * What a new prefix needs back ([PrefixState]), kept as the installation stands Ready with
     * nothing running ([PrefixStateKeeper]), and again once the game environment has stopped when
     * Wine's run put it off ([keepPutOffPrefixState]).
     */
    private fun keepPrefixState() = SaveSyncFolder.locked {
        // Under the launches' lock: a launch holds it from the Save's link to its command, so no
        // Wine program starts while the installers' files move. Then the prefix lock inside it
        // (always in this order): a rebuild at a session start holds that one alone
        // (AppRuntime.updatePrefixAtStart, AppRuntime.install), so a capture that waits for it
        // never meets a rebuild.
        PrefixState.locked { prefixStateKeeper.keep() }
    }

    /**
     * A capture of the prefix state that Wine's run put off ([PrefixStateKeeper.takePutOff]) runs
     * now, once, on the sync thread under the same locks: the game environment has stopped. (The
     * Ready snapshot after an install can come before its game environment's stop.)
     */
    private fun keepPutOffPrefixState() {
        if (!prefixStateKeeper.takePutOff()) return
        log("prefix state: the capture that Wine's run put off runs now")
        onSyncWorker { keepPrefixState() }
    }

    /**
     * Starts a job ([dispatch]) for the press or step [what]: at once, or, while the snapshot still
     * shows the last operation's end, once the engine has cleared it (`DismissResult`,
     * [OperationStart]; protocol 5 has no operation ID, so an old result could pass for the new
     * job's). Nothing starts while the session closes, another start waits, the engine is not
     * connected, or the game or a job runs, nor when the engine does not clear its last result in
     * [PlayCheck.WAIT_MS]: then [cancelled] runs, the log says why and, with [shown], the screens
     * too ([Refusal] -1 with the engine's code for it, [Refusals.text]). [joinsDownload]: keys
     * that join a running Download (the engine takes `authorize` while it runs), which is then no
     * job that holds the start back.
     */
    private fun beginOperation(
        what: String, shown: Boolean = true, cancelled: () -> Unit = {}, joinsDownload: Boolean = false, dispatch: () -> Unit,
    ): Boolean {
        val refused = { why: String, reason: String? ->
            log("$what not started: $why")
            if (shown && reason != null) _status.update { it.copy(refusal = Refusal(-1, reason)) }
            cancelled()
            false
        }
        if (closing) return refused("the session is closing", null)
        if (pendingStart != null) return refused("another start waits for the engine to clear its last result", REFUSED_ENGINE_BUSY)
        val connection = channel ?: return refused("the engine is not connected", REFUSED_ENGINE_GONE)
        val status = _status.value
        val job = status.snapshot?.activeJob
        if ((job != null && !(joinsDownload && job == com.pd2launcher.app.control.Job.DOWNLOAD)) || status.snapshot?.game == Game.RUNNING) {
            return refused("the game or a job runs", REFUSED_BUSY)
        }
        if (!OperationStart.terminal(status.snapshot)) {
            dispatch()
            return true
        }
        val seq = sendCommand(Command.DismissResult) ?: return refused("the last result could not be cleared (DismissResult not sent)", REFUSED_ENGINE_GONE)
        val pending = OperationStart(connection, status.snapshotSeq ?: -1, seq, SystemClock.elapsedRealtime(), dispatch) {
            // No clear in PlayCheck.WAIT_MS, its DismissResult refused, or the engine stopped.
            refused("the engine did not clear its last result (DismissResult seq $seq)", REFUSED_ENGINE_BUSY.takeUnless { closing })
        }
        pendingStart = pending
        scope.launch {
            delay(PlayCheck.WAIT_MS)
            post {
                if (pendingStart === pending) {
                    pendingStart = null
                    pending.cancel()
                }
            }
        }
        return true
    }

    private fun cancelOperationStart() {
        val pending = pendingStart
        pendingStart = null
        pending?.cancel()
    }

    /**
     * Install runs Wine with the game environment's X screen and sound: the game environment
     * first (installs keep the game's own size, whatever the game resolution; a Play at another
     * size starts it again), then [send]; nothing is sent without it. The game environment then
     * runs for an install ([installEnvironment]: this one, or the Diablo II install that Install
     * Project Diablo 2 continues), also when it ran before with nothing in it (a launch that failed
     * leaves it), and a command that could not be sent, or one the engine refuses, stops it again:
     * nothing would run in it. [shown]: a press of the player's, whose refusal the screens show
     * ([beginOperation]).
     */
    private fun sendInstallStart(
        job: com.pd2launcher.app.control.Job,
        imported: Boolean = false,
        shown: Boolean = true,
        cancelled: () -> Unit = {},
        joinsDownload: Boolean = false,
        send: () -> Long?,
    ): Boolean = beginOperation("install ($job)", shown, cancelled, joinsDownload) {
        dispatchInstall(job, imported, cancelled, send)
    }

    private fun dispatchInstall(job: com.pd2launcher.app.control.Job, imported: Boolean, cancelled: () -> Unit, send: () -> Long?) {
        val sent = installWork.start(
            job, imported, _status.value.snapshotSeq ?: -1,
            prepare = {
                if (!game.running) {
                    startGameEnvironment(GameResolution.display(this).let { (w, h) -> GameResolution.ownSize(w, h, ::log) }, given = false)
                }
                if (!game.running) log("install not sent: the game environment is not running")
                game.running.also { if (it) installEnvironment = true }
            },
            send = send,
            stop = {
                log("install not sent; stopping the game environment started for it")
                stopGameEnvironment()
            },
        )
        if (sent) installStart = installWork.sequence else cancelled()
    }

    /**
     * Install Project Diablo 2: the player's press ([press]), or the step after Install Diablo II.
     * When it does not start, the game environment that Install Diablo II left for it stops (none
     * other runs in it), so the session can become idle.
     */
    private fun installProjectDiablo(press: Boolean) {
        sendInstallStart(com.pd2launcher.app.control.Job.INSTALL_PROJECT_DIABLO, shown = press, cancelled = {
            val snapshot = _status.value.snapshot
            if (installWork.sequence == null && installEnvironment && game.running &&
                snapshot?.activeJob == null && snapshot?.game != Game.RUNNING
            ) {
                log("Install Project Diablo 2 did not start; stopping the game environment left for it")
                stopGameEnvironment()
            }
        }) { sendCommand(Command.InstallProjectDiablo) }
    }

    /**
     * The game's sound follows the app's visibility and the Launcher's "Sound in background"
     * setting; it holds audio focus only while the Game client runs.
     */
    private fun applyBackgroundSound() {
        val snapshot = _status.value.snapshot
        val setting = snapshot?.settings?.soundInBackground ?: true
        game.setBackground(!appForeground, setting, gameRunning = snapshot?.game == Game.RUNNING || _status.value.launcherGame)
    }

    /**
     * While the official PD2 launcher's session runs ([on]), every [LauncherGame.POLL_MS] whether
     * it has started the game ([SessionStatus.launcherGame]); a session that ended clears it. The
     * same looks go to the launcher's run ([OfficialLauncherLog.processes]), and every second one,
     * while its game does not run, whether the X screen shows a picture ([OfficialLauncherLog.screen]).
     * While the launcher runs and none of its other programs ([LauncherHang.watched]), also the
     * windows on the X screen ([XWindows]): a launcher that runs on without one, or beside its
     * game, stops ([LauncherHang], [launcherStop]). Worker thread only; the looking runs in [scope]
     * and reads only the app's own processes and its own X screen.
     */
    private fun watchLauncherGame(on: Boolean) {
        if (!on) {
            launcherGameWatch?.cancel()
            launcherGameWatch = null
            // Also the sound's game state, which the next session's audio starts from.
            launcherGameSeen(false)
            return
        }
        if (launcherGameWatch != null) return
        launcherGameWatch = scope.launch {
            val watch = coroutineContext.job
            var looks = 0
            // The X screen's pixels, mapped once for each framebuffer (a mapping shows every later frame).
            var screen: Pair<Framebuffer, ScreenPixels>? = null
            var hang = LauncherHang.Watch()
            var windowShown: Boolean? = null
            while (isActive) {
                val processes = UidCleanup.scan(File("/proc"), Os.getuid(), Os.getpid(), ::owner)
                val running = LauncherGame.running(processes)
                // Taken on the worker, and only from the watch still running there: a session
                // that ended stays cleared.
                if (running != _status.value.launcherGame) post { if (launcherGameWatch === watch) launcherGameSeen(running) }
                launcherLog.processes(processes.filterNot { OwnedProcesses.isRunning(it.pid) })
                // The launcher's and its game's environment, once for each.
                programEnvironments.next(processes).forEach { line -> post { log(line) } }
                if (!running && looks++ % 2 == 0 && launcherLog.running) {
                    val framebuffer = GameEnvironment.framebuffer.value
                    val pixels = framebuffer?.let { current ->
                        screen?.takeIf { it.first === current }?.second ?: current.map()?.also { screen = current to it }
                    }
                    if (pixels != null) launcherLog.screen(OfficialLauncherLog.picture(pixels.header.width, pixels.header.height, pixels::pixel))
                }
                // Only while the launcher runs alone: the windows of its game or updater are not its own.
                val windows = if (LauncherHang.watched(processes)) withContext(Dispatchers.IO) { XWindows.showing(GameEnvironment.DISPLAY) } else null
                val shown = windows?.isNotEmpty()
                if (shown != null && shown != windowShown) {
                    windowShown = shown
                    val text = if (shown) "a window shows on the X screen" else "no window shows on the X screen"
                    launcherLog.note(text)
                    post { log("official PD2 launcher session: $text") }
                }
                val step = LauncherHang.next(hang, processes, windows, SystemClock.elapsedRealtime())
                hang = step.watch
                step.stop?.let { stop -> post { if (launcherGameWatch === watch) launcherStop(stop, step.watch.launchers) } }
                delay(LauncherGame.POLL_MS)
            }
        }
    }

    /**
     * The official PD2 launcher runs on when the player is done with it ([LauncherHang]): without a
     * window (its X), its session stops as Close launcher stops it, through the engine's safe stop;
     * beside its game, only PD2Launcher.exe ends ([endLauncher]) and the game runs on. Only while
     * the engine still runs the launcher's job: not again when a stop is already under way.
     * Worker thread only.
     */
    private fun launcherStop(stop: LauncherHang.Stop, launchers: Set<Int>) {
        val snapshot = _status.value.snapshot
        if (snapshot?.activeJob != com.pd2launcher.app.control.Job.OFFICIAL_LAUNCHER || snapshot.operation?.status != OperationStatus.RUNNING) return
        val pids = launchers.joinToString()
        when (stop.why) {
            LauncherHang.Why.NO_WINDOW -> {
                val why = "no window for ${stop.afterMs} ms while PD2Launcher.exe (pid $pids) runs on and none of its other programs runs"
                log("official PD2 launcher stops (its X): $why; safe stop, as Close launcher")
                launcherLog.note("stop (its X): $why: the engine is asked to stop the session safely, as Close launcher does")
                sendCommand(Command.RequestSafeStop)
            }
            LauncherHang.Why.GAME -> {
                if (launcherEnd?.isActive == true) return
                val why = "its game Game.exe has run for ${stop.afterMs} ms beside PD2Launcher.exe (pid $pids)"
                log("official PD2 launcher stops (game started): $why; only PD2Launcher.exe ends, the game runs on")
                launcherLog.note("stop (game started): $why: only PD2Launcher.exe ends, the game runs on")
                launcherEnd = scope.launch(Dispatchers.IO) { endLauncher(launchers) }
            }
        }
    }

    /**
     * Ends the PD2Launcher.exe processes [launchers] and no other process ([UidCleanup]): SIGTERM,
     * and SIGKILL after [LauncherHang.END_GRACE_MS] to one that still runs. Wine's server sees the
     * end as any end of one of its programs, and the engine's session waits for the rest of the
     * prefix (the game) as after any end of the launcher. Off the worker.
     */
    private fun endLauncher(launchers: Set<Int>) {
        val start = SystemClock.elapsedRealtime()
        val targets = { otherProcesses().filter { it.pid in launchers && it.name.equals(LauncherHang.PROCESS, ignoreCase = true) } }
        UidCleanup(targets, ::sendSignal, { line -> log("official PD2 launcher: $line") }).run(LauncherHang.END_GRACE_MS)
        val left = targets()
        val text = if (left.isEmpty()) {
            "PD2Launcher.exe ended ${SystemClock.elapsedRealtime() - start} ms after the stop"
        } else {
            "PD2Launcher.exe (pid ${left.joinToString { it.pid.toString() }}) still runs after SIGKILL"
        }
        log("official PD2 launcher: $text")
        launcherLog.note(text)
    }

    /** The app's other processes now: those of its user ID that the session service did not start itself. */
    private fun otherProcesses(): List<UidProcess> =
        UidCleanup.scan(File("/proc"), Os.getuid(), Os.getpid(), ::owner).filterNot { OwnedProcesses.isRunning(it.pid) }

    /** The official PD2 launcher's game [running] or not ([watchLauncherGame]). Worker thread only. */
    private fun launcherGameSeen(running: Boolean) {
        if (running == _status.value.launcherGame) return
        _status.update { it.copy(launcherGame = running) }
        log(if (running) "official PD2 launcher: its game runs" else "official PD2 launcher: its game ended")
        applyBackgroundSound()
        followMemory()
    }

    /**
     * Stop in the Game view: the official PD2 launcher's session stops through the engine, which
     * closes it and its Wine processes safely (the game environment follows once the session has
     * ended); the Game client goes with the game environment.
     */
    private fun stopInGameView() {
        if (_status.value.snapshot?.activeJob == com.pd2launcher.app.control.Job.OFFICIAL_LAUNCHER) {
            log("official PD2 launcher: safe stop requested")
            launcherLog.note("Close launcher: the engine is asked to stop the session safely")
            sendCommand(Command.RequestSafeStop)
        } else {
            gameStoppedByApp = true
            stopGameEnvironment()
        }
    }

    private fun stopGameEnvironment() {
        // Wine's processes (wineserver, the wine64-preloaders under FEX) outlive Xvfb otherwise.
        wineserverKill()
        game.stop()
        xScreen = null
        xScreenGiven = false
        installEnvironment = false
        installStart = null
        installWork.clear()
        _status.update { it.copy(gameEnvironment = false) }
        log("game environment stopped")
        keepPutOffPrefixState()
    }

    /**
     * The notification's Close, on the worker: what runs stops as the player would stop it
     * ([BackgroundWork.stop]): the game as Stop game in its menu, the official PD2 launcher as
     * Close launcher, an engine job as Stop (an install pauses; Resume continues it later with the
     * keys held in memory, [HeldKeys]), and an app update's download as its Stop. Nothing new starts
     * meanwhile ([closing]), and the notification says that the app closes. A folder copy for an
     * Import stops, and what it copied goes ([SourceImport.cancel]): the player imports again
     * later. Once the stop and the copy's end have finished, or their waits
     * ([BackgroundWork.waitMs], [BackgroundWork.COPY_STOP_MS]) have ended, [finishClose] ends the
     * session.
     */
    private fun close() {
        if (closeStarted || stopping) return
        closeStarted = true
        cancelOperationStart()
        closing = true
        val now = SystemClock.elapsedRealtime()
        val stop = BackgroundWork.stop(_status.value, now)
        val wait = BackgroundWork.waitMs(stop)
        log("Close in the notification: what runs: $stop")
        // Awake for the waits: with the screen off they would not end until the device wakes.
        closeWakeLock = getSystemService(PowerManager::class.java).newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "pd2:close").also {
            it.acquire(wait + BackgroundWork.COPY_STOP_MS + ENGINE_STOP_TIMEOUT_MS + WINESERVER_TIMEOUT_MS + CLOSE_WAKE_MARGIN_MS)
        }
        followForeground()
        if (AppUpdater.work.value is AppUpdate.Work.Downloading) {
            log("Close: the app update's download stops; Try again continues it")
            AppUpdater.stop()
        }
        // Also when none runs: none starts from here on, until the next session start.
        val copying = SourceImport.cancel(::log)
        if (copying) log("Close: the folder copy for the Import stops, and what it copied goes")
        when (stop) {
            BackgroundWork.Stop.GAME -> {
                log("Close: the game stops, as Stop game")
                stopInGameView()
            }
            BackgroundWork.Stop.OFFICIAL_LAUNCHER -> {
                log("Close: the official PD2 launcher stops safely, as Close launcher")
                launcherLog.note("Close in the notification: the engine is asked to stop the session safely")
                sendCommand(Command.RequestSafeStop)
            }
            BackgroundWork.Stop.JOB -> {
                log("Close: the job stops at a safe point, as Stop; Resume continues it later")
                sendCommand(Command.RequestSafeStop)
            }
            BackgroundWork.Stop.NOTHING -> {}
        }
        scope.launch {
            val stopped = BackgroundWork.stopped(stop, _status.value, SystemClock.elapsedRealtime()) ||
                withTimeoutOrNull(wait) { _status.first { BackgroundWork.stopped(stop, it, SystemClock.elapsedRealtime()) } } != null
            // The copy ends at its next block, then removes what it copied.
            val copyEnded = !copying ||
                withTimeoutOrNull(BackgroundWork.COPY_STOP_MS) { SourceImport.copy.first { it !is SourceCopy.Copying } } != null
            post {
                if (copying) {
                    log(
                        when {
                            !copyEnded ->
                                "Close: the folder copy did not end within ${BackgroundWork.COPY_STOP_MS} ms; it stops at its next " +
                                    "block, or the next session start removes what it copied"
                            SourceImport.copy.value is SourceCopy.Copied -> "Close: the folder copy had finished before Close; it stays"
                            else -> "Close: the folder copy ended (${SourceImport.copy.value})"
                        },
                    )
                }
                finishClose(stop, stopped, wait)
            }
        }
    }

    /**
     * The end of [close], on the worker: the game environment and the engine stop (the engine
     * closes its Control channel, stops safely and exits, or gets SIGKILL after
     * [ENGINE_STOP_TIMEOUT_MS]), then the service stops (its notification goes with it), and the
     * app's screens close with their task ([finishAppTasks]). Nothing starts the session again by
     * itself ([START_NOT_STICKY]); the next start of the app starts it afresh.
     */
    private fun finishClose(stop: BackgroundWork.Stop, stopped: Boolean, waitMs: Long) {
        if (stopping) return
        if (stopped) log("Close: $stop stopped") else log("Close: $stop did not stop within $waitMs ms; the session ends anyway")
        if (game.running) stopGameEnvironment()
        if (engine != null) {
            stopEngine()
            log("Close: the engine stopped")
        }
        log("Close: the service stops and the app's screens close")
        stopAsked = true
        stopSelf()
        finishAppTasks()
        closeWakeLock?.let { if (it.isHeld) it.release() }
        closeWakeLock = null
    }

    /**
     * Every task of the app ends, with its activities, and leaves the recent apps: the app is
     * closed. A task already gone is skipped.
     */
    private fun finishAppTasks() {
        val tasks = try {
            getSystemService(ActivityManager::class.java).appTasks
        } catch (error: Exception) {
            log("Close: the app's tasks: $error")
            return
        }
        for (task in tasks) {
            try {
                task.finishAndRemoveTask()
            } catch (error: Exception) {
                log("Close: a task of the app did not end: $error")
            }
        }
    }

    /**
     * On the worker: the service is in the foreground, with its notification, while something must
     * go on in the background ([BackgroundWork.keeps]) or Close runs, and leaves it (the
     * notification goes) once nothing does; then the session ends a short time after the app's
     * screens went away ([SessionIdle]). The notification follows Close's progress.
     */
    private fun followForeground() {
        if (stopping || stopAsked) return
        val wanted = closing || BackgroundWork.keeps(
            _status.value, installEnvironment, SourceImport.copy.value is SourceCopy.Copying,
            BackgroundWork.appUpdating(AppUpdater.work.value), SystemClock.elapsedRealtime(),
        )
        if (wanted && (!inForeground || notificationClosing != closing)) {
            startForeground(NOTIFICATION_ID, notification(closing))
            if (!inForeground) log("in the foreground with the notification: something must go on in the background")
            inForeground = true
            notificationClosing = closing
        } else if (!wanted && inForeground) {
            stopForeground(STOP_FOREGROUND_REMOVE)
            inForeground = false
            log("out of the foreground, no notification: nothing must go on in the background")
        }
    }

    /**
     * The game ended [ranMs] after it started ([EarlyGameEnd]): the app's other processes, each
     * with the binary it runs.
     */
    private fun logEarlyGameEnd(ranMs: Long) {
        val processes = UidCleanup.scan(File("/proc"), Os.getuid(), Os.getpid(), ::owner).map { process ->
            val exe = try {
                Os.readlink("/proc/${process.pid}/exe")
            } catch (_: ErrnoException) {
                "?"
            }
            "${process.pid} ${process.name} ($exe)"
        }
        log(
            "the game ended $ranMs ms after it started; the app's processes: " +
                processes.ifEmpty { listOf("none") }.joinToString("; "),
        )
    }

    /**
     * On the worker: the line [memoryWatch] gives for now, if any, and while the game runs the
     * environment line of each Wine program not seen before ([programEnvironments]). The game runs
     * while the Game client or the official PD2 launcher's game runs; while neither runs and none
     * ran at the last look, nothing is read.
     */
    private fun followMemory() {
        val status = _status.value
        val running = status.snapshot?.game == Game.RUNNING || status.launcherGame
        if (!running && !memoryWatch.running) return
        memoryWatch.next(running, MemoryLog.read(), SystemClock.elapsedRealtime())?.let(::log)
        // The game's environment, once for each Wine program.
        if (running) programEnvironments.next(UidCleanup.scan(File("/proc"), Os.getuid(), Os.getpid(), ::owner)).forEach(::log)
    }

    /**
     * How the app's earlier processes ended ([ActivityManager.getHistoricalProcessExitReasons]),
     * each once: the ones whose keys the last look did not have; the keys of the list now are kept
     * for the next look ([MemoryLog.SEEN_EXITS]). A kill for low memory shows as LOW_MEMORY.
     */
    private fun logProcessExits() {
        try {
            val exits = getSystemService(ActivityManager::class.java).getHistoricalProcessExitReasons(packageName, 0, 0).map {
                MemoryLog.Exit(it.timestamp, it.pid, it.processName, it.reason, it.status, it.importance, it.pss, it.rss, it.description)
            }
            val preferences = getSharedPreferences(MemoryLog.PREFERENCES, Context.MODE_PRIVATE)
            val seen = preferences.getStringSet(MemoryLog.SEEN_EXITS, null).orEmpty()
            val ended = MemoryLog.unseen(exits, seen)
            if (ended.isEmpty()) log("no earlier app process ended since the last look")
            ended.forEach { log(MemoryLog.describe(it)) }
            val keys = MemoryLog.keys(exits)
            if (keys != seen) preferences.edit().putStringSet(MemoryLog.SEEN_EXITS, keys).apply()
        } catch (error: Exception) {
            log("earlier app processes: $error")
        }
    }

    /** The game's X screen size: the game resolution in the app's settings ([GameResolution.forPlay]). */
    private fun gameScreenSize(): Pair<Int, Int> {
        val (width, height) = GameResolution.display(this)
        return GameResolution.forPlay(width, height, AppSettingsStore.load(this).customSize, ::log)
    }

    /**
     * The game environment's X display and gamepad file, for the guests the engine starts (their
     * sound needs nothing here: Wine finds its ALSA configuration in `XDG_RUNTIME_DIR`).
     */
    private fun gameSessionEnvironment(): List<String> = listOf(
        "DISPLAY=${GameEnvironment.DISPLAY}",
        "PD2_GAMEPAD_FILE=${GameEnvironment.gamepadFile(paths).path}",
    )

    private fun owner(file: File): Int? = try {
        Os.stat(file.path).st_uid
    } catch (_: ErrnoException) {
        null
    }

    /**
     * SIGKILL to the process group of [pid]: every program the service starts leads a session of
     * its own (the spawner's setsid), so this reaches what it started that stayed in its group.
     */
    private fun killProcessGroup(pid: Int) {
        try {
            Os.kill(-pid, OsConstants.SIGKILL)
        } catch (error: ErrnoException) {
            log("SIGKILL to process group $pid: ${error.message}")
        }
    }

    /**
     * Debug builds only: the seccomp test harness that every program starts under while
     * [SessionPaths.seccompHarnessForTest] exists ([ProcessStarter]), so a test runs the whole
     * session, started from the app as usual, under Android 11's app seccomp policy on a device
     * whose Android puts no filter on apps. A blocked call gets ENOSYS and a line in the program's
     * log and in logcat. Without the harness program the start fails: a test that asks for the
     * harness must not pass without it.
     */
    private fun seccompHarness(): File? {
        if (!BuildConfig.DEBUG || !paths.seccompHarnessForTest.exists()) return null
        if (!paths.seccompRun.canExecute()) {
            throw IOException("${paths.seccompHarnessForTest.name} is there, but no seccomp test harness at ${paths.seccompRun}")
        }
        return paths.seccompRun
    }

    /** Whether [process] ended within [timeoutMs]. */
    private fun awaitExit(process: OwnedProcess, timeoutMs: Long): Boolean {
        val deadline = SystemClock.elapsedRealtime() + timeoutMs
        while (OwnedProcesses.isRunning(process.pid) && SystemClock.elapsedRealtime() < deadline) Thread.sleep(50)
        return !OwnedProcesses.isRunning(process.pid)
    }

    private fun summary(message: EngineMessage.SnapshotMessage): String {
        val s = message.snapshot
        val operation = s.operation?.let { " operation=${it.phase}/${it.status} failure=${it.failure}" } ?: ""
        return "host=${s.host} missing=${s.missing} installation=${s.installation} " +
            "update=${s.update} step=${s.step} job=${s.activeJob}$operation"
    }

    private fun appVersion(): String = packageManager.getPackageInfo(packageName, 0).versionName ?: "unknown"

    /** [SettingsLog.line] for [settings], the controls and the engine's settings in the last snapshot. */
    private fun settingsLine(settings: AppSettings): String = SettingsLog.line(
        settings, ControlsStore.load(this),
        LauncherSettings.view(_status.value.snapshot?.settings, _status.value.engine == SessionStatus.Engine.CONNECTED),
    )

    /** [InputSources.line] for the input devices there are now and [settings]' Touch and the chord. */
    private fun inputLine(settings: AppSettings): String {
        val devices = ArrayList<InputFacts>()
        for (id in InputDevice.getDeviceIds()) InputDevice.getDevice(id)?.let { devices += InputFacts.of(it) }
        return InputSources.line(devices, settings.touchMode, ControlsStore.load(this).chord)
    }

    private fun log(message: String) {
        Log.i(TAG, message)
        serviceLog.line(message)
    }

    private fun createChannel() {
        val manager = getSystemService(NotificationManager::class.java)
        if (manager.getNotificationChannel(CHANNEL) == null) {
            manager.createNotificationChannel(
                NotificationChannel(CHANNEL, getString(R.string.notification_channel_name), NotificationManager.IMPORTANCE_LOW),
            )
            log("notification channel created")
        }
    }

    /**
     * The session's notification: a tap opens the app; its Close button stops what runs and closes
     * the app ([close]). While Close runs ([closing]), it says that the app closes, without the
     * button.
     */
    private fun notification(closing: Boolean): Notification {
        // The app's own activity, with what was on top of it in its task closed (a picker, Android's
        // installer window), not a second activity over them: one activity says whether the app shows.
        val activity = Intent(this, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_CLEAR_TOP or Intent.FLAG_ACTIVITY_SINGLE_TOP)
        // Its own request code: a pending intent of the same intent without these flags is not reused.
        val open = PendingIntent.getActivity(this, NOTIFICATION_ID, activity, PendingIntent.FLAG_IMMUTABLE)
        val builder = Notification.Builder(this, CHANNEL)
            .setSmallIcon(android.R.drawable.ic_dialog_info)
            .setContentIntent(open)
            .setOngoing(true)
        // Shown at once: Android 12 and later may hold a foreground service's notification back
        // for 10 seconds (the hold-back came with Android 12).
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) builder.setForegroundServiceBehavior(Notification.FOREGROUND_SERVICE_IMMEDIATE)
        if (closing) {
            return builder
                .setContentTitle(getString(R.string.notification_closing_title))
                .setContentText(getString(R.string.notification_closing_text))
                .build()
        }
        val close = PendingIntent.getService(
            this, REQUEST_CLOSE, Intent(this, SessionService::class.java).setAction(ACTION_CLOSE), PendingIntent.FLAG_IMMUTABLE,
        )
        val action = Notification.Action.Builder(Icon.createWithResource(this, R.drawable.ic_close), getString(R.string.notification_close), close)
        // Not from the lock screen: the game or an install stops only after an unlock (Android 12
        // and later; Android 11 has no such option, so there the button asks for no unlock).
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) action.setAuthenticationRequired(true)
        return builder
            .setContentTitle(getString(R.string.notification_title))
            .setContentText(getString(R.string.notification_text))
            .addAction(action.build())
            .build()
    }

    /** The official launcher's command [seq], sent after snapshot [afterSnapshot], which showed operation [before]. */
    private data class LauncherSent(val seq: Long, val afterSnapshot: Long, val before: Operation?)

    companion object {
        /** The jobs that run an installer (Blizzard's, the PD2 installer): their end makes a capture due ([PrefixState]). */
        private val INSTALLER_JOBS = setOf(
            com.pd2launcher.app.control.Job.INSTALL_BASE_GAME,
            com.pd2launcher.app.control.Job.INSTALL_PROJECT_DIABLO,
            com.pd2launcher.app.control.Job.REPAIR,
        )
        private const val TAG = "pd2-session"
        private const val ENGINE_TAG = "pd2-engine"
        private const val ENGINE_LINE = "pd2-launcher:"
        const val ACTION_START = "com.pd2launcher.app.action.START"
        /** Try again after the engine or the userland failed: a fresh start, its failures forgotten ([RestartPolicy]). */
        const val ACTION_RETRY = "com.pd2launcher.app.action.RETRY"
        const val ACTION_GAME_START = "com.pd2launcher.app.action.GAME_ENVIRONMENT_START"
        const val ACTION_GAME_STOP = "com.pd2launcher.app.action.GAME_ENVIRONMENT_STOP"
        /** Step 5, Play: with the update checks on, the look for a new version first ([PlayCheck]); then the game environment and the Game client. */
        const val ACTION_PLAY = "com.pd2launcher.app.action.PLAY"
        /** Play without updating: the game as installed, without the look for a new version. */
        const val ACTION_PLAY_WITHOUT_CHECK = "com.pd2launcher.app.action.PLAY_WITHOUT_CHECK"
        /** Play's Official PD2 launcher: the game environment at the game resolution, then the launcher. */
        const val ACTION_OFFICIAL_LAUNCHER = "com.pd2launcher.app.action.OFFICIAL_LAUNCHER"
        /** Step 1's Check again: the engine runs its startup check once more. */
        const val ACTION_CHECK_AGAIN = "com.pd2launcher.app.action.CHECK_AGAIN"
        /** Repair's Import after an update in place whose moved game the engine rejected ([importUpdatedGame]). */
        const val ACTION_IMPORT_UPDATED_GAME = "com.pd2launcher.app.action.IMPORT_UPDATED_GAME"
        /** Diablo II source: download from Blizzard. */
        const val ACTION_USE_BLIZZARD_DOWNLOAD = "com.pd2launcher.app.action.USE_BLIZZARD_DOWNLOAD"
        /** Step 3, Download: start it, or continue it after a stop. */
        const val ACTION_DOWNLOAD = "com.pd2launcher.app.action.DOWNLOAD"
        /** Stop safely: the running step stops where it is safe to. */
        const val ACTION_STOP_SAFELY = "com.pd2launcher.app.action.STOP_SAFELY"
        /** Step 4: Install Project Diablo 2 (again), with the game environment. */
        const val ACTION_INSTALL_PD2 = "com.pd2launcher.app.action.INSTALL_PD2"
        /** The notification's Close: what runs stops safely, then the session and the app's screens ([close]). */
        const val ACTION_CLOSE = "com.pd2launcher.app.action.CLOSE"
        /** Settings → Files and saves → Game files → Save sync: a sync now ([syncSavesSoon]), named by [EXTRA_SYNC_WHY]. */
        const val ACTION_SYNC_SAVES = "com.pd2launcher.app.action.SYNC_SAVES"
        /** The card's It works ([confirmRoute]). */
        const val ACTION_ROUTE_WORKS = "com.pd2launcher.app.action.ROUTE_WORKS"
        /** The card's Quit ([quitRoute]). */
        const val ACTION_ROUTE_QUIT = "com.pd2launcher.app.action.ROUTE_QUIT"
        /** Debug builds: the X screen's size for [ACTION_GAME_START], `<width>x<height>`. */
        const val EXTRA_X_SCREEN = "xscreen"
        /** Why [ACTION_SYNC_SAVES] came (Sync now, the setting turned on, another folder), for the log. */
        const val EXTRA_SYNC_WHY = "sync_why"
        /** The service was started with startForegroundService ([send]'s fallback): it shows the notification at once. */
        private const val EXTRA_FOREGROUND = "foreground"
        private const val CHANNEL = "session"
        private const val NOTIFICATION_ID = 1
        /** The Close button's pending intent; the notification's own uses [NOTIFICATION_ID]. */
        private const val REQUEST_CLOSE = 2
        /** The standalone ARM64 Launcher, shipped as a native library so it is extracted executable. */
        const val ENGINE_LIBRARY = "libpd2launcher.so"
        private const val HANDSHAKE_TIMEOUT_MS = 15_000L
        private const val ENGINE_STOP_TIMEOUT_MS = 10_000L
        private const val WINESERVER_TIMEOUT_MS = 10_000L
        private const val REMOVE_DOWNLOADS_TIMEOUT_MS = 120_000L
        private const val CLEANUP_WAKE_MS = 30_000L
        /** Beyond Close's own waits, for its last steps (the game environment's stop, the engine's exit). */
        private const val CLOSE_WAKE_MARGIN_MS = 30_000L
        /** The longest [endOtherProcesses] waits for the other processes of the uid to go. */
        private const val SWEEP_TIMEOUT_MS = 10_000L
        /** The longest a session start waits for the service stopped before it to stop its processes ([awaitPreviousService]). */
        private const val PREVIOUS_STOP_TIMEOUT_MS = 40_000L
        /** The longest Play waits for the check after a finished update to show ([Play.Recheck.WAITING]). */
        private const val RECHECK_WAIT_MS = 30_000L
        /**
         * The longest a launch request holds a save sync back ([syncSaves]) when nothing clears
         * it: longer than Play's buttons wait ([Play.LAUNCH_PENDING_MS]), as a slow game start
         * must not meet a sync.
         */
        private const val SYNC_LAUNCH_WAIT_MS = 120_000L
        /** The engine's refusal codes that [beginOperation] shows for its own refusals ([Refusals.text]). */
        private const val REFUSED_BUSY = "mutation-already-active"
        private const val REFUSED_ENGINE_BUSY = "engine-busy"
        private const val REFUSED_ENGINE_GONE = "service-unavailable"

        private val _status = MutableStateFlow(SessionStatus())
        val status: StateFlow<SessionStatus> = _status.asStateFlow()

        /** Whether the activity is started; the game's sound mutes in the background (see [applyBackgroundSound]). */
        @Volatile private var appForeground = true

        /**
         * How and when the app's screens last went away: the player closed the app, or it only
         * left the screen ([SessionIdle.Away]). A session's idle time starts no earlier, and its
         * wait depends on it.
         */
        @Volatile private var away = SessionIdle.Away(closed = false, since = 0)

        /**
         * The activity's onStart (true) and onStop (false); [closed]: the player closed the app
         * (Back on Play, or the activity ends), not only left it (Home, the lock, another app).
         */
        fun appForeground(foreground: Boolean, closed: Boolean = false) {
            if (!foreground) away = SessionIdle.Away(closed, SystemClock.elapsedRealtime())
            appForeground = foreground
            val service = running ?: return
            service.post { service.applyBackgroundSound() }
            // Closed with nothing running: the session ends at once.
            if (!foreground && closed) service.post { service.stopWhenIdle() }
        }

        /**
         * The player closed the app after its screens went away: swiped away from the recent apps
         * (the activity ends, or the task goes). With nothing running, the session ends at once.
         */
        fun appClosed() {
            if (appForeground) return
            away = SessionIdle.Away(closed = true, since = SystemClock.elapsedRealtime())
            val service = running ?: return
            service.post { service.stopWhenIdle() }
        }

        /** The running service, for [authorize]: keys never travel in an Intent. */
        @Volatile private var running: SessionService? = null

        /** The worker of the service that stopped last, which may still be stopping its processes ([onDestroy]). */
        @Volatile private var stoppingWorker: ExecutorService? = null

        /** The CD keys entered in this process, for Resume after a stop ([HeldKeys]). */
        private val heldKeys = HeldKeys()

        /**
         * Hands both CD keys from the key screen to the engine; the arrays are cleared once sent,
         * or at once when no session runs. Copies stay in memory for [resumeInstall].
         */
        fun authorize(d2Key: CharArray, lodKey: CharArray) {
            heldKeys.hold(d2Key, lodKey)
            sendKeys(d2Key, lodKey)
        }

        /**
         * Resume of a paused Install Diablo II: the keys entered in this process go to
         * the engine again, with the game environment. False when none are held (the app was
         * closed since): the key page asks for them.
         */
        fun resumeInstall(): Boolean {
            val (d2Key, lodKey) = heldKeys.copies() ?: return false
            sendKeys(d2Key, lodKey)
            return true
        }

        /**
         * Agree and install after Download failed or stopped ([KeysWhileDownloading.retries]):
         * one start, with the game environment, sends Download again and then the keys, which
         * join it; Install Diablo II follows it by itself.
         */
        fun authorizeWithDownload(d2Key: CharArray, lodKey: CharArray) {
            heldKeys.hold(d2Key, lodKey)
            val service = running
            try {
                if (service != null) {
                    service.worker.execute {
                        service.sendInstallStart(
                            com.pd2launcher.app.control.Job.INSTALL_BASE_GAME, cancelled = { d2Key.fill('\u0000'); lodKey.fill('\u0000') },
                        ) {
                            if (service.sendCommand(Command.DownloadAll) != null) {
                                service.sendAuthorize(d2Key, lodKey)
                            } else {
                                d2Key.fill('\u0000')
                                lodKey.fill('\u0000')
                                null
                            }
                        }
                    }
                    return
                }
            } catch (_: java.util.concurrent.RejectedExecutionException) {
            }
            d2Key.fill('\u0000')
            lodKey.fill('\u0000')
        }

        /** Whether [resumeInstall] has keys to send. */
        fun keysHeld(): Boolean = heldKeys.held

        /**
         * The keys the engine refused ([KeyProblems]), for the key page's fields; they are held no
         * more, so Resume cannot send them again. The caller clears them.
         */
        fun takeRefusedKeys(): Pair<CharArray, CharArray>? = heldKeys.take()

        /** The keys to the engine with the game environment; cleared once sent, or at once. */
        private fun sendKeys(d2Key: CharArray, lodKey: CharArray) {
            val service = running
            try {
                if (service != null) {
                    service.worker.execute {
                        service.sendInstallStart(
                            com.pd2launcher.app.control.Job.INSTALL_BASE_GAME, cancelled = { d2Key.fill('\u0000'); lodKey.fill('\u0000') },
                            joinsDownload = true,
                        ) { service.sendAuthorize(d2Key, lodKey) }
                    }
                    return
                }
            } catch (_: java.util.concurrent.RejectedExecutionException) {
            }
            d2Key.fill('\u0000')
            lodKey.fill('\u0000')
        }

        /**
         * A player command for the engine from a screen (Settings, the Diablo II source);
         * [withGameEnvironment] starts the game environment first, for commands that run Wine,
         * and drops the command when it does not start. Dropped when no session runs.
         */
        fun command(command: Command, withGameEnvironment: Boolean = false) {
            val service = running ?: return
            try {
                service.worker.execute {
                    if (withGameEnvironment) service.sendInstallStart(
                        if (command is Command.InstallProjectDiablo) com.pd2launcher.app.control.Job.INSTALL_PROJECT_DIABLO else com.pd2launcher.app.control.Job.INSTALL_BASE_GAME,
                        imported = command is Command.ImportExistingInstall,
                    ) { service.sendCommand(command) } else service.sendCommand(command)
                }
            } catch (_: java.util.concurrent.RejectedExecutionException) {
            }
        }

        /**
         * Removes the installer downloads (the notice on Play, Settings → Files and saves → Storage); [done]
         * gets the outcome on the session's worker thread.
         */
        fun removeInstallerDownloads(done: (InstallerDownloads.Outcome) -> Unit) {
            val service = running ?: return done(InstallerDownloads.Outcome.Failed("the session is not running"))
            try {
                service.worker.execute { done(service.removeInstallerDownloads()) }
            } catch (_: RejectedExecutionException) {
                done(InstallerDownloads.Outcome.Failed("the session is stopping"))
            }
        }

        /** The answer to "New version found" ([PlayCheck.State.Found]): Update, or Cancel. */
        fun answerNewVersion(update: Boolean) {
            val service = running ?: return
            service.post { service.answerNewVersion(update) }
        }

        /** The player dismissed the refusal on screen. */
        fun dismissRefusal() {
            _status.update { it.copy(refusal = null) }
        }

        /**
         * Before Share logs and Save logs: an open run of the official launcher writes its last
         * engine lines so far ([OfficialLauncherLog.checkpoint]), and the service log the engine
         * snapshots counted since its last snapshot line ([SnapshotLines.checkpoint]).
         */
        fun checkpointLogs() {
            val service = running ?: return
            service.launcherLog.checkpoint()
            service.snapshotLines.checkpoint()
        }

        /**
         * From the activity's return to the front: starts the session again when it ended by
         * itself meanwhile ([SessionIdle]); nothing while one runs.
         */
        fun resume(context: Context) {
            if (running == null || ending) send(context)
        }

        /** The session is ending because it was idle ([stopWhenIdle]); a start request clears it. */
        @Volatile private var ending = false

        /**
         * Starts the service (or hands it [action]), from the app's screens: a plain start, so no
         * notification shows while nothing must go on in the background ([followForeground]).
         * Android refuses it when it counts the app as in the background (a start just as the app
         * comes back): then a start in the foreground, which shows the notification until the
         * service sees that nothing must go on.
         */
        fun send(context: Context, action: String = ACTION_START, xScreen: String? = null, syncWhy: String? = null) {
            val intent = Intent(context, SessionService::class.java).setAction(action)
            if (xScreen != null) intent.putExtra(EXTRA_X_SCREEN, xScreen)
            if (syncWhy != null) intent.putExtra(EXTRA_SYNC_WHY, syncWhy)
            try {
                context.startService(intent)
            } catch (error: IllegalStateException) {
                Log.w(TAG, "session start $action refused in the background ($error); starting it in the foreground")
                context.startForegroundService(intent.putExtra(EXTRA_FOREGROUND, true))
            }
        }

        /** `<width>x<height>`, each 320 to 8192, else null. */
        fun parseSize(text: String): Pair<Int, Int>? {
            val (w, h) = Regex("([0-9]{3,4})x([0-9]{3,4})").matchEntire(text.trim())?.destructured ?: return null
            return (w.toInt() to h.toInt()).takeIf { (a, b) -> a in 320..8192 && b in 320..8192 }
        }
    }
}
