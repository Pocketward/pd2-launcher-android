package com.pd2launcher.app

import android.Manifest
import android.annotation.SuppressLint
import android.app.NotificationManager
import android.content.ActivityNotFoundException
import android.content.Context
import android.content.Intent
import android.content.pm.ActivityInfo
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.os.PowerManager
import android.os.StatFs
import android.os.SystemClock
import android.provider.DocumentsContract
import android.provider.Settings
import android.util.Log
import android.view.KeyEvent
import android.view.WindowManager
import androidx.activity.ComponentActivity
import androidx.activity.SystemBarStyle
import androidx.activity.compose.BackHandler
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import androidx.core.view.WindowInsetsControllerCompat
import androidx.lifecycle.lifecycleScope
import androidx.lifecycle.withStarted
import com.pd2launcher.app.control.Command
import com.pd2launcher.app.control.Game
import com.pd2launcher.app.control.Installation
import com.pd2launcher.app.control.Installer
import com.pd2launcher.app.control.Step
import com.pd2launcher.app.input.PadButton
import com.pd2launcher.app.ui.AboutScreen
import com.pd2launcher.app.ui.ControlsEditor
import com.pd2launcher.app.ui.CheckScreen
import com.pd2launcher.app.ui.CheckingDeviceScreen
import com.pd2launcher.app.ui.DownloadScreen
import com.pd2launcher.app.ui.DownloadsNotice
import com.pd2launcher.app.ui.FormFactor
import com.pd2launcher.app.ui.GameView
import com.pd2launcher.app.ui.InstallProgress
import com.pd2launcher.app.ui.InstallRun
import com.pd2launcher.app.ui.InstallScreen
import com.pd2launcher.app.ui.InstallState
import com.pd2launcher.app.ui.KeysScreen
import com.pd2launcher.app.ui.LogShareUi
import com.pd2launcher.app.ui.LauncherTheme
import com.pd2launcher.app.ui.LocalFormFactor
import com.pd2launcher.app.ui.LocalLogShare
import com.pd2launcher.app.ui.LocalNetworkCheck
import com.pd2launcher.app.ui.NetworkCheckUi
import com.pd2launcher.app.ui.NetworkState
import com.pd2launcher.app.ui.LocalRefusal
import com.pd2launcher.app.ui.NotSupportedScreen
import com.pd2launcher.app.ui.Play
import com.pd2launcher.app.ui.PlayDestinations
import com.pd2launcher.app.ui.PlayScreen
import com.pd2launcher.app.ui.RefusalNotice
import com.pd2launcher.app.ui.RepairScreen
import com.pd2launcher.app.ui.rememberImported
import com.pd2launcher.app.ui.rememberInstallerDownloadBytes
import com.pd2launcher.app.ui.SettingsNav
import com.pd2launcher.app.ui.SettingsScreen
import com.pd2launcher.app.ui.SourceScreen
import com.pd2launcher.app.ui.SystemState
import com.pd2launcher.app.ui.text
import java.io.File
import java.io.FileNotFoundException
import java.time.Instant
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/**
 * The native screens: the device check, then Check,
 * Download, Install and Play from the engine's snapshot.
 * Laid out for the display's shape (ui/FormFactor.kt): phones in portrait, handhelds in
 * landscape, edge to edge; the Game view is landscape on both. Touch and controller (d-pad
 * focus, A selects, B goes back). Back goes to the previous step where that is safe; while a
 * download or install runs it leaves the app and the work keeps running.
 */
class MainActivity : ComponentActivity() {
    private val notificationsOff = mutableStateOf(false)
    /** Debug builds only: the [DebugPreview] screen asked for with `--es preview <name>`. */
    private val preview = mutableStateOf<String?>(null)
    /** Debug builds only: the Game view, opened with `--ez gameview true`. */
    private val gameView = mutableStateOf(false)
    /** Debug builds only: the key screen, opened with `--ez keys true` (Install also opens it). */
    private val keys = mutableStateOf(false)
    /** Where the player went on to from Check, and the way back. */
    private val nav = mutableStateOf(FlowNav())
    /** Start over was pressed on setup's Repair; its reset leads straight on ([FlowNav.afterStartOver]). */
    private val startedOver = mutableStateOf(false)
    /** The install the player started (or found running), for its tasks and figure. */
    private val installRun = mutableStateOf<InstallRun?>(null)
    /** Play or the official PD2 launcher was pressed and the engine has not shown it yet ([Play.launchPending]). */
    private val launchPress = mutableStateOf<Play.LaunchPress?>(null)
    /** The source step's selection while the player chooses; null: the engine's or the default. */
    private val sourceChoice = mutableStateOf<SourceChoice?>(null)
    /** Settings, from Play or the game's menu, and where in it. */
    private val settingsOpen = mutableStateOf(false)
    private val settingsNav = SettingsNav()
    /** The on-screen controls editor, from Settings, over a sample screen. */
    private val editorOpen = mutableStateOf(false)
    /** Repair, from Play, or from Check when the installation's game folder is gone. */
    private val repairOpen = mutableStateOf(false)
    /** About, from Play or Settings; over both. */
    private val aboutOpen = mutableStateOf(false)
    /** Share logs and Save logs, from Settings → App and beside a failure on Check, Download and Install ([shareLogs], [saveLogs]). */
    private val logShare = LogShareUi(onShare = ::shareLogs, onSave = ::saveLogs)
    /** Whether the servers can be reached ([NetworkCheck]): on Check, and under a failed download ([checkNetwork]). */
    private val networkCheck = NetworkCheckUi(onRecheck = ::checkNetwork, onOpenSettings = ::openAppSettings)
    /** The network checks' results, beside the app's other logs (Share logs packs them). */
    private val networkLog by lazy { RotatingLog(paths.logs, "network") }
    /** Save logs: Android's "Save as" screen for the zip; [saveChosen] copies it where the player chose. */
    private val saveLogsTo = registerForActivityResult(SaveInDownload(), ::saveChosen)
    /** Which layout the launcher screens use, from the display's shape (ui/FormFactor.kt). */
    private lateinit var formFactor: FormFactor
    private val batteryRestricted = mutableStateOf(false)
    private var sessionStarted = false
    private val paths by lazy { SessionPaths(DeviceCheck.dataDir(this)) }
    /** The debug entry points below ([readScreenExtras], [startSession], the tests in onCreate); never in a release build. */
    private val debuggable get() = BuildConfig.DEBUG

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        if (debuggable) {
            // `am start -n com.pd2launcher.app.dev/com.pd2launcher.app.MainActivity --ez probe true`.
            if (intent.getBooleanExtra("probe", false)) {
                Thread { SeccompProbe.runAndRecord(applicationContext) }.start()
            }
            // `am start -n com.pd2launcher.app.dev/com.pd2launcher.app.MainActivity --ez spawntest true`.
            if (intent.getBooleanExtra("spawntest", false)) {
                Thread { SpawnTest.run(applicationContext) }.start()
            }
            // `am start -n com.pd2launcher.app.dev/com.pd2launcher.app.MainActivity --ez unpacktest true [--ez corrupt true]`.
            if (intent.getBooleanExtra("unpacktest", false)) {
                val corrupt = intent.getBooleanExtra("corrupt", false)
                Thread {
                    UnpackTest.run(applicationContext, corrupt) { Log.i(TAG, "unpack test: $it") }
                }.start()
            }
        }

        formFactor = FormFactor.of(this)
        requestedOrientation = formFactor.launcherOrientation
        readScreenExtras(intent)
        AppSettingsStore.load(this)
        // An install started and not finished opens on Install; an installed game on Play (Root).
        nav.value = FlowNav.opening(SetupRecordStore.load(this))
        // The device check comes before anything is unpacked; the session service (which
        // unpacks and starts the engine) starts only once it passed.
        DeviceCheckRun.start(this)

        enableEdgeToEdge(
            statusBarStyle = SystemBarStyle.dark(android.graphics.Color.TRANSPARENT),
            navigationBarStyle = SystemBarStyle.dark(android.graphics.Color.TRANSPARENT),
        )
        // The install's run follows every snapshot, and the clock for parts timed by their
        // typical length, also while the app is in the background.
        lifecycleScope.launch { SessionService.status.collect { trackInstall() } }
        lifecycleScope.launch {
            while (true) {
                delay(1_000)
                trackInstall()
            }
        }
        setContent {
            LauncherTheme {
                // The engine's refusal of the last command shows on the screen it came from.
                val refusal = SessionService.status.collectAsState().value.refusal
                CompositionLocalProvider(
                    LocalFormFactor provides formFactor,
                    LocalRefusal provides refusal?.let { RefusalNotice(it, SessionService::dismissRefusal) },
                    LocalLogShare provides logShare,
                    LocalNetworkCheck provides networkCheck,
                ) { Root() }
            }
        }
    }

    @Composable
    private fun Root() {
        val device by DeviceCheckRun.state.collectAsState()
        val status by SessionService.status.collectAsState()
        val framebuffer by GameEnvironment.framebuffer.collectAsState()
        val allowed = Onboarding.sessionAllowed(device)
        LaunchedEffect(allowed) { if (allowed) startSessionOnce() }

        val previewName = preview.value
        BackHandler(enabled = previewName != null) { preview.value = null }
        val previewScreen = previewName?.let { DebugPreview.screen(it, device, status, notificationsOff.value, networkCheck.state) }
        val play = Play.mode(status)
        // From a press of Play or the official PD2 launcher until the game or its session shows
        // (or the engine refuses it), both buttons wait, so a second press cannot restart the game
        // environment under it; followed here, whichever screen shows meanwhile.
        val press = launchPress.value
        val launching = Play.launchPending(press, status, SystemClock.elapsedRealtime())
        LaunchedEffect(launching) { if (!launching) launchPress.value = null }
        LaunchedEffect(press) {
            if (press != null) {
                delay(Play.LAUNCH_PENDING_MS)
                if (launchPress.value === press) launchPress.value = null
            }
        }
        // Once installed (the app's own record), Play or Repair, never Check or the install flow;
        // Start over's reset ends it (the record goes): Repair closes and Check follows.
        val record by SetupRecordStore.current.collectAsState()
        val installed = if (record == SetupRecord.INSTALLED) Onboarding.installed(device, status) else null
        LaunchedEffect(record) { if (record == SetupRecord.NONE) repairOpen.value = false }
        // While the Game client runs the Game view stays; it ends when the game exits.
        val playing = play == Play.Mode.GAME
        BackHandler(enabled = playing) {}
        val gamePreview = DebugPreview.gameLayer(previewName)
        val showGameView = (gameView.value && debuggable) || playing || gamePreview != null
        BackHandler(enabled = showGameView && !playing && gamePreview == null) { gameView.value = false }
        // The controls editor lays them out on the game's landscape screen, without system bars.
        val gameScreen = showGameView || editorOpen.value
        DisposableEffect(showGameView, gameScreen) {
            val bars = WindowCompat.getInsetsController(window, window.decorView)
            // Settings → Keep the screen on during play, read as the Game view opens.
            val keepOn = showGameView && AppSettingsStore.current.value.keepScreenOn
            if (keepOn) window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
            if (gameScreen) {
                bars.systemBarsBehavior = WindowInsetsControllerCompat.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
                bars.hide(WindowInsetsCompat.Type.systemBars())
                // The game's X screen is landscape on every device, phones included, however held.
                requestedOrientation = ActivityInfo.SCREEN_ORIENTATION_SENSOR_LANDSCAPE
            }
            onDispose {
                if (keepOn) window.clearFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
                if (gameScreen) {
                    bars.show(WindowInsetsCompat.Type.systemBars())
                    requestedOrientation = formFactor.launcherOrientation
                }
            }
        }
        val showKeys = keys.value && debuggable
        BackHandler(enabled = showKeys) { keys.value = false }
        val sourceCopy by SourceImport.copy.collectAsState()
        val pickFolder = rememberLauncherForActivityResult(ActivityResultContracts.OpenDocumentTree()) { tree ->
            if (tree != null) SourceImport.copyFolder(this, tree)
        }

        val back = { nav.value = nav.value.back() }
        // The download finished while Download was shown: Install follows.
        val installReady = InstallProgress.page(status) != null
        LaunchedEffect(installReady) { if (installReady) nav.value = nav.value.downloadDone() }
        // Start over reset the interrupted stage: straight to the keys, or Install's start button.
        val installPage = InstallProgress.page(status)
        LaunchedEffect(installPage) {
            val snapshot = status.snapshot
            if (startedOver.value && installPage == InstallProgress.Page.KEYS && snapshot != null) {
                startedOver.value = false
                nav.value = nav.value.afterStartOver(needsKeys = !DiabloSource.importsExisting(snapshot))
            }
        }
        val notificationPermission = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { granted ->
            // Android stops asking once the player declined twice: its settings page instead.
            if (!granted && !shouldShowRequestPermissionRationale(Manifest.permission.POST_NOTIFICATIONS)) openNotificationSettings()
        }
        val grant: (Grant) -> Unit = { grant ->
            when (grant) {
                Grant.NOTIFICATIONS ->
                    // At targetSdk 28 Android asks by itself only when the first channel is made;
                    // after that only its settings page turns notifications on.
                    if (applicationInfo.targetSdkVersion >= 33) {
                        notificationPermission.launch(Manifest.permission.POST_NOTIFICATIONS)
                    } else {
                        openNotificationSettings()
                    }
                Grant.BATTERY -> requestBatteryExemption()
                Grant.NETWORK -> openAppSettings()
            }
        }

        @Composable
        fun Source(onBack: () -> Unit = back, onPreviewContinue: (() -> Unit)? = null) {
            val choice = sourceChoice.value ?: status.snapshot?.installer?.let(DiabloSource::choiceOf) ?: SourceChoice.BLIZZARD
            val view = DiabloSource.view(choice, status, sourceCopy)
            SourceScreen(
                view,
                onSelect = { sourceChoice.value = it },
                onPickFolder = { pickFolder.launch(null) },
                onContinue = {
                    if (view.continuePicks) {
                        pickFolder.launch(null)
                    } else if (onPreviewContinue != null) {
                        onPreviewContinue()
                    } else {
                        // No confirmation page: the download starts as Download shows.
                        val select = DiabloSource.continueCommand(choice, status.snapshot?.installer)
                        select?.let { SessionService.command(it) }
                        // A new source makes what was downloaded or installed for the old one stale.
                        val ready = installReady && select == null
                        if (!ready && Onboarding.startsDownload(status, sourceChanges = select != null)) {
                            SessionService.command(Command.DownloadAll)
                        }
                        nav.value = nav.value.afterSource(ready)
                    }
                },
                onBack = onBack,
            )
        }

        val openSettings: (Int) -> Unit = { tab ->
            settingsNav.open(tab)
            settingsOpen.value = true
        }
        val leavePreview = { preview.value = null }

        if (showKeys) {
            KeysScreen(
                onAuthorize = { d2Key, lodKey ->
                    SessionService.authorize(d2Key, lodKey)
                    keys.value = false
                },
                onBack = { keys.value = false },
            )
        } else if (aboutOpen.value) {
            AboutScreen(onBack = { aboutOpen.value = false }, runtime = (status.userland as? SessionStatus.Userland.Ready)?.version)
        } else if (editorOpen.value) {
            ControlsEditor(sample = true, onClose = { editorOpen.value = false })
        } else if (showGameView && !settingsOpen.value) {
            // The menu's Settings shows them over the running game; back returns to it.
            GameView(framebuffer, status.gameEnvironment, presentCounter = paths.presentCounter, onSettings = openSettings, preview = gamePreview, previewLauncher = DebugPreview.gameLauncher(previewName))
        } else if (settingsOpen.value) {
            val app by AppSettingsStore.current.collectAsState()
            SettingsScreen(
                launcher = LauncherSettings.view(status.snapshot?.settings, status.engine == SessionStatus.Engine.CONNECTED),
                app = app,
                display = remember { GameResolution.display(this) },
                highestFrameRate = remember { FrameRateVote.highest(this) },
                // The device check's probe, not another on the main thread (its ioctls) each time.
                gpu = (device as? DeviceCheckState.Done)?.facts?.gpu,
                system = SystemState(batteryRestricted.value, notificationsOff.value),
                nav = settingsNav,
                fromGame = showGameView,
                onCommand = { SessionService.command(it) },
                onAppChange = { AppSettingsStore.save(this, it) },
                onBatterySettings = { openSystemSettings(Intent(Settings.ACTION_IGNORE_BATTERY_OPTIMIZATION_SETTINGS)) },
                onNotificationSettings = ::openNotificationSettings,
                onEditOnScreen = { editorOpen.value = true },
                onAbout = { aboutOpen.value = true },
                onBack = { settingsOpen.value = false },
            )
        } else if (previewName == DebugPreview.SOURCE) {
            // Continue sends nothing here: the running Download preview shows what follows.
            Source(onBack = leavePreview, onPreviewContinue = { preview.value = DebugPreview.DOWNLOAD })
        } else if (previewName == DebugPreview.KEYS || previewName == DebugPreview.KEYS_TYPING) {
            KeysScreen(
                onAuthorize = { d2Key, lodKey -> d2Key.fill('\u0000'); lodKey.fill('\u0000') },
                onBack = leavePreview,
                layoutPreview = true,
                typingPreview = previewName == DebugPreview.KEYS_TYPING,
            )
        } else if (previewName != null && DebugPreview.install(previewName, status) != null) {
            val session = DebugPreview.install(previewName, status)!!
            val run = DebugPreview.installRun(previewName, session, SystemClock.elapsedRealtime())
            val install = InstallProgress.view(session, run)
            InstallScreen(
                install, InstallProgress.tasks(install, run, taskLabels(session)), onStopSafely = {}, onInstall = {}, onBack = leavePreview,
                window = framebuffer.takeIf { status.gameEnvironment },
            )
        } else if (previewName == DebugPreview.REPAIR) {
            RepairScreen(status, onCheckAgain = {}, onStartOver = {}, onBack = leavePreview)
        } else if (previewName == DebugPreview.PLAY || DebugPreview.notice(previewName) != null || DebugPreview.playUpdate(previewName, status) != null) Box(Modifier.fillMaxSize()) {
            val shown = DebugPreview.playUpdate(previewName, status) ?: status
            PlayScreen(
                facts = Play.facts(shown.snapshot, (device as? DeviceCheckState.Done)?.facts?.gpu?.name),
                problem = null,
                onPlay = {},
                destinations = PlayDestinations({ openSettings(SettingsNav.GAME) }, { openSettings(SettingsNav.CONTROLLER) }, {}, { aboutOpen.value = true }),
                update = Play.update(shown),
            )
            // Its answers save nothing here.
            DebugPreview.notice(previewName)?.let { DownloadsNotice(it, onRemove = leavePreview, onLater = leavePreview) }
        } else if (previewName == DebugPreview.ABOUT) {
            AboutScreen(onBack = leavePreview, runtime = (status.userland as? SessionStatus.Userland.Ready)?.version)
        } else if (previewScreen != null) {
            when (previewScreen) {
                Screen.CheckingDevice -> CheckingDeviceScreen()
                is Screen.NotSupported -> NotSupportedScreen(previewScreen, onClose = leavePreview)
                is Screen.Check -> CheckScreen(previewScreen, onCheckAgain = {}, onContinue = {}, onGrant = grant)
                is Screen.Download -> {
                    // As on the real page: no Back button while the download runs (system Back leaves the preview).
                    val working = previewScreen.state == DownloadState.RUNNING || previewScreen.state == DownloadState.STOPPING
                    DownloadScreen(previewScreen, onDownload = {}, onStopSafely = {}, onBack = leavePreview.takeUnless { working })
                }
                Screen.Source -> Source()
                Screen.Install, Screen.Keys -> leavePreview()
            }
        } else if (repairOpen.value || installed == Installed.Repair) {
            RepairScreen(
                status,
                onCheckAgain = {
                    // Repair stays through the new check (Play would show while it runs) and says
                    // when the installation is Ready again; Back then returns to Play.
                    repairOpen.value = true
                    SessionService.send(this, SessionService.ACTION_CHECK_AGAIN)
                },
                onStartOver = {
                    // Installed, Repair stays until the reset ends (then Check) and shows a failure on it.
                    if (record != SetupRecord.INSTALLED) repairOpen.value = false
                    nav.value = FlowNav()
                    SessionService.command(Command.StartOver)
                },
                // Back to Play; with the installation found broken (or Start over running) there
                // is none: Back leaves the app.
                onBack = { if (installed == Installed.Repair) moveTaskToBack(true) else repairOpen.value = false },
            )
        } else if (installed is Installed.Refused) {
            NotSupportedScreen(installed.screen, onClose = ::finish)
        } else if (installed is Installed.Play || play == Play.Mode.PLAY) Box(Modifier.fillMaxSize()) {
            val pressed = {
                SessionService.dismissRefusal()
                launchPress.value = Play.LaunchPress(SystemClock.elapsedRealtime(), status.gameEnvironmentFailure)
            }
            PlayScreen(
                // The engine's placeholder before its audit names no renderer or update yet.
                facts = Play.facts(status.snapshot?.takeIf(Onboarding::audited), (device as? DeviceCheckState.Done)?.facts?.gpu?.name),
                problem = (installed as? Installed.Play)?.problem?.let { text(it) }
                    ?: status.gameEnvironmentFailure ?: Play.officialLauncherProblem(status)?.let { getString(it) },
                waiting = (installed as? Installed.Play)?.waiting?.let { text(it) },
                onPlay = {
                    pressed()
                    SessionService.send(this@MainActivity, SessionService.ACTION_PLAY)
                },
                // In the engine alone: no game environment.
                update = Play.update(status),
                onCommand = { SessionService.command(it) },
                onOfficialLauncher = {
                    // In the game environment, as the game: the Game view shows it once its session runs.
                    pressed()
                    SessionService.send(this@MainActivity, SessionService.ACTION_OFFICIAL_LAUNCHER)
                }.takeIf { Play.officialLauncherEnabled(status) },
                launching = launching,
                onTryAgain = { SessionService.send(this@MainActivity, SessionService.ACTION_RETRY) }.takeIf { Onboarding.failed(status) },
                destinations = PlayDestinations(
                    onSettings = { openSettings(SettingsNav.GAME) },
                    onControls = { openSettings(SettingsNav.CONTROLLER) },
                    onRepair = { repairOpen.value = true },
                    onAbout = { aboutOpen.value = true },
                ),
            )
            // Once, after the first game session (never right after the install): the installer
            // downloads and, after an import, the Diablo II folder the player copied onto the device can go.
            val app by AppSettingsStore.current.collectAsState()
            val downloadBytes by rememberInstallerDownloadBytes(status.snapshot?.installation)
            val importedNow by rememberImported(status.snapshot?.installation)
            val bytes = downloadBytes
            val imported = importedNow
            if (bytes != null && imported != null && InstallerDownloads.showNotice(status.snapshot, app.downloadsNoticeAnswered, app.gamePlayed, bytes, imported)) {
                val answer = { AppSettingsStore.save(this@MainActivity, AppSettingsStore.load(this@MainActivity).copy(downloadsNoticeAnswered = true)) }
                DownloadsNotice(
                    InstallerDownloads.notice(bytes, imported, remember { SourceImport.pickedFolder(this@MainActivity) }),
                    onRemove = {
                        answer()
                        SessionService.removeInstallerDownloads { Log.i(TAG, "installer downloads from the notice: $it") }
                    },
                    onLater = { answer() },
                )
            }
        } else when (
            val screen = Onboarding.screen(
                device, status, notificationsOff.value, nav.value.page, batteryRestricted = batteryRestricted.value,
                network = networkCheck.state,
            )
        ) {
            Screen.CheckingDevice -> CheckingDeviceScreen()
            is Screen.NotSupported -> NotSupportedScreen(screen, onClose = ::finish)
            is Screen.Check -> {
                // The first network check, once Check shows; onResume runs it again on each return.
                LaunchedEffect(Unit) { if (networkCheck.state == null) checkNetwork() }
                CheckScreen(
                    screen,
                    onCheckAgain = { SessionService.send(this, SessionService.ACTION_CHECK_AGAIN) },
                    onContinue = { nav.value = nav.value.open(FlowPage.SOURCE) },
                    onRepair = { repairOpen.value = true },
                    onGrant = grant,
                    onTryAgain = { SessionService.send(this, SessionService.ACTION_RETRY) },
                )
            }
            Screen.Source -> Source()
            is Screen.Download -> {
                val downloading = screen.state == DownloadState.RUNNING || screen.state == DownloadState.STOPPING
                // While the download runs Back leaves the app; it goes on in the session service.
                BackHandler(enabled = downloading) { moveTaskToBack(true) }
                DownloadScreen(
                    screen,
                    onDownload = {
                        if (status.snapshot?.installer == Installer.NotSelected) {
                            SessionService.send(this, SessionService.ACTION_USE_BLIZZARD_DOWNLOAD)
                        }
                        SessionService.send(this, SessionService.ACTION_DOWNLOAD)
                    },
                    onStopSafely = { SessionService.send(this, SessionService.ACTION_STOP_SAFELY) },
                    onContinue = { nav.value = nav.value.open(FlowPage.INSTALL) },
                    onBack = if (downloading) null else back,
                )
            }
            // An interrupted stage takes no keys and no install until Start over (Repair).
            Screen.Install, Screen.Keys -> if (InstallProgress.page(status) == InstallProgress.Page.REPAIR) {
                RepairScreen(
                    status,
                    onCheckAgain = { SessionService.send(this, SessionService.ACTION_CHECK_AGAIN) },
                    onStartOver = {
                        startedOver.value = true
                        SessionService.command(Command.StartOver)
                    },
                    onBack = back,
                )
            } else if (screen == Screen.Install) {
                val run = installRun.value
                val install = InstallProgress.view(status, run)
                // Never back to a step while install work runs: Back leaves the app.
                BackHandler(enabled = install.working) { moveTaskToBack(true) }
                InstallScreen(
                    install,
                    InstallProgress.tasks(install, run, taskLabels(status)),
                    onStopSafely = { SessionService.send(this, SessionService.ACTION_STOP_SAFELY) },
                    onInstall = { startInstall(status) },
                    onBack = back,
                    window = framebuffer.takeIf { status.gameEnvironment },
                )
            } else KeysScreen(
                onAuthorize = { d2Key, lodKey ->
                    // The running Install shows at once; the game environment starts first.
                    beginInstall(status)
                    SessionService.authorize(d2Key, lodKey)
                    nav.value = nav.value.installStarted()
                },
                onBack = back,
                problem = InstallProgress.view(status).problem?.let { text(it) },
                // A paused Install Diablo II whose keys went with the app: they continue it.
                resuming = InstallProgress.view(status).state == InstallState.STOPPED,
            )
        }
    }

    /**
     * Install's start: Install Project Diablo 2 when Diablo II is in, an installed Diablo II is
     * imported, otherwise the key page asks for the CD keys first (its "Agree and install" starts).
     */
    private fun startInstall(status: SessionStatus) {
        val snapshot = status.snapshot ?: return
        val existing = snapshot.installer as? Installer.Existing
        val imports = existing != null && DiabloSource.importsExisting(snapshot)
        when (InstallProgress.startAction(snapshot, InstallProgress.view(status).state, imports, SessionService.keysHeld())) {
            InstallProgress.StartAction.PROJECT_DIABLO -> {
                beginInstall(status)
                SessionService.send(this, SessionService.ACTION_INSTALL_PD2)
            }
            InstallProgress.StartAction.IMPORT -> {
                beginInstall(status)
                SessionService.command(Command.ImportExistingInstall(DiabloSource.importMode(existing!!)), withGameEnvironment = true)
            }
            // Resume: Install Diablo II goes on with the keys entered before the stop.
            InstallProgress.StartAction.HELD_KEYS -> {
                beginInstall(status)
                if (!SessionService.resumeInstall()) nav.value = nav.value.open(FlowPage.KEYS)
            }
            InstallProgress.StartAction.KEY_PAGE -> nav.value = nav.value.open(FlowPage.KEYS)
        }
    }

    private fun beginInstall(status: SessionStatus) {
        installRun.value = InstallProgress.start(installSource(status), status, SystemClock.elapsedRealtime())
    }

    private fun trackInstall() {
        val status = SessionService.status.value
        val next = InstallProgress.track(installRun.value, status, installSource(status), SystemClock.elapsedRealtime())
        if (next != installRun.value) installRun.value = next
    }

    /** The source the engine has, else the one on screen. */
    private fun installSource(status: SessionStatus): SourceChoice =
        status.snapshot?.installer?.let(DiabloSource::choiceOf) ?: sourceChoice.value ?: SourceChoice.BLIZZARD

    /** The task names for the player's source (an installed Diablo II is copied or adopted). */
    private fun taskLabels(status: SessionStatus): InstallProgress.TaskLabels {
        val existing = status.snapshot?.installer as? Installer.Existing
        val copied = SourceImport.copy.value as? SourceCopy.Copied
        return InstallProgress.TaskLabels(
            source = installSource(status),
            inPlace = existing?.inPlacePossible == true,
            projectDiabloFound = copied != null && copied.projectDiablo != ProjectDiabloFound.NONE,
        )
    }

    private fun openNotificationSettings() {
        openSystemSettings(Intent(Settings.ACTION_APP_NOTIFICATION_SETTINGS).putExtra(Settings.EXTRA_APP_PACKAGE, packageName))
    }

    /** Android's own "Let app always run in background?" for the app, else its battery optimisation list. */
    @SuppressLint("BatteryLife")
    private fun requestBatteryExemption() {
        try {
            startActivity(Intent(Settings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS, Uri.parse("package:$packageName")))
        } catch (error: ActivityNotFoundException) {
            Log.w(TAG, "no battery optimisation prompt: $error")
            openSystemSettings(Intent(Settings.ACTION_IGNORE_BATTERY_OPTIMIZATION_SETTINGS))
        }
    }

    /** A system settings page; some handhelds' firmware lacks one, which changes nothing. */
    private fun openSystemSettings(intent: Intent) {
        try {
            startActivity(intent)
        } catch (error: ActivityNotFoundException) {
            Log.w(TAG, "no settings page for ${intent.action}: $error")
        }
    }

    /**
     * Share logs: [prepareLogs], then Android's share sheet offers the zip. A second press while
     * one is prepared does nothing.
     */
    private fun shareLogs() = prepareLogs(LogShareUi.Action.SHARE) { zip ->
        startActivity(LogShare.chooser(this, zip, getString(R.string.share_logs_subject), getString(R.string.share_logs_chooser)))
    }

    /**
     * Save logs: [prepareLogs], then Android's "Save as" screen asks where to put the zip (for
     * example Downloads) and [saveChosen] copies it there.
     */
    private fun saveLogs() = prepareLogs(LogShareUi.Action.SAVE) { zip -> saveLogsTo.launch(zip.name) }

    /**
     * The zip for Share logs and Save logs: the engine saves a report first when it runs
     * ([LogShare.awaitReport]), then [LogShare] packs it with the app's logs and [aboutDevice], and
     * [hand] passes it on once the app is on screen (a player who went elsewhere meanwhile finds it
     * on return). A save stays WORKING until the "Save as" screen answers ([saveChosen]).
     */
    private fun prepareLogs(action: LogShareUi.Action, hand: (File) -> Unit) {
        if (logShare.state == LogShareUi.State.WORKING) return
        logShare.action = action
        logShare.state = LogShareUi.State.WORKING
        lifecycleScope.launch {
            val handed = try {
                val outcome = LogShare.awaitReport(
                    SessionService.status.map { LogShare.ReportWatch(it.engine == SessionStatus.Engine.CONNECTED, it.snapshot?.report) },
                    request = { SessionService.command(Command.ExportDiagnostics) },
                    dismiss = { SessionService.command(Command.DismissReport) },
                    timeoutMs = REPORT_WAIT_MS,
                )
                if (outcome !is LogShare.ReportOutcome.Saved) Log.w(TAG, "${action.name.lowercase()} logs: engine report ${LogShare.describe(outcome)}")
                val zip = withContext(Dispatchers.IO) {
                    LogShare.write(cacheDir, paths, aboutDevice(outcome), (outcome as? LogShare.ReportOutcome.Saved)?.file)
                }
                lifecycle.withStarted { hand(zip) }
                true
            } catch (error: CancellationException) {
                throw error
            } catch (error: Exception) {
                Log.w(TAG, "${action.name.lowercase()} logs: $error")
                false
            }
            when {
                !handed -> logShare.state = LogShareUi.State.FAILED
                action == LogShareUi.Action.SHARE -> logShare.state = LogShareUi.State.IDLE
            }
        }
    }

    /**
     * The "Save as" screen's answer: null when the player closed it. Copies the zip the last
     * [prepareLogs] left ([LogShare.latest], also after Android made the activity again meanwhile)
     * to [target] ([LogShare.saveTo]), emptied first: a player who picked an older file to overwrite
     * gets no tail of it. A provider that cannot empty a file ("wt") is written as "w".
     */
    private fun saveChosen(target: Uri?) {
        logShare.action = LogShareUi.Action.SAVE
        if (target == null) {
            logShare.state = LogShareUi.State.IDLE
            return
        }
        logShare.state = LogShareUi.State.WORKING
        lifecycleScope.launch {
            val saved = try {
                withContext(Dispatchers.IO) {
                    LogShare.saveTo(
                        LogShare.latest(cacheDir),
                        open = {
                            try {
                                contentResolver.openOutputStream(target, "wt")
                            } catch (error: IllegalArgumentException) {
                                contentResolver.openOutputStream(target, "w")
                            } catch (error: FileNotFoundException) {
                                contentResolver.openOutputStream(target, "w")
                            }
                        },
                        delete = { DocumentsContract.deleteDocument(contentResolver, target) },
                    )
                }
                true
            } catch (error: CancellationException) {
                throw error
            } catch (error: Exception) {
                Log.w(TAG, "save logs: $error")
                false
            }
            logShare.state = if (saved) LogShareUi.State.SAVED else LogShareUi.State.FAILED
        }
    }

    /**
     * `device.txt`: this device, the app, where the installation stands and how the wait for the
     * engine's [report] ended. Free space is measured now, not at the device check.
     */
    private fun aboutDevice(report: LogShare.ReportOutcome): String {
        val status = SessionService.status.value
        val device = DeviceCheckRun.state.value
        val facts = (device as? DeviceCheckState.Done)?.facts
        val snapshot = status.snapshot
        val operation = snapshot?.operation
        return LogShare.about(
            listOf(
                "App" to "${BuildConfig.VERSION_NAME} (${BuildConfig.VERSION_CODE}, ${BuildConfig.APPLICATION_ID}, ${BuildConfig.BUILD_TYPE})",
                "Created" to Instant.now().toString(),
                "Device" to "${Build.MANUFACTURER} ${Build.MODEL} (${Build.DEVICE})",
                "Chip" to "${Build.SOC_MANUFACTURER} ${Build.SOC_MODEL}",
                "Android" to "${Build.VERSION.RELEASE} (API ${Build.VERSION.SDK_INT}), ${Build.SUPPORTED_ABIS.joinToString(" ")}",
                "GPU" to (facts?.gpu?.name ?: "not found"),
                "Page size" to (facts?.pageSize?.toString() ?: "unknown"),
                "Free space" to "${StatFs(paths.data).availableBytes shr 20} MB",
                "Device check" to when (device) {
                    is DeviceCheckState.Done -> device.result.findings.joinToString("; ") { "${it.kind}: ${it.message}" }.ifEmpty { "passed" }
                    is DeviceCheckState.Failed -> "failed: ${device.reason}"
                    DeviceCheckState.Running -> "running"
                },
                "Data folder" to paths.data,
                "Game support files" to status.userland.toString(),
                "Engine" to status.engine.toString(),
                "Installation" to (snapshot?.installation?.toString() ?: "unknown"),
                "Step" to (snapshot?.step?.toString() ?: "unknown"),
                "Operation" to (operation?.let { "${it.phase} ${it.status}, failure ${it.failure ?: "none"}" } ?: "none"),
                "Game environment" to (status.gameEnvironmentFailure?.let { "failed: $it" } ?: if (status.gameEnvironment) "running" else "stopped"),
                "Engine report" to LogShare.describe(report),
            ),
        )
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        setIntent(intent)
        readScreenExtras(intent)
        if (sessionStarted) startSession(intent)
    }

    override fun onStart() {
        super.onStart()
        SessionService.appForeground(true)
    }

    override fun onStop() {
        super.onStop()
        SessionService.appForeground(false)
    }

    override fun onResume() {
        super.onResume()
        notificationsOff.value = !getSystemService(NotificationManager::class.java).areNotificationsEnabled()
        batteryRestricted.value = !getSystemService(PowerManager::class.java).isIgnoringBatteryOptimizations(packageName)
        // Again on each return during setup (the player may have allowed the app's network meanwhile),
        // and for a debug preview, whatever the installation; Check runs the first one itself.
        val snapshot = SessionService.status.value.snapshot
        val setup = snapshot != null && snapshot.installation != Installation.READY && snapshot.game != Game.RUNNING
        if (setup || preview.value != null) checkNetwork()
    }

    /**
     * Runs the network check ([NetworkCheck]) unless one runs: Check shows its row, a failed download
     * its note, and its report goes to the app's network log.
     */
    private fun checkNetwork() {
        val last = networkCheck.state
        if (last is NetworkState.Checking) return
        networkCheck.state = NetworkState.Checking(last?.report)
        lifecycleScope.launch {
            val report = withContext(Dispatchers.IO) {
                runCatching { NetworkCheck.run(this@MainActivity) }
                    .onSuccess { networkLog.line(it.describe()) }
                    .onFailure { Log.w(TAG, "network check: $it") }
                    .getOrNull()
            }
            networkCheck.state = report?.let { NetworkState.Done(it) } ?: last
        }
    }

    /** The app's own settings page, where Android allows or blocks its Wi-Fi and mobile data. */
    private fun openAppSettings() =
        openSystemSettings(Intent(Settings.ACTION_APPLICATION_DETAILS_SETTINGS, Uri.fromParts("package", packageName, null)))

    /**
     * Controllers: A selects and B goes back on every native screen. Android does not treat
     * the gamepad's A as a confirm key, so it is handed on as the d-pad's center, and B as Back.
     */
    override fun dispatchKeyEvent(event: KeyEvent): Boolean {
        val mapped = when (event.keyCode) {
            KeyEvent.KEYCODE_BUTTON_A -> KeyEvent.KEYCODE_DPAD_CENTER
            KeyEvent.KEYCODE_BUTTON_B -> KeyEvent.KEYCODE_BACK
            else -> return super.dispatchKeyEvent(event)
        }
        return super.dispatchKeyEvent(
            KeyEvent(
                event.downTime, event.eventTime, event.action, mapped, event.repeatCount,
                event.metaState, event.deviceId, event.scanCode, event.flags, event.source,
            ),
        )
    }

    /**
     * Debug builds only: `--es preview <name>` (`--ei tab <n>` and `--es picker <button>` with
     * `settings`, `--ez mouse true` for mouse mode), `--ez gameview true` and `--ez keys true`.
     */
    private fun readScreenExtras(intent: Intent) {
        if (!debuggable) return
        preview.value = intent.getStringExtra("preview")
        if (preview.value == DebugPreview.SETTINGS) {
            preview.value = null
            settingsNav.open(intent.getIntExtra("tab", SettingsNav.GAME))
            settingsNav.mouse = intent.getBooleanExtra("mouse", false)
            settingsNav.picker = intent.getStringExtra("picker")?.let { name -> PadButton.entries.firstOrNull { it.name == name } }
            settingsOpen.value = true
        } else if (preview.value != null) {
            settingsOpen.value = false
        }
        if (intent.getBooleanExtra("gameview", false)) gameView.value = true
        if (intent.getBooleanExtra("keys", false)) keys.value = true
    }

    private fun startSessionOnce() {
        if (sessionStarted) return
        sessionStarted = true
        startSession(intent)
    }

    /**
     * Starts the session service. Debug builds also take
     * `--es session game-start|game-stop` to start or stop the Android game environment, and
     * `--es xscreen <width>x<height>` with game-start for an X screen of another size than the
     * display's (a phone-sized proxy), and `--es session install-pd2` for Install Project
     * Diablo 2 as Install sends it (the engine refuses it unless the client is what is missing),
     * and `--es session download` for Download as its page sends it (on an installed game it only
     * checks the downloads it holds).
     */
    private fun startSession(intent: Intent) {
        val action = when (if (debuggable) intent.getStringExtra("session") else null) {
            "game-start" -> SessionService.ACTION_GAME_START
            "game-stop" -> SessionService.ACTION_GAME_STOP
            "install-pd2" -> SessionService.ACTION_INSTALL_PD2
            "download" -> SessionService.ACTION_DOWNLOAD
            else -> SessionService.ACTION_START
        }
        SessionService.send(this, action, if (debuggable) intent.getStringExtra("xscreen") else null)
    }

    private companion object {
        const val TAG = "pd2-session"
        /** How long Share logs and Save logs wait for the engine's report before they go on without one. */
        const val REPORT_WAIT_MS = 20_000L
    }
}

/** Android's "Save as" screen for the logs' zip, opened in the Download folder ([LogShare.SAVE_FOLDER_DOCUMENT]). */
private class SaveInDownload : ActivityResultContracts.CreateDocument("application/zip") {
    override fun createIntent(context: Context, input: String): Intent =
        super.createIntent(context, input).putExtra(
            DocumentsContract.EXTRA_INITIAL_URI,
            DocumentsContract.buildDocumentUri(SourceImport.SHARED_STORAGE, LogShare.SAVE_FOLDER_DOCUMENT),
        )
}
