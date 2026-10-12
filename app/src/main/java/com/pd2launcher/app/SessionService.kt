package com.pd2launcher.app

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.os.IBinder
import android.os.PowerManager
import android.os.SystemClock
import android.system.ErrnoException
import android.system.Os
import android.system.OsConstants
import android.system.StructTimeval
import android.util.Log
import com.pd2launcher.app.control.Command
import com.pd2launcher.app.control.ControlChannel
import com.pd2launcher.app.control.DisplaySize
import com.pd2launcher.app.control.EngineMessage
import com.pd2launcher.app.control.Game
import com.pd2launcher.app.control.HostFacts
import com.pd2launcher.app.control.Installation
import com.pd2launcher.app.control.Snapshot
import com.pd2launcher.app.control.collect
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
import kotlinx.coroutines.flow.filterNotNull
import kotlinx.coroutines.flow.getAndUpdate
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch

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
) {
    enum class Engine { STOPPED, STARTING, CONNECTED, RESTARTING, FAILED }

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
 * The foreground service that owns every process:
 * the Launcher engine always, the Android game environment on demand, and the cleanup after
 * an engine exit (`wineserver -k`, then SIGTERM and SIGKILL after 5 s for every other
 * process of the app's uid) before the engine starts again.
 *
 * Every process-model step runs on one worker thread; reaper threads only hand over to it.
 */
class SessionService : Service() {
    private lateinit var paths: SessionPaths
    /** The device's GPU, probed once: Automatic's graphics choices and the engine's host values follow it. */
    private val gpu by lazy { HostGpu.probe() }
    private lateinit var serviceLog: RotatingLog
    private lateinit var starter: ProcessStarter
    private lateinit var game: GameEnvironment
    private val worker: ExecutorService = Executors.newSingleThreadExecutor { Thread(it, "pd2-session") }
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)

    // Worker thread only.
    private var engine: OwnedProcess? = null
    private var channel: ControlChannel? = null
    private var snapshots: Job? = null
    private var stopping = false
    private val restarts = ArrayDeque<Long>()
    /** Keeps the userland's resolv.conf on the default network's DNS servers. */
    private var dns: Dns? = null
    /** The install job running, or ending until the engine settles its outcome ([InstallProgress]). */
    @Volatile private var endingInstall: com.pd2launcher.app.control.Job? = null
    /** The running X screen's size, sent with `LaunchGame` as D2GL's viewport. */
    private var xScreen: Pair<Int, Int>? = null
    /** The X screen's size came from `--es xscreen` (debug builds), so Play keeps it. */
    private var xScreenGiven = false
    /** The `seq` of the install start the game environment was started for, until it is refused or another starts. */
    private var installStart: Long? = null
    /**
     * The `seq` of the last `LaunchGame` or `OpenOfficialLauncher` sent and when, until the engine
     * shows the game or the session, or refuses it ([Play.launchBusy]).
     */
    @Volatile private var launchSent: Pair<Long, Long>? = null
    /** When the Game client began running ([InstallerDownloads.played]); the snapshot collection only. */
    private var gameRunningSince: Long? = null

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onCreate() {
        super.onCreate()
        paths = SessionPaths(DeviceCheck.dataDir(this))
        paths.createFolders()
        serviceLog = RotatingLog(paths.logs, "service")
        starter = ProcessStarter(paths.logs, serviceLog)
        game = GameEnvironment(paths, starter, ::log, this)
        SetupRecordStore.load(this)
        // The first notification channel triggers Android 13's permission prompt at targetSdk 28:
        // it comes over Check, once the device check passed (MainActivity).
        createChannel()
        startForeground(NOTIFICATION_ID, notification())
        running = this
        log("service started (pid ${Os.getpid()}, uid ${Os.getuid()})")
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        worker.execute { if (engine == null && !stopping) startSession() }
        when (intent?.action) {
            ACTION_GAME_START -> {
                val size = intent.getStringExtra(EXTRA_X_SCREEN)?.let(::parseSize)
                worker.execute { startGameEnvironment(size) }
            }
            ACTION_GAME_STOP -> worker.execute { stopInGameView() }
            ACTION_PLAY -> worker.execute { play() }
            ACTION_OFFICIAL_LAUNCHER -> worker.execute { officialLauncher() }
            ACTION_CHECK_AGAIN -> worker.execute { sendCommand(Command.CheckAgain) }
            ACTION_USE_BLIZZARD_DOWNLOAD -> worker.execute { sendCommand(Command.UseBlizzardDownload) }
            ACTION_DOWNLOAD -> worker.execute { sendCommand(Command.DownloadAll) }
            ACTION_STOP_SAFELY -> worker.execute { sendCommand(Command.RequestSafeStop) }
            ACTION_INSTALL_PD2 -> worker.execute { installProjectDiablo() }
        }
        return START_NOT_STICKY
    }

    /** The processes stop on the worker thread, not the main one (the engine may take 10 s). */
    override fun onDestroy() {
        running = null
        log("service stopping")
        // The snapshot collection posts to the worker; it ends before the worker shuts down.
        scope.cancel()
        worker.execute {
            stopping = true
            // As in stopGameEnvironment: Wine's processes outlive Xvfb otherwise.
            if (game.running) wineserverKill()
            game.stop()
            stopEngine()
            dns?.stop()
            // A service started again meanwhile owns the status.
            if (running == null) _status.value = SessionStatus()
            serviceLog.line("service stopped")
            serviceLog.close()
        }
        worker.shutdown()
        super.onDestroy()
    }

    /** [task] on the worker thread; dropped once the service stopped (its worker is shut down). */
    private fun post(task: () -> Unit) {
        try {
            worker.execute(task)
        } catch (_: RejectedExecutionException) {
        }
    }

    /** Unpacks the App userland if needed, then starts the engine. */
    private fun startSession() {
        try {
            ensureUserland()
            ensureAppRuntime()
            if (dns == null) dns = Dns(this, listOf(paths.resolvConf, paths.guestResolvConf), ::log).also { it.start() }
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

    private fun ensureUserland() {
        setUserland(SessionStatus.Userland.Checking)
        val installer = UserlandInstaller(paths.userlandRoot, OwnedProcesses) { log("userland: $it") }
        val manifest = UserlandAssets.manifest(this)
        if (installer.currentVersion() == manifest.version && installer.isComplete(manifest.version)) {
            setUserland(SessionStatus.Userland.Ready(manifest.version))
            return
        }
        val started = SystemClock.elapsedRealtime()
        var shownPercent = -1L
        // Shown as unpacking from the start (Play: "Updating game files…"), not once the first
        // file is written after what an older version left has gone.
        setUserland(SessionStatus.Userland.Unpacking(0, manifest.fileBytes))
        val result = installer.install(manifest, { UserlandAssets.openTar(this) }) { done, total ->
            // One screen update per percent.
            val percent = if (total > 0) done * 100 / total else 0
            if (percent != shownPercent) {
                shownPercent = percent
                setUserland(SessionStatus.Userland.Unpacking(done, total))
            }
        }
        log("userland ${result.outcome} ${result.version} in ${SystemClock.elapsedRealtime() - started} ms ${result.detail}".trim())
        val current = installer.currentVersion()
        if (current == null) {
            setUserland(SessionStatus.Userland.Failed(result.detail.ifEmpty { result.outcome.toString() }))
            throw IOException("no App userland is current")
        }
        setUserland(SessionStatus.Userland.Ready(current))
    }

    /**
     * The runtime the app provides (RootFS, Wine-GE, Wine Gecko and Mono), unpacked once into the
     * Launcher's managed root before the engine starts; shown as part of the userland unpack.
     */
    private fun ensureAppRuntime() {
        val (version, bytes) = AppRuntime.info(this) ?: run {
            log("app runtime: none in the APK")
            return
        }
        val runtime = AppRuntime(paths.managedRoot, ::log)
        if (runtime.isCurrent(version)) return
        val ready = _status.value.userland
        val started = SystemClock.elapsedRealtime()
        var shownPercent = -1L
        // As unpacking from the start: the older version's trees are removed first, which takes
        // seconds on a phone, and Play says "Updating game files…" through it.
        setUserland(SessionStatus.Userland.Unpacking(0, bytes))
        AppRuntime.openTar(this).use { tar ->
            runtime.install(version, tar) { done ->
                val percent = if (bytes > 0) done * 100 / bytes else 0
                if (percent != shownPercent) {
                    shownPercent = percent
                    setUserland(SessionStatus.Userland.Unpacking(done, bytes))
                }
            }
        }
        log("app runtime $version in ${SystemClock.elapsedRealtime() - started} ms")
        setUserland(ready)
    }

    /**
     * The prefix's Windows Installer cache ([InstallerCache]) and the runtime's archive copies
     * ([AppRuntime.ARCHIVES]) go once the game is installed; the archive copies come
     * back from the APK when the installation needs setting up again (after Start over, say),
     * before the player can start that install: commands run on this worker thread after it.
     * Run only as the installation's state changes ([AppRuntime.sweep]): never before the
     * engine's audit ends, nor while a job or the game runs.
     */
    private fun keepRuntimeArchives(sweep: AppRuntime.Sweep) {
        val runtime = AppRuntime(paths.managedRoot, ::log)
        try {
            when (sweep) {
                AppRuntime.Sweep.REMOVE -> {
                    runtime.removeArchives().takeIf { it.isNotEmpty() }?.let { log("runtime archive copies removed: $it") }
                    InstallerCache.remove(paths.managedRoot).takeIf { it > 0 }?.let { log("Windows Installer cache: $it .msi files removed") }
                }
                AppRuntime.Sweep.RESTORE -> if (runtime.archivesMissing() && AppRuntime.info(this) != null) {
                    val restored = AppRuntime.openTar(this).use { runtime.restoreArchives(it) }
                    log("runtime archive copies restored from the APK: $restored")
                }
            }
        } catch (error: Exception) {
            log("runtime archive copies: $error")
        }
    }

    private fun setUserland(userland: SessionStatus.Userland) {
        _status.update { it.copy(userland = userland) }
    }

    /** A player command for the engine; dropped (and logged) while it is not connected. Returns its `seq` when sent. */
    private fun sendCommand(command: Command): Long? {
        val connection = channel
        if (connection == null || connection.state.value !is ControlChannel.State.Connected) {
            log("command $command not sent: the engine is not connected")
            return null
        }
        if (command is Command.StartOver && !keepSaves()) return null
        return try {
            connection.send(command).also {
                log("command seq $it: $command")
                _status.update { status -> status.copy(refusal = null) }
                // No paused Install Diablo II goes on after Start over or an import.
                if (HeldKeys.forgets(command)) heldKeys.clear()
            }
        } catch (error: Exception) {
            log("command $command failed: $error")
            null
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
        log("engine command error: seq ${error.seq}: ${error.reason}")
        _status.update { it.copy(refusal = Refusal(error.seq, error.reason)) }
        if (Refusals.stopsGameEnvironment(error.seq, installStart, _status.value.snapshot)) {
            log("install start seq ${error.seq} refused; stopping the game environment started for it")
            stopGameEnvironment()
        }
        if (error.seq == installStart) installStart = null
        if (error.seq == launchSent?.first) launchSent = null
    }

    private fun startEngine() {
        _status.update { it.copy(engine = SessionStatus.Engine.STARTING) }
        val enginePath = File(applicationInfo.nativeLibraryDir, ENGINE_LIBRARY)
        if (!enginePath.isFile) throw IOException("the engine is not in the APK ($enginePath)")
        val loader = paths.loader.path
        val pair = Spawner.socketPair()
        val process = try {
            // Started through the userland's loader.
            starter.start(
                "engine", loader, listOf(loader, enginePath.path, "--engine"),
                paths.engineEnvironment(mali = gpu is MaliGpu) + gameSessionEnvironment(), pair.child,
                // Debug builds: the engine's own lines (`pd2-launcher: …`) also in logcat.
                mirror = if (BuildConfig.DEBUG) { line -> if (line.startsWith(ENGINE_LINE)) Log.i(ENGINE_TAG, line) } else null,
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
        val socket = pair.app.fileDescriptor
        try {
            Os.setsockoptTimeval(socket, OsConstants.SOL_SOCKET, OsConstants.SO_RCVTIMEO, StructTimeval.fromMillis(HANDSHAKE_TIMEOUT_MS))
            val welcome = connection.handshake(appVersion(), HostFacts.collect(this))
            Os.setsockoptTimeval(socket, OsConstants.SOL_SOCKET, OsConstants.SO_RCVTIMEO, StructTimeval.fromMillis(0))
            log("engine pid ${process.pid} welcome: protocol ${welcome.protocol}, launcher ${welcome.launcherVersion}")
            // Only an engine that answered proves the userland works; every older version goes now.
            UserlandInstaller(paths.userlandRoot, OwnedProcesses) { log("userland: $it") }.markStarted()
        } catch (error: Exception) {
            // The engine exits without a valid hello; its exit runs the cleanup.
            log("engine pid ${process.pid} handshake failed: $error")
            connection.close()
            return
        }
        _status.update { it.copy(engine = SessionStatus.Engine.CONNECTED) }
        snapshots?.cancel()
        snapshots = scope.launch {
            connection.snapshot.filterNotNull().collect { message ->
                log("engine snapshot ${summary(message)}")
                val next = message.snapshot
                val now = SystemClock.elapsedRealtime()
                val before = _status.getAndUpdate {
                    it.copy(snapshotSeq = message.seq, snapshot = next, updateRecheck = Play.recheck(it.updateRecheck, it.snapshot, next))
                }
                val previous = before.snapshot
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
                    if (record == SetupRecord.NONE) AppSettingsStore.startedOver(this@SessionService)
                }
                post { applyBackgroundSound() }
                AppRuntime.sweep(previous, next)?.let { sweep -> post { keepRuntimeArchives(sweep) } }
                // Diablo II and Lord of Destruction are in, or their install failed: the held keys are done with.
                if (HeldKeys.spent(previous, next)) heldKeys.clear()
                // The launch sent has shown: Play and the official PD2 launcher take presses again.
                if (next.game == Game.RUNNING || next.activeJob == com.pd2launcher.app.control.Job.OFFICIAL_LAUNCHER) launchSent = null
                if (Play.gameStarted(previous?.game, next.game)) gameRunningSince = now
                // The Game client exited: the game environment goes with it.
                if (Play.gameEnded(previous?.game, next.game)) {
                    val since = gameRunningSince
                    // Logged before wineserver -k, while what the launch left still runs.
                    if (EarlyGameEnd.early(since, now)) post { logEarlyGameEnd(now - since!!) }
                    post { stopGameEnvironment() }
                    // A game session that ran: Play's one-time notice may show from now on
                    // (InstallerDownloads.showNotice); one that ended at once does not count.
                    if (InstallerDownloads.played(gameRunningSince, now)) post { AppSettingsStore.gamePlayed(this@SessionService) }
                    gameRunningSince = null
                }
                // So does the official PD2 launcher's session.
                if (Play.officialLauncherEnded(previous?.activeJob, next.activeJob)) post { stopGameEnvironment() }
                // Install Diablo II done: Install Project Diablo 2 follows; after the last
                // install job the game environment stops.
                val ending = endingInstall
                if (InstallProgress.continuesToPd2(ending, next)) {
                    post { installProjectDiablo() }
                } else if (InstallProgress.installEnded(ending, next)) {
                    post { stopGameEnvironment() }
                }
                endingInstall = InstallProgress.endingInstallJob(ending, next)
            }
        }
    }

    /** Closing the channel makes the engine stop safely and exit; SIGKILL if it does not in time. */
    private fun stopEngine() {
        val process = engine ?: return
        channel?.close()
        if (!awaitExit(process, ENGINE_STOP_TIMEOUT_MS)) {
            log("engine pid ${process.pid} did not exit within $ENGINE_STOP_TIMEOUT_MS ms; SIGKILL")
            sendSignal(process.pid, UidCleanup.SIGKILL)
        }
        engine = null
    }

    private fun onEngineExit(process: OwnedProcess, status: ExitStatus) {
        if (engine?.pid != process.pid) return
        engine = null
        snapshots?.cancel()
        channel?.close()
        channel = null
        if (stopping) return
        log("engine pid ${process.pid} ended unexpectedly (${ProcessStarter.describe(status)}); cleaning up before a restart")
        _status.update { it.copy(engine = SessionStatus.Engine.RESTARTING, snapshotSeq = null, snapshot = null) }
        // Awake for the cleanup: with the screen off the device suspends between polls, and the
        // 5 s grace is counted in elapsed real time (which includes suspend).
        val wakeLock = getSystemService(PowerManager::class.java).newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "pd2:cleanup")
        wakeLock.acquire(CLEANUP_WAKE_MS)
        try {
            wineserverKill()
            UidCleanup(
                list = { UidCleanup.scan(File("/proc"), Os.getuid(), Os.getpid(), ::owner) },
                signal = ::sendSignal,
                log = ::log,
                now = SystemClock::elapsedRealtime,
            ).run()
        } finally {
            if (wakeLock.isHeld) wakeLock.release()
        }
        game.forget()
        xScreen = null
        installStart = null
        launchSent = null
        _status.update { it.copy(gameEnvironment = false, refusal = null, updateRecheck = null) }

        val now = SystemClock.elapsedRealtime()
        while (restarts.isNotEmpty() && now - restarts.first() > RESTART_WINDOW_MS) restarts.removeFirst()
        if (restarts.size >= MAX_RESTARTS) {
            log("engine ended ${restarts.size + 1} times within ${RESTART_WINDOW_MS / 1000} s; not restarting")
            _status.update { it.copy(engine = SessionStatus.Engine.FAILED) }
            return
        }
        restarts.addLast(now)
        log("restarting the engine")
        try {
            startEngine()
        } catch (error: Exception) {
            log("engine restart failed: $error")
            _status.update { it.copy(engine = SessionStatus.Engine.FAILED) }
        }
    }

    /**
     * `wineserver -k` for the Managed prefix: the engine binary's `--stop-wine` runs Wine-GE's
     * wineserver through App-provided FEX with the game's Wine environment, as the engine runs
     * its guests, so it works whether or not an engine is running. Waits at most
     * [WINESERVER_TIMEOUT_MS].
     */
    private fun wineserverKill() {
        val enginePath = File(applicationInfo.nativeLibraryDir, ENGINE_LIBRARY)
        val loader = paths.loader.path
        try {
            val process = starter.start(
                "stop-wine", loader, listOf(loader, enginePath.path, "--stop-wine"), paths.engineEnvironment(mali = gpu is MaliGpu),
            )
            if (!awaitExit(process, WINESERVER_TIMEOUT_MS)) {
                log("wineserver -k did not finish within $WINESERVER_TIMEOUT_MS ms; SIGKILL")
                sendSignal(process.pid, UidCleanup.SIGKILL)
            }
        } catch (error: IOException) {
            log("wineserver -k failed to start: ${error.message}")
        }
    }

    /**
     * The engine's `--remove-downloads` removes the installer downloads ([InstallerDownloads]);
     * refused while the game, an install or a repair runs (the engine also refuses while the
     * installation is in use). Waits at most [REMOVE_DOWNLOADS_TIMEOUT_MS].
     */
    private fun removeInstallerDownloads(): InstallerDownloads.Outcome {
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
                paths.engineEnvironment(mali = gpu is MaliGpu),
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
     * At the display's native landscape size unless [size] is given; [given]: the size came from
     * `--es xscreen` (debug builds), so Play keeps it.
     */
    private fun startGameEnvironment(size: Pair<Int, Int>?, given: Boolean = size != null) {
        if (engine == null) {
            log("game environment not started: the engine is not running")
            return
        }
        val (width, height) = size ?: landscapeSize()
        try {
            game.start(width, height)
            if (xScreen == null) {
                xScreen = width to height
                xScreenGiven = given
            }
            _status.update { it.copy(gameEnvironment = true, gameEnvironmentFailure = null) }
        } catch (error: Exception) {
            _status.update { it.copy(gameEnvironment = false, gameEnvironmentFailure = error.message ?: error.toString()) }
        }
    }

    /**
     * Whether a launch is under way or something runs ([Play.launchBusy]): then Play and the
     * official PD2 launcher send nothing, and the game environment stays as it is (stopped or
     * started again at another size, it would take the running game or session with it).
     */
    private fun launchBusy(what: String): Boolean =
        Play.launchBusy(_status.value.snapshot, launchSent?.second, SystemClock.elapsedRealtime()).also { busy ->
            if (busy) log("$what not started: the game, a job or a launch already runs; the game environment stays as it is")
        }

    /**
     * Play: the game environment (if not running yet, or again when the game resolution changed
     * its X screen's size), then `LaunchGame` with the X screen's size, the graphics driver and
     * D2GL's graphics profile ([GraphicsProfiles.forPlay]) in the app's settings now; nothing while
     * a launch is under way or something runs ([launchBusy]).
     */
    private fun play() {
        if (launchBusy("Play")) return
        val settings = AppSettingsStore.load(this)
        if (game.running && !xScreenGiven) {
            val wanted = landscapeSize()
            if (xScreen != wanted) {
                log("the game resolution wants a ${wanted.first} x ${wanted.second} X screen; restarting the game environment")
                stopGameEnvironment()
            }
        }
        if (!game.running) startGameEnvironment(null)
        val (width, height) = xScreen?.takeIf { game.running } ?: run {
            log("game not launched: the game environment is not running")
            return
        }
        linkSaves()
        launchSent = sendCommand(
            Command.LaunchGame(
                DisplaySize(width.toLong(), height.toLong()),
                settings.graphicsDriver,
                GraphicsProfiles.forPlay(settings.graphicsProfile, gpu),
            ),
        )?.let { it to SystemClock.elapsedRealtime() }
    }

    /**
     * The official PD2 launcher: the game environment at its own resolution (Settings → Game →
     * Official launcher resolution, the display's full size by default; started again when it
     * runs at another size), then `OpenOfficialLauncher`. A refusal stops the game environment
     * started for it, as for an install start. Nothing while a launch is under way or something
     * runs ([launchBusy]).
     */
    private fun officialLauncher() {
        if (launchBusy("official launcher")) return
        val (width, height) = GameResolution.display(this)
        val wanted = GameResolution.forLauncher(width, height, AppSettingsStore.load(this).launcherResolution)
        if (game.running && xScreen != wanted) {
            log("the official launcher wants a ${wanted.first} x ${wanted.second} X screen; restarting the game environment")
            stopGameEnvironment()
        }
        val started = !game.running
        if (started) startGameEnvironment(wanted, given = false)
        if (!game.running) {
            log("official launcher not opened: the game environment is not running")
            return
        }
        val seq = sendCommand(Command.OpenOfficialLauncher)
        installStart = seq?.takeIf { started }
        launchSent = seq?.let { it to SystemClock.elapsedRealtime() }
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
            SaveKeeper.keepAll(SessionPaths(DeviceCheck.dataDir(this)), log = ::log)
            true
        } catch (error: Exception) {
            log("Start over not sent: the saves could not be moved to the player's folder: $error")
            _status.update { it.copy(refusal = Refusal(-1, Refusals.SAVES_NOT_KEPT)) }
            false
        }
    }

    /**
     * Before a launch the game folder's Save links to the player's own folder ([SaveKeeper.link]),
     * so the game reads and writes the characters there; only for an installed game nothing runs in.
     * Only a ready installation is linked: a folder copy still being installed must stay free of
     * links, as the engine refuses a copy that holds one when it checks it again.
     */
    private fun linkSaves() {
        val paths = SessionPaths(DeviceCheck.dataDir(this))
        val snapshot = _status.value.snapshot ?: return
        if (snapshot.installation != Installation.READY) return
        val folder = snapshot.gameFolder
        if (folder?.available == false || snapshot.activeJob != null || snapshot.game == Game.RUNNING) return
        val save = SaveKeeper.liveSave(paths, folder?.path)
        if (save.parentFile?.isDirectory != true) return
        try {
            SaveKeeper.link(save, paths.userSave, paths.saveBackups, log = ::log)
        } catch (error: Exception) {
            // Nothing is lost: what was not moved is still in the game's Save; the next launch tries again.
            log("Save not linked to the player's folder: $error")
        }
    }

    /**
     * Install runs Wine with the game environment's X screen and sound: the game environment
     * first, then [send]; nothing is sent without it. When the game environment was started for
     * it, a refusal of the sent command stops it again.
     */
    private fun sendInstallStart(send: () -> Long?): Boolean {
        val started = !game.running
        if (started) startGameEnvironment(null)
        if (!game.running) {
            log("install not sent: the game environment is not running")
            return false
        }
        installStart = send()?.takeIf { started }
        return true
    }

    private fun installProjectDiablo() {
        sendInstallStart { sendCommand(Command.InstallProjectDiablo) }
    }

    /**
     * The game's sound follows the app's visibility and the Launcher's "Sound in background"
     * setting; it holds audio focus only while the Game client runs.
     */
    private fun applyBackgroundSound() {
        val snapshot = _status.value.snapshot
        val setting = snapshot?.settings?.soundInBackground ?: true
        game.setBackground(!appForeground, setting, gameRunning = snapshot?.game == Game.RUNNING)
    }

    /**
     * Stop in the Game view: the official PD2 launcher's session stops through the engine, which
     * closes it and its Wine processes safely (the game environment follows once the session has
     * ended); the Game client goes with the game environment.
     */
    private fun stopInGameView() {
        if (_status.value.snapshot?.activeJob == com.pd2launcher.app.control.Job.OFFICIAL_LAUNCHER) {
            log("official PD2 launcher: safe stop requested")
            sendCommand(Command.RequestSafeStop)
        } else {
            stopGameEnvironment()
        }
    }

    private fun stopGameEnvironment() {
        // Wine's processes (wineserver, the wine64-preloaders under FEX) outlive Xvfb otherwise.
        wineserverKill()
        game.stop()
        xScreen = null
        xScreenGiven = false
        _status.update { it.copy(gameEnvironment = false) }
        log("game environment stopped")
    }

    /**
     * The game ended [ranMs] after it started ([EarlyGameEnd]): the FEX telemetry written since the
     * launch (FEX's data folder in the managed root, as the engine sets it) and the app's other
     * processes, each with the binary it runs.
     */
    private fun logEarlyGameEnd(ranMs: Long) {
        val now = System.currentTimeMillis()
        val telemetry = EarlyGameEnd.telemetry(
            File(paths.managedRoot, "fex/data/Telemetry"), now - ranMs - EarlyGameEnd.LAUNCH_MS, now,
        )
        val processes = UidCleanup.scan(File("/proc"), Os.getuid(), Os.getpid(), ::owner).map { process ->
            val exe = try {
                Os.readlink("/proc/${process.pid}/exe")
            } catch (_: ErrnoException) {
                "?"
            }
            "${process.pid} ${process.name} ($exe)"
        }
        log(
            "the game ended $ranMs ms after it started; FEX telemetry since the launch: " +
                telemetry.ifEmpty { listOf("none") }.joinToString("; ") +
                "; the app's processes: " + processes.ifEmpty { listOf("none") }.joinToString("; "),
        )
    }

    /**
     * The X screen's size: the display's native size in landscape at the game resolution in the
     * app's settings ([GameResolution.forPlay]).
     */
    private fun landscapeSize(): Pair<Int, Int> {
        val (width, height) = GameResolution.display(this)
        return GameResolution.forPlay(width, height, AppSettingsStore.load(this).gameResolution, gpu)
    }

    /** The game environment's X display, PulseAudio socket and gamepad file, for the guests the engine starts. */
    private fun gameSessionEnvironment(): List<String> = listOf(
        "DISPLAY=${GameEnvironment.DISPLAY}",
        "PULSE_SERVER=unix:${File(paths.runtimeDir, "pulse/native").path}",
        "PD2_GAMEPAD_FILE=${GameEnvironment.gamepadFile(paths).path}",
    )

    private fun owner(file: File): Int? = try {
        Os.stat(file.path).st_uid
    } catch (_: ErrnoException) {
        null
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
        return "seq=${message.seq} host=${s.host} missing=${s.missing} installation=${s.installation} " +
            "update=${s.update} step=${s.step} job=${s.activeJob}$operation"
    }

    private fun appVersion(): String = packageManager.getPackageInfo(packageName, 0).versionName ?: "unknown"

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

    private fun notification(): Notification {
        val open = PendingIntent.getActivity(
            this, 0, Intent(this, MainActivity::class.java), PendingIntent.FLAG_IMMUTABLE,
        )
        return Notification.Builder(this, CHANNEL)
            .setSmallIcon(android.R.drawable.ic_dialog_info)
            .setContentTitle(getString(R.string.notification_title))
            .setContentText(getString(R.string.notification_text))
            .setContentIntent(open)
            .setOngoing(true)
            .build()
    }

    companion object {
        private const val TAG = "pd2-session"
        private const val ENGINE_TAG = "pd2-engine"
        private const val ENGINE_LINE = "pd2-launcher:"
        const val ACTION_START = "com.pd2launcher.app.action.START"
        const val ACTION_GAME_START = "com.pd2launcher.app.action.GAME_ENVIRONMENT_START"
        const val ACTION_GAME_STOP = "com.pd2launcher.app.action.GAME_ENVIRONMENT_STOP"
        /** Step 5, Play: the game environment, then the Game client. */
        const val ACTION_PLAY = "com.pd2launcher.app.action.PLAY"
        /** Play's Official PD2 launcher: the game environment at the launcher's resolution, then the launcher. */
        const val ACTION_OFFICIAL_LAUNCHER = "com.pd2launcher.app.action.OFFICIAL_LAUNCHER"
        /** Step 1's Check again: the engine runs its startup check once more. */
        const val ACTION_CHECK_AGAIN = "com.pd2launcher.app.action.CHECK_AGAIN"
        /** Diablo II source: download from Blizzard. */
        const val ACTION_USE_BLIZZARD_DOWNLOAD = "com.pd2launcher.app.action.USE_BLIZZARD_DOWNLOAD"
        /** Step 3, Download: start it, or continue it after a stop. */
        const val ACTION_DOWNLOAD = "com.pd2launcher.app.action.DOWNLOAD"
        /** Stop safely: the running step stops where it is safe to. */
        const val ACTION_STOP_SAFELY = "com.pd2launcher.app.action.STOP_SAFELY"
        /** Step 4: Install Project Diablo 2 (again), with the game environment. */
        const val ACTION_INSTALL_PD2 = "com.pd2launcher.app.action.INSTALL_PD2"
        /** Debug builds: the X screen's size for [ACTION_GAME_START], `<width>x<height>`. */
        const val EXTRA_X_SCREEN = "xscreen"
        private const val CHANNEL = "session"
        private const val NOTIFICATION_ID = 1
        /** The standalone ARM64 Launcher, shipped as a native library so it is extracted executable. */
        const val ENGINE_LIBRARY = "libpd2launcher.so"
        private const val HANDSHAKE_TIMEOUT_MS = 15_000L
        private const val ENGINE_STOP_TIMEOUT_MS = 10_000L
        private const val WINESERVER_TIMEOUT_MS = 10_000L
        private const val REMOVE_DOWNLOADS_TIMEOUT_MS = 120_000L
        private const val CLEANUP_WAKE_MS = 30_000L
        private const val RESTART_WINDOW_MS = 60_000L
        private const val MAX_RESTARTS = 3
        /** The longest Play waits for the check after a finished update to show ([Play.Recheck.WAITING]). */
        private const val RECHECK_WAIT_MS = 30_000L

        private val _status = MutableStateFlow(SessionStatus())
        val status: StateFlow<SessionStatus> = _status.asStateFlow()

        /** Whether the activity is started; the game's sound mutes in the background (see [applyBackgroundSound]). */
        @Volatile private var appForeground = true

        /** The activity's onStart (true) and onStop (false). */
        fun appForeground(foreground: Boolean) {
            appForeground = foreground
            val service = running ?: return
            service.post { service.applyBackgroundSound() }
        }

        /** The running service, for [authorize]: keys never travel in an Intent. */
        @Volatile private var running: SessionService? = null

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

        /** Whether [resumeInstall] has keys to send. */
        fun keysHeld(): Boolean = heldKeys.held

        /** The keys to the engine with the game environment; cleared once sent, or at once. */
        private fun sendKeys(d2Key: CharArray, lodKey: CharArray) {
            val service = running
            try {
                if (service != null) {
                    service.worker.execute {
                        try {
                            service.sendInstallStart { service.sendAuthorize(d2Key, lodKey) }
                        } finally {
                            // Without the game environment the install cannot run: the keys are
                            // not sent (cleared here), and the key screen shows why (gameEnvironmentFailure).
                            d2Key.fill('\u0000')
                            lodKey.fill('\u0000')
                        }
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
                    if (withGameEnvironment) service.sendInstallStart { service.sendCommand(command) } else service.sendCommand(command)
                }
            } catch (_: java.util.concurrent.RejectedExecutionException) {
            }
        }

        /**
         * Removes the installer downloads (the notice on Play, Settings → App → Storage); [done]
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

        /** The player dismissed the refusal on screen. */
        fun dismissRefusal() {
            _status.update { it.copy(refusal = null) }
        }

        /** Starts the service (or hands it [action]). */
        fun send(context: Context, action: String = ACTION_START, xScreen: String? = null) {
            val intent = Intent(context, SessionService::class.java).setAction(action)
            if (xScreen != null) intent.putExtra(EXTRA_X_SCREEN, xScreen)
            context.startForegroundService(intent)
        }

        /** `<width>x<height>`, each 320 to 8192, else null. */
        fun parseSize(text: String): Pair<Int, Int>? {
            val (w, h) = Regex("([0-9]{3,4})x([0-9]{3,4})").matchEntire(text.trim())?.destructured ?: return null
            return (w.toInt() to h.toInt()).takeIf { (a, b) -> a in 320..8192 && b in 320..8192 }
        }
    }
}
