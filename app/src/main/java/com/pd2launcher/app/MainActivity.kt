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
import androidx.compose.runtime.produceState
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
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
import com.pd2launcher.app.input.ControllerLog
import com.pd2launcher.app.input.PadButton
import com.pd2launcher.app.input.PadLayouts
import com.pd2launcher.app.ui.AboutScreen
import com.pd2launcher.app.ui.ControlsEditor
import com.pd2launcher.app.ui.rememberImportRecord
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
import com.pd2launcher.app.ui.navigationKey
import com.pd2launcher.app.ui.LocalRefusal
import com.pd2launcher.app.ui.NotSupportedScreen
import com.pd2launcher.app.ui.Play
import com.pd2launcher.app.ui.PlayDestinations
import com.pd2launcher.app.ui.PlayScreen
import com.pd2launcher.app.ui.NewVersionQuestion
import com.pd2launcher.app.ui.playCheckWaiting
import com.pd2launcher.app.ui.RefusalNotice
import com.pd2launcher.app.ui.RepairScreen
import com.pd2launcher.app.ui.rememberImported
import com.pd2launcher.app.ui.rememberInstallerDownloads
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
    /** The CD key the engine refused at Install, shown under its field on the key page ([KeyProblems]). */
    private val keyProblem = mutableStateOf<KeyProblem?>(null)
    /** The `seq` of the last key refusal shown, so it opens the key page once. */
    private var keyRefusalShown = -1L
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
    /** Each controller as it connects, beside the app's other logs (Share logs packs them): what a player's controller sends. */
    private val controllerLog by lazy {
        val log = RotatingLog(paths.logs, "controllers")
        ControllerLog(this) { line ->
            Log.i(CONTROLLERS_TAG, line)
            log.line(line)
        }
    }
    /** Save logs: Android's "Save as" screen for the zip; [saveChosen] copies it where the player chose. */
    private val saveLogsTo = registerForActivityResult(SaveInDownload(), ::saveChosen)
    /**
     * The source step's folder picker; the picked folder's copy starts ([SourceImport.copyFolder]).
     * Registered with the activity, not in Root, so a pick that comes back after Android made the
     * activity again still arrives.
     */
    private val pickFolder = registerForActivityResult(ActivityResultContracts.OpenDocumentTree()) { tree ->
        if (tree != null) SourceImport.copyFolder(this, tree)
    }
    /** Which layout the launcher screens use, from the display's shape (ui/FormFactor.kt). */
    private lateinit var formFactor: FormFactor
    private val batteryRestricted = mutableStateOf(false)
    /** App updates: About's group, Settings → App's section and Android's window ([AppUpdateHost]). */
    private val appUpdate = AppUpdateHost(this)
    private var sessionStarted = false
    /**
     * Back on the first screen (Play, or Repair in its place) closes the app: Android moves it to
     * the back, and the idle session ends at once, not after the short wait of Home
     * ([SessionIdle]). Until the next onStart.
     */
    private var closing = false
    private val paths by lazy { SessionPaths(DeviceCheck.dataDir(this)) }
    /** The debug entry points below ([readScreenExtras], [startSession], the tests in onCreate); never in a release build. */
    private val debuggable get() = BuildConfig.DEBUG

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        // Each controller's layout is read as it connects, for the life of the process.
        PadLayouts.watch(this)
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
        // About stays open when Android restarted the app under it (allowing app updates can);
        // a new start from the notice of a new app version opens it.
        aboutOpen.value = savedInstanceState?.getBoolean(STATE_ABOUT) ?: intent.getBooleanExtra(AppUpdater.EXTRA_ABOUT, false)
        // A debug intent's settings and Play act once: not again when Android makes the activity again.
        freshIntent = savedInstanceState == null
        readScreenExtras(intent)
        AppSettingsStore.load(this)
        appUpdate.create(intent, fresh = savedInstanceState == null)
        // An install started and not finished opens on Install; an installed game on Play (Root
        // follows the record, so it loads on every start, before the page is chosen). When Android
        // makes the activity again (for example, the process ended under the folder picker), the
        // first run's page and source choice come back: the picked folder's copy shows, not Check
        // and the default source.
        val record = SetupRecordStore.load(this)
        nav.value = FlowNav.start(savedInstanceState?.getStringArrayList(STATE_NAV), record)
        sourceChoice.value = SourceChoice.named(savedInstanceState?.getString(STATE_SOURCE))
        keyRefusalShown = savedInstanceState?.getLong(STATE_KEY_REFUSAL, -1L) ?: -1L
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
                // The engine's refusal of the last command shows on the screen it came from; a
                // refused CD key shows under its field on the key page instead.
                val refusal = SessionService.status.collectAsState().value.refusal?.takeIf { KeyProblems.of(it.reason) == null }
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
        // After an update in place, until its first Ready: Repair for any audit short of Ready (Use your game).
        val updatedInPlace = remember(status.snapshot, record) { File(paths.managedRoot, LegacyPrefix.UPDATED).exists() && paths.managedGame.isDirectory }
        val installed = if (record == SetupRecord.INSTALLED) Onboarding.installed(device, status, updatedInPlace) else null
        LaunchedEffect(record) {
            if (record == SetupRecord.NONE) repairOpen.value = false
            // An update in place that failed while the app showed Play: Install, as on a start.
            if (record == SetupRecord.STARTED && nav.value.page == FlowPage.CHECK) nav.value = FlowNav.opening(record)
        }
        // While the Game client runs the Game view stays; it ends when the game exits.
        val playing = play == Play.Mode.GAME
        BackHandler(enabled = playing) {}
        // The running X screen's size, for the game resolution's note in Settings: read as Settings opens.
        val runningScreen by produceState<Pair<Int, Int>?>(null, framebuffer, settingsOpen.value) {
            value = framebuffer?.let { withContext(Dispatchers.IO) { it.header() } }?.let { it.width to it.height }
        }
        val gamePreview = DebugPreview.gameLayer(previewName)
        val showGameView = (gameView.value && debuggable) || playing || gamePreview != null
        BackHandler(enabled = showGameView && !playing && gamePreview == null) { gameView.value = false }
        // The controls editor lays them out on the game's landscape screen, without system bars.
        val gameScreen = showGameView || editorOpen.value
        // Settings → Keep the screen on during play, followed live (the in-game menu switches it too).
        val keepOn = showGameView && AppSettingsStore.current.collectAsState().value.keepScreenOn
        DisposableEffect(keepOn) {
            if (keepOn) window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
            onDispose { if (keepOn) window.clearFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON) }
        }
        DisposableEffect(gameScreen) {
            val bars = WindowCompat.getInsetsController(window, window.decorView)
            if (gameScreen) {
                bars.systemBarsBehavior = WindowInsetsControllerCompat.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
                bars.hide(WindowInsetsCompat.Type.systemBars())
                // The game's X screen is landscape on every device, phones included, however held.
                requestedOrientation = ActivityInfo.SCREEN_ORIENTATION_SENSOR_LANDSCAPE
            }
            onDispose {
                if (gameScreen) {
                    bars.show(WindowInsetsCompat.Type.systemBars())
                    requestedOrientation = formFactor.launcherOrientation
                }
            }
        }
        val showKeys = keys.value && debuggable
        BackHandler(enabled = showKeys) { keys.value = false }
        val sourceCopy by SourceImport.copy.collectAsState()

        val back = { nav.value = nav.value.back() }
        // An import never installs or updates Project Diablo 2: Repair instead (InstallProgress.page).
        val importRecord by rememberImportRecord(status.snapshot?.step to status.snapshot?.installation)
        val imported = importRecord == true
        // The download finished while Download was shown: Install follows.
        val installReady = InstallProgress.page(status, imported) != null
        LaunchedEffect(installReady) { if (installReady) nav.value = nav.value.downloadDone() }
        // Start over reset the interrupted stage: straight to the keys, or Install's start button.
        val installPage = InstallProgress.page(status, imported)
        LaunchedEffect(installPage) {
            val snapshot = status.snapshot
            if (startedOver.value && installPage == InstallProgress.Page.KEYS && snapshot != null) {
                startedOver.value = false
                nav.value = nav.value.afterStartOver(needsKeys = !DiabloSource.importsExisting(snapshot))
            }
        }
        // The engine refused a CD key at Install: back to the key page, with why under the field.
        val refusal = status.refusal
        LaunchedEffect(refusal) {
            val problem = refusal?.let { KeyProblems.of(it.reason) } ?: return@LaunchedEffect
            if (refusal.seq == keyRefusalShown) return@LaunchedEffect
            keyRefusalShown = refusal.seq
            keyProblem.value = problem
            // No install started with the refused keys, also when they were to join Fast download's own download.
            installRun.value = null
            nav.value = nav.value.open(FlowPage.KEYS)
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
            // The row the player last selected, kept over a restart (AppSettings.sourceChoice).
            val saved = AppSettingsStore.current.collectAsState().value.sourceChoice
            val choice = DiabloSource.pageChoice(sourceChoice.value, status.snapshot, saved)
            val view = DiabloSource.view(choice, status, sourceCopy)
            SourceScreen(
                view,
                onSelect = {
                    sourceChoice.value = it
                    if (onPreviewContinue == null) AppSettingsStore.sourceChosen(this@MainActivity, it)
                },
                onPickFolder = { pickFolder.launch(null) },
                onContinue = {
                    if (view.continuePicks) {
                        pickFolder.launch(null)
                    } else if (onPreviewContinue != null) {
                        onPreviewContinue()
                    } else {
                        // A new start: an install run of an earlier source is over.
                        installRun.value = null
                        // No confirmation page: the download starts as Download shows. The install
                        // source goes first; Install Diablo II reads it when it starts.
                        val setting = DiabloSource.sourceSetting(choice, status.snapshot?.settings?.installSource)
                        setting?.let { SessionService.command(it) }
                        val select = DiabloSource.continueCommand(choice, status.snapshot?.installer)
                        select?.let { SessionService.command(it) }
                        // A new source makes what was downloaded or installed for the old one stale.
                        val ready = installReady && select == null
                        // The engine still holds keys that joined a Download that failed or stopped:
                        // Download runs again with the game environment, and Install Diablo II
                        // follows it by itself with those keys (as Install's Try again does).
                        val joins = KeysWhileDownloading.continueJoins(choice, status.snapshot, sourceChanges = setting != null || select != null)
                        if (joins) {
                            beginInstall(status, joinsDownload = true)
                            SessionService.command(Command.DownloadAll, withGameEnvironment = true)
                        } else if (!ready && Onboarding.startsDownload(status, sourceChanges = select != null)) {
                            SessionService.command(Command.DownloadAll)
                        }
                        // A download of Diablo II: the key page at once, while Download gets the
                        // installers ready (KeysWhileDownloading); a folder goes on as before.
                        nav.value = when {
                            joins -> nav.value.open(FlowPage.INSTALL)
                            KeysWhileDownloading.keysAtOnce(choice, status.snapshot) -> nav.value.keysWhileDownloading()
                            else -> nav.value.afterSource(ready)
                        }
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
            AboutScreen(
                onBack = { aboutOpen.value = false },
                runtime = (status.userland as? SessionStatus.Userland.Ready)?.version,
                update = appUpdate.about(status, sourceCopy is SourceCopy.Copying),
            )
        } else if (editorOpen.value) {
            ControlsEditor(sample = true, onClose = { editorOpen.value = false })
        } else if (showGameView && !settingsOpen.value) {
            // Settings, when open (the in-game menu has no way there now), show over the running game; back returns to it.
            GameView(framebuffer, status.gameEnvironment, presentCounter = paths.presentCounter, preview = gamePreview, previewLauncher = DebugPreview.gameLauncher(previewName))
        } else if (settingsOpen.value) {
            val app by AppSettingsStore.current.collectAsState()
            // Live: the Frame rate input measures it again while it shows ([ScreenRateProbe]).
            remember { ScreenRate.load(this) }
            val fullFrameRate by ScreenRate.full.collectAsState()
            SettingsScreen(
                launcher = LauncherSettings.view(status.snapshot?.settings, status.engine == SessionStatus.Engine.CONNECTED),
                app = app,
                display = remember { GameResolution.display(this) },
                fullFrameRate = fullFrameRate,
                // The device check's probe, not another on the main thread (its ioctls) each time.
                gpu = (device as? DeviceCheckState.Done)?.facts?.gpu,
                system = SystemState(batteryRestricted.value, notificationsOff.value),
                nav = settingsNav,
                back = Onboarding.settingsBack(showGameView, previewName == DebugPreview.REPAIR || repairOpen.value || installed == Installed.Repair),
                onCommand = { SessionService.command(it) },
                onAppChange = { AppSettingsStore.update(this, it) },
                onBatterySettings = { openSystemSettings(Intent(Settings.ACTION_IGNORE_BATTERY_OPTIMIZATION_SETTINGS)) },
                onNotificationSettings = ::openNotificationSettings,
                onEditOnScreen = { editorOpen.value = true },
                onAbout = { aboutOpen.value = true },
                onBack = { settingsOpen.value = false },
                updates = appUpdate.available,
                newVersion = appUpdate.newVersion(),
                // The app's own Play and the official launcher (and a game it starts) use the game resolution.
                running = runningScreen.takeIf { playing },
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
            // As where the app holds Repair on screen: with Settings and the logs.
            RepairScreen(status, onCheckAgain = {}, onStartOver = {}, onBack = leavePreview, onSettings = { openSettings(SettingsNav.GAME) })
        } else if (previewName == DebugPreview.PLAY || DebugPreview.notice(previewName) != null || DebugPreview.playUpdate(previewName, status) != null) Box(Modifier.fillMaxSize()) {
            val shown = DebugPreview.playUpdate(previewName, status) ?: status
            PlayScreen(
                facts = Play.facts(shown.snapshot, (device as? DeviceCheckState.Done)?.facts?.gpu?.name),
                problem = null,
                onPlay = {},
                destinations = PlayDestinations({ openSettings(SettingsNav.GAME) }, { openSettings(SettingsNav.CONTROLLER) }, {}, { aboutOpen.value = true }),
                update = Play.update(shown),
                newVersion = appUpdate.newVersionDot(),
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
                // is none: Back leaves the app, and Settings and the logs are on Repair itself.
                onBack = {
                    if (installed == Installed.Repair) {
                        closing = true
                        moveTaskToBack(true)
                    } else {
                        repairOpen.value = false
                    }
                },
                onSettings = { openSettings(SettingsNav.GAME) }.takeIf { Onboarding.repairLinks(record) },
                // After an update in place whose moved game the engine rejected: Import of it first.
                onImport = {
                    repairOpen.value = false
                    SessionService.send(this, SessionService.ACTION_IMPORT_UPDATED_GAME)
                }.takeIf {
                    remember(status.snapshot, record) {
                        UpgradeImport.offersImport(
                            status.snapshot, record,
                            updated = File(paths.managedRoot, LegacyPrefix.UPDATED).exists(),
                            gameThere = paths.managedGame.isDirectory,
                        )
                    }
                },
            )
        } else if (installed is Installed.Refused) {
            NotSupportedScreen(installed.screen, onClose = ::finish)
        } else if (installed is Installed.Play || play == Play.Mode.PLAY) Box(Modifier.fillMaxSize()) {
            // Back on Play closes the app (the notice below takes Back first while it shows).
            BackHandler {
                closing = true
                moveTaskToBack(true)
            }
            val pressed = {
                SessionService.dismissRefusal()
                launchPress.value = Play.LaunchPress(SystemClock.elapsedRealtime(), status.gameEnvironmentFailure)
            }
            PlayScreen(
                // The engine's placeholder before its audit names no renderer or update yet.
                facts = Play.facts(status.snapshot?.takeIf(Onboarding::audited), (device as? DeviceCheckState.Done)?.facts?.gpu?.name),
                problem = (installed as? Installed.Play)?.problem?.let { text(it) }
                    ?: status.gameEnvironmentFailure ?: Play.officialLauncherProblem(status)?.let { getString(it) },
                // Play's look for a new version says what it does, over the engine's own audit
                // under it ("Getting ready…").
                waiting = playCheckWaiting(status.playCheck)?.let { getString(it) }
                    ?: (installed as? Installed.Play)?.waiting?.let { text(it) },
                onPlay = {
                    // With the checks on, Play's check holds the buttons ("Checking for updates…",
                    // the question, the start), and Cancel gives them back at once.
                    if (PlayCheck.checks(status.snapshot)) SessionService.dismissRefusal() else pressed()
                    SessionService.send(this@MainActivity, SessionService.ACTION_PLAY)
                },
                onPlayWithoutUpdating = {
                    pressed()
                    SessionService.send(this@MainActivity, SessionService.ACTION_PLAY_WITHOUT_CHECK)
                },
                playAfterUpdate = status.playCheck is PlayCheck.State.Updating,
                // In the engine alone: no game environment. Check files (Install Project Diablo 2) can
                // fall back to the PD2 installer: it starts as Install's.
                update = Play.update(status),
                onCommand = {
                    if (Play.holdsButtons(it)) pressed()
                    SessionService.command(it, withGameEnvironment = it is Command.InstallProjectDiablo)
                },
                onOfficialLauncher = {
                    // In the game environment, as the game: the Game view shows it once its session runs.
                    pressed()
                    SessionService.send(this@MainActivity, SessionService.ACTION_OFFICIAL_LAUNCHER)
                }.takeIf { Play.officialLauncherEnabled(status) },
                // Also after Play's check, until the game shows.
                launching = launching || status.playCheck is PlayCheck.State.Starting,
                onTryAgain = { SessionService.send(this@MainActivity, SessionService.ACTION_RETRY) }.takeIf { Onboarding.failed(status) },
                destinations = PlayDestinations(
                    onSettings = { openSettings(SettingsNav.GAME) },
                    onControls = { openSettings(SettingsNav.CONTROLLER) },
                    onRepair = { repairOpen.value = true },
                    onAbout = { aboutOpen.value = true },
                ),
                newVersion = appUpdate.newVersionDot(),
            )
            // Once, after the first game session (never right after the install): the installer
            // downloads can go and, after an import from Android's own storage, so can the Diablo II
            // folder the player copied onto the device (after an import from Lite, Lite keeps its copy).
            val app by AppSettingsStore.current.collectAsState()
            val downloadsNow by rememberInstallerDownloads(status.snapshot?.installation)
            val importedNow by rememberImported(status.snapshot?.installation, status.snapshot?.gameFolder?.path)
            val downloads = downloadsNow
            val imported = importedNow
            if (downloads != null && imported != null &&
                InstallerDownloads.showNotice(status.snapshot, app.downloadsNoticeAnswered, app.gamePlayed, downloads.bytes, imported)
            ) {
                val answer = { AppSettingsStore.update(this@MainActivity) { it.copy(downloadsNoticeAnswered = true) } }
                DownloadsNotice(
                    InstallerDownloads.notice(
                        downloads.bytes, imported, remember { SourceImport.pickedFolder(this@MainActivity) },
                        remember { SourceImport.pickedFrom(this@MainActivity) }, clientOnly = downloads.clientOnly,
                    ),
                    onRemove = {
                        answer()
                        SessionService.removeInstallerDownloads { Log.i(TAG, "installer downloads from the notice: $it") }
                    },
                    onLater = { answer() },
                )
            }
            // Play found a new version: Update (the game starts when it is done) or Cancel.
            if (status.playCheck == PlayCheck.State.Found) {
                NewVersionQuestion(
                    onUpdate = {
                        pressed()
                        SessionService.answerNewVersion(update = true)
                    },
                    onCancel = { SessionService.answerNewVersion(update = false) },
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
                        if (KeysWhileDownloading.keysWait(status.snapshot)) {
                            // Keys wait in the engine (a Download they joined stopped or failed):
                            // Install Diablo II follows Download by itself, so it runs with the
                            // game environment, and Install shows it.
                            beginInstall(status, joinsDownload = true)
                            SessionService.command(Command.DownloadAll, withGameEnvironment = true)
                            nav.value = nav.value.open(FlowPage.INSTALL)
                        } else {
                            if (status.snapshot?.installer == Installer.NotSelected) {
                                SessionService.send(this, SessionService.ACTION_USE_BLIZZARD_DOWNLOAD)
                            }
                            SessionService.send(this, SessionService.ACTION_DOWNLOAD)
                        }
                    },
                    onStopSafely = { SessionService.send(this, SessionService.ACTION_STOP_SAFELY) },
                    onContinue = { nav.value = nav.value.open(FlowPage.INSTALL) },
                    onBack = if (downloading) null else back,
                )
            }
            // An interrupted stage takes no keys and no install until Start over (Repair).
            Screen.Install, Screen.Keys -> if (InstallProgress.page(status, imported) == InstallProgress.Page.REPAIR) {
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
            } else {
                val install = InstallProgress.view(status)
                // The engine's key check failed at the start of Install Diablo II: under the field too.
                val failedKey = KeyProblems.of(status.snapshot?.operation?.failure).takeIf { install.state == InstallState.FAILED }
                val refused = keyProblem.value
                // The Download line last shown (KeysWhileDownloading.hold).
                val downloadLine = remember { arrayOfNulls<Line>(1) }
                KeysScreen(
                    onAuthorize = { d2Key, lodKey ->
                        keyProblem.value = null
                        // The running Install shows at once; the game environment starts first.
                        // After a Download that failed or stopped, Download runs again first and
                        // the keys join it.
                        if (KeysWhileDownloading.retries(status)) {
                            beginInstall(status, joinsDownload = true)
                            SessionService.authorizeWithDownload(d2Key, lodKey)
                        } else {
                            beginInstall(status)
                            SessionService.authorize(d2Key, lodKey)
                        }
                        nav.value = nav.value.installStarted()
                    },
                    onBack = {
                        keyProblem.value = null
                        back()
                    },
                    problem = install.problem?.takeIf { failedKey == null }?.let { text(it) },
                    // A paused Install Diablo II whose keys went with the app: they continue it.
                    resuming = install.state == InstallState.STOPPED,
                    keyProblem = refused ?: failedKey,
                    // Only the keys of this refusal go back into the fields.
                    refill = SessionService::takeRefusedKeys.takeIf { refused != null },
                    // Download, which gets the installers ready while the keys are typed.
                    download = KeysWhileDownloading.hold(downloadLine[0], KeysWhileDownloading.line(status)).also { downloadLine[0] = it },
                    onRetryDownload = {
                        if (startDownload(status)) nav.value = nav.value.installStarted()
                    }.takeIf { KeysWhileDownloading.retries(status) },
                )
                // Nothing started Download in this engine's run (the app was closed since Continue):
                // the key page starts it, after a moment for Continue's own start to show.
                val current by rememberUpdatedState(status)
                val starts = KeysWhileDownloading.startsOnKeyPage(status)
                LaunchedEffect(starts) {
                    if (!starts) return@LaunchedEffect
                    delay(KeysWhileDownloading.START_WAIT_MS)
                    if (KeysWhileDownloading.startsOnKeyPage(current) && startDownload(current)) nav.value = nav.value.installStarted()
                }
            }
        }
    }

    /**
     * Install's start: Install Project Diablo 2 when Diablo II is in, an installed Diablo II is
     * imported, otherwise the key page asks for the CD keys first (its "Agree and install" starts).
     */
    private fun startInstall(status: SessionStatus) {
        val snapshot = status.snapshot ?: return
        // An import never installs or updates Project Diablo 2 (the owner's rule of 2026-10-26).
        if (snapshot.step == Step.INSTALL_PD2 && InstallerDownloads.imported(SessionPaths(DeviceCheck.dataDir(this)).managedRoot)) return
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
            // The engine holds the keys: Download runs again, and the install follows it by itself.
            InstallProgress.StartAction.DOWNLOAD_AGAIN -> {
                beginInstall(status, joinsDownload = true)
                SessionService.command(Command.DownloadAll, withGameEnvironment = true)
            }
        }
    }

    /**
     * Download from the key page: with keys waiting in the engine ([KeysWhileDownloading.keysWait]),
     * Install Diablo II follows it by itself, so it runs with the game environment and the run
     * joins it (true: Install shows it); else a plain Download (false).
     */
    private fun startDownload(status: SessionStatus): Boolean {
        if (!KeysWhileDownloading.keysWait(status.snapshot)) {
            SessionService.command(Command.DownloadAll)
            return false
        }
        beginInstall(status, joinsDownload = true)
        SessionService.command(Command.DownloadAll, withGameEnvironment = true)
        return true
    }

    private fun beginInstall(status: SessionStatus, joinsDownload: Boolean = status.snapshot?.activeJob == com.pd2launcher.app.control.Job.DOWNLOAD) {
        installRun.value = InstallProgress.start(installSource(status), status, SystemClock.elapsedRealtime(), joinsDownload)
    }

    private fun trackInstall() {
        val status = SessionService.status.value
        val next = InstallProgress.track(installRun.value, status, installSource(status), SystemClock.elapsedRealtime())
        if (next != installRun.value) installRun.value = next
    }

    /** The source the engine has, else the one on screen. */
    private fun installSource(status: SessionStatus): SourceChoice =
        status.snapshot?.let(DiabloSource::choiceOf) ?: sourceChoice.value ?: SourceChoice.DEFAULT

    /** The task names for the player's source (an installed Diablo II is copied or adopted). */
    private fun taskLabels(status: SessionStatus): InstallProgress.TaskLabels {
        val existing = status.snapshot?.installer as? Installer.Existing
        return InstallProgress.TaskLabels(
            source = installSource(status),
            inPlace = existing?.inPlacePossible == true,
            // The keys joined Download: its rest is the first task.
            installers = installRun.value?.joined == true,
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
                    SessionService.checkpointLogs()
                    val launcher = officialLauncherFolder()
                    LogShare.write(cacheDir, paths, aboutDevice(outcome, launcher), (outcome as? LogShare.ReportOutcome.Saved)?.file, launcher)
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

    /** The official PD2 launcher's folder (ProjectD2) in the game folder in use; null without a game folder. */
    private fun officialLauncherFolder(): File? =
        GameFilesProvider.gameFolder(paths, SessionService.status.value.snapshot?.gameFolder?.path)?.let { File(it, LauncherFiles.FOLDER) }

    /**
     * `device.txt`: this device, the app, where the installation stands, the official PD2
     * launcher's program in [launcher] (its folder) and how the wait for the engine's [report]
     * ended. Free space is measured now, not at the device check.
     */
    private fun aboutDevice(report: LogShare.ReportOutcome, launcher: File?): String {
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
                // Android 11 names no chip: its hardware name (Build.HARDWARE) instead.
                "Chip" to if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) "${Build.SOC_MANUFACTURER} ${Build.SOC_MODEL}" else "hardware ${Build.HARDWARE}",
                "Android" to "${Build.VERSION.RELEASE} (API ${Build.VERSION.SDK_INT}), ${Build.SUPPORTED_ABIS.joinToString(" ")}",
                "GPU" to (facts?.gpu?.name ?: "not found"),
                // Turnip's TP UBWC flag hint: on an Adreno 740, from the phone's Vulkan driver.
                "UBWC flag hint" to UbwcFlagHint.choice(facts?.gpu).describe(),
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
                "Official launcher" to (
                    launcher?.let { "${LauncherFiles.LAUNCHER} ${LauncherFiles.describe(LauncherFiles.launcher(it))}" }
                        ?: "no game folder"
                    ),
                "Engine report" to LogShare.describe(report),
                "CPU" to HardwareFacts.cpuOfDevice(),
                "Memory" to HardwareFacts.memoryOfDevice(),
                "Time zone" to HardwareFacts.timeZone(java.util.TimeZone.getDefault(), System.currentTimeMillis()),
                "Runtime" to BuildConfig.RUNTIME_PARTS,
                "Screen" to screenFacts(),
                "Thermal state" to thermalState(),
                "Settings" to SettingsLog.line(
                    AppSettingsStore.load(this),
                    LauncherSettings.view(snapshot?.settings, status.engine == SessionStatus.Engine.CONNECTED),
                ),
                "Detailed logging" to if (AppSettingsStore.current.value.detailedLogging) "on" else "off",
            ),
        )
    }

    /** The screen for device.txt: the whole panel, the app's window, the density, the rate now and the full rate the app measured. */
    private fun screenFacts(): String {
        val panel = windowManager.maximumWindowMetrics.bounds
        val window = resources.displayMetrics
        return HardwareFacts.screen(
            panel = panel.width() to panel.height(),
            window = window.widthPixels to window.heightPixels,
            densityDpi = window.densityDpi,
            hertzNow = display?.refreshRate,
            fullHertz = ScreenRate.full.value,
        )
    }

    /** Android's thermal status (a throttled CPU is slower) and power saving, for device.txt. */
    private fun thermalState(): String {
        val power = getSystemService(PowerManager::class.java) ?: return "unknown"
        val thermal = when (power.currentThermalStatus) {
            PowerManager.THERMAL_STATUS_NONE -> "none"
            PowerManager.THERMAL_STATUS_LIGHT -> "light"
            PowerManager.THERMAL_STATUS_MODERATE -> "moderate"
            PowerManager.THERMAL_STATUS_SEVERE -> "severe"
            PowerManager.THERMAL_STATUS_CRITICAL -> "critical"
            PowerManager.THERMAL_STATUS_EMERGENCY -> "emergency"
            PowerManager.THERMAL_STATUS_SHUTDOWN -> "shutdown"
            else -> "level ${power.currentThermalStatus}"
        }
        return "$thermal; power saving ${if (power.isPowerSaveMode) "on" else "off"}"
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        setIntent(intent)
        // The notice of a new app version opens About, over whatever shows.
        if (intent.getBooleanExtra(AppUpdater.EXTRA_ABOUT, false)) aboutOpen.value = true
        freshIntent = true
        readScreenExtras(intent)
        appUpdate.newIntent(intent)
        if (sessionStarted) startSession(intent)
    }

    override fun onSaveInstanceState(outState: Bundle) {
        super.onSaveInstanceState(outState)
        outState.putBoolean(STATE_ABOUT, aboutOpen.value)
        outState.putStringArrayList(STATE_NAV, nav.value.saved())
        sourceChoice.value?.let { outState.putString(STATE_SOURCE, it.name) }
        outState.putLong(STATE_KEY_REFUSAL, keyRefusalShown)
    }

    override fun onStart() {
        super.onStart()
        closing = false
        controllerLog.start()
        SessionService.appForeground(true)
        // The session ends by itself while nothing runs and the app is closed; back in front, it starts again.
        if (sessionStarted) SessionService.resume(this)
    }

    override fun onStop() {
        super.onStop()
        controllerLog.stop()
        // Closed (Back on the first screen, or the activity ends), or only left (Home, the lock, another app).
        SessionService.appForeground(false, closed = closing || isFinishing)
    }

    override fun onDestroy() {
        // Swiped away from the recent apps after Home: closed now.
        if (isFinishing) SessionService.appClosed()
        super.onDestroy()
    }

    override fun onResume() {
        super.onResume()
        notificationsOff.value = !getSystemService(NotificationManager::class.java).areNotificationsEnabled()
        batteryRestricted.value = !getSystemService(PowerManager::class.java).isIgnoringBatteryOptimizations(packageName)
        appUpdate.resumed()
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
    override fun dispatchKeyEvent(event: KeyEvent): Boolean = super.dispatchKeyEvent(navigationKey(event))

    /**
     * Debug builds only: `--es preview <name>` (`--ei tab <n>` and `--es picker <button>` with
     * `settings`, `--ez mouse true` for mouse mode), `--ez gameview true` and `--ez keys true`;
     * Settings → Game without taps ([debugGameSettings]): `--es display fit|center` and
     * `--es resolution own|<width>x<height>`.
     */
    private fun readScreenExtras(intent: Intent) {
        if (!debuggable) return
        if (freshIntent) debugGameSettings(intent)
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

    /**
     * Debug builds: `--es display fit|center` sets Display (at once, also over a running game) and
     * `--es resolution own|<width>x<height>` the game resolution (the game's own size or a Custom
     * size the display allows; from the next Play), as Settings would. Each logs one line.
     */
    private fun debugGameSettings(intent: Intent) {
        intent.getStringExtra("display")?.let { name ->
            val display = GameDisplay.entries.firstOrNull { it.name.equals(name, ignoreCase = true) }
            if (display != null) AppSettingsStore.update(this) { it.copy(gameDisplay = display) }
            Log.i(TAG, "debug display $name: ${display ?: "unknown, not changed"}")
        }
        intent.getStringExtra("resolution")?.let { value ->
            val (width, height) = GameResolution.display(this)
            val size = if (value == "own") GameResolution.ownSize(width, height) else SessionService.parseSize(value)
            if (size != null && (value == "own" || GameResolution.customAllowed(size, width, height))) {
                AppSettingsStore.update(this) { it.copy(customSize = GameResolution.customPick(size, width, height)) }
                Log.i(TAG, "debug resolution $value: ${size.first}x${size.second} from the next Play")
            } else {
                Log.i(TAG, "debug resolution $value: not allowed on ${width}x$height, not changed")
            }
        }
    }

    /** The intent is new (not the one of an activity Android made again): its debug settings and Play act. */
    private var freshIntent = true

    private fun startSessionOnce() {
        if (sessionStarted) return
        sessionStarted = true
        startSession(intent)
    }

    /**
     * Starts the session service. Debug builds also take
     * `--es session game-start|game-stop` to start or stop the Android game environment,
     * `--es session play` for Play as its button sends it, and
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
            "play" -> if (freshIntent) SessionService.ACTION_PLAY else SessionService.ACTION_START
            "install-pd2" -> SessionService.ACTION_INSTALL_PD2
            "download" -> SessionService.ACTION_DOWNLOAD
            else -> SessionService.ACTION_START
        }
        SessionService.send(this, action, if (debuggable) intent.getStringExtra("xscreen") else null)
    }

    private companion object {
        const val TAG = "pd2-session"
        /** The controllers log's lines in logcat too ([controllerLog]). */
        const val CONTROLLERS_TAG = "pd2-controllers"
        /** How long Share logs and Save logs wait for the engine's report before they go on without one. */
        const val REPORT_WAIT_MS = 20_000L
        /** Whether About was open, kept when Android ends the app under it. */
        const val STATE_ABOUT = "about_open"
        /** The first run's pages and the source choice, kept when Android makes the activity again (for example, the process ended under the folder picker). */
        const val STATE_NAV = "flow_nav"
        const val STATE_SOURCE = "source_choice"
        /** The key refusal already shown ([keyRefusalShown]). */
        const val STATE_KEY_REFUSAL = "key_refusal_shown"
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
