package com.pd2launcher.app

import android.content.Context
import android.os.Build
import android.os.SystemClock
import android.util.Log
import java.util.concurrent.atomic.AtomicBoolean
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update

/**
 * Runs the first-start device check once per app process, off the main thread, before anything
 * is unpacked. The screens follow [state].
 */
object DeviceCheckRun {
    private const val TAG = "pd2-device"
    private const val KEEP_LOGS = 2
    private val started = AtomicBoolean(false)
    private val _state = MutableStateFlow<DeviceCheckState>(DeviceCheckState.Running)
    val state: StateFlow<DeviceCheckState> = _state.asStateFlow()

    /**
     * The check's own log in `files/logs` ([LOG_NAME]; Share logs packs it), for its baseline
     * lines: logcat drops them within a minute on some phones (a phone check of 2026-11-04).
     */
    @Volatile private var file: RotatingLog? = null
    const val LOG_NAME = "device-check"
    private const val LOG_LIMIT = 256L shl 10

    /** One baseline line: in logcat and in the check's own log ([file]). */
    private fun line(text: String) {
        Log.i(TAG, text)
        file?.line(text)
    }

    fun start(context: Context) {
        if (!started.compareAndSet(false, true)) return
        val app = context.applicationContext
        PhoneDriver.init(app)
        PhoneGles.init(app)
        Thread({ _state.value = run(app) }, "pd2-device-check").start()
    }

    /**
     * The player chose Try anyway for a GPU that only the phone's own driver can run
     * ([DeviceCheckResult.tryAnyway]): saved ([AppSettings.phoneDriverAccepted]), and the check
     * passes from now on.
     */
    fun tryAnyway(context: Context) {
        AppSettingsStore.update(context) { it.copy(phoneDriverAccepted = true) }
        val route = (_state.value as? DeviceCheckState.Done)?.facts?.driverOptions?.route
        Log.i(TAG, "the player chose Try anyway: route ${route ?: "unknown"}")
        _state.update { if (it is DeviceCheckState.Done) it.copy(result = it.result.copy(accepted = true)) else it }
    }

    /**
     * The session read the phone's own drivers for the player's pick of the Phone driver: the facts take
     * their reports (read once per process, so this probes nothing again), and Play and the GPU
     * page name the route that the app takes now. Worker thread (the probe loads the vendor's driver).
     */
    fun readPhoneDrivers() {
        val facts = (_state.value as? DeviceCheckState.Done)?.facts ?: return
        // Read already (at the check, or at an earlier pick): no new probe, but the same lines for
        // this pick, so the check's own log has them after each Phone driver pick too.
        if (facts.phoneRead) {
            line("the phone's own drivers for a Phone driver pick: read before")
            line(PhoneGlesDriver.logLine(facts.phoneGles, facts.glServer))
            line(SystemDriverGpu.logLine(facts.phoneDriver, facts.systemDriverServer))
            return
        }
        val phone = HostGpu.phoneDriver(facts.gpu, forced = true)
        val gles = HostGpu.phoneGles(facts.gpu, forced = true)
        Log.i(TAG, "the phone's own drivers (for a Phone driver pick): $phone; $gles")
        line(PhoneGlesDriver.logLine(gles, facts.glServer))
        line(SystemDriverGpu.logLine(phone, facts.systemDriverServer))
        _state.update { if (it is DeviceCheckState.Done) it.copy(facts = it.facts.copy(phoneDriver = phone, phoneGles = gles, phoneRead = true)) else it }
    }

    /**
     * Start over took the installation ([AppSettings.afterStartOver], which forgets Try anyway): the
     * device check's question shows again before the new setup.
     */
    fun startedOver() {
        _state.update { if (it is DeviceCheckState.Done) it.copy(result = it.result.copy(accepted = false)) else it }
    }

    private fun run(context: Context): DeviceCheckState = try {
        val began = SystemClock.elapsedRealtime()
        file = RotatingLog(SessionPaths(DeviceCheck.dataDir(context)).logs, LOG_NAME, LOG_LIMIT, KEEP_LOGS)
        val gpu = HostGpu.probe()
        // The phone's own driver where the built-in one does not know the GPU, where the player
        // picked the Phone driver, or in a debug build (its forced route); its report is read once per process.
        val forced = SystemDriver.forced(context.filesDir)
        val debug = BuiltInDriver.debugRules(context.filesDir)
        val phonePicked = AppSettingsStore.load(context).driverPick == PlayerDriver.PHONE
        val reads = DriverChoice.readsPhoneDriver(forced = forced != null, debug, phonePicked)
        val phone = HostGpu.phoneDriver(gpu, forced = reads)
        // The phone's own OpenGL ES driver for the VirGL route, on the same terms.
        val gles = HostGpu.phoneGles(gpu, forced = DriverChoice.readsPhoneDriver(forced = forced == GraphicsRoute.VIRGL, debug, phonePicked))
        // Read where the built-in driver does not know the GPU, or where a rule above asked for it (HostGpu.phoneDriver).
        val phoneRead = reads || gpu == null || !DeviceCheck.bundledDriverKnows(gpu)
        val manifest = UserlandAssets.manifest(context)
        val sessionPaths = SessionPaths(DeviceCheck.dataDir(context))
        val installer = UserlandInstaller(sessionPaths.userlandRoot, OwnedProcesses)
        val complete = installer.isComplete(manifest.version)
        val required = if (complete) 0 else DeviceCheck.requiredBytes(manifest.fileBytes)
        // An unpacked userland may have left the mode's files out (DriverChoice.skippedFiles); the
        // session's unpack writes them back before the engine starts wherever a route needs them
        // now (DriverChoice.skippableFiles), so they count as there.
        val facts = DeviceCheck.measure(context, gpu, required, manifest.placeholder.length, phone, forcedRoute = forced, phoneGles = gles, phoneRead = phoneRead)
        val evaluated = DeviceCheck.evaluate(facts, DeviceCheck::glRenderer)
            .copy(accepted = AppSettingsStore.load(context).phoneDriverAccepted)
        Log.i(TAG, "facts: $facts")
        line("graphics routes that can run the game: ${facts.driverOptions} -> ${facts.driverOptions.route ?: "none"}")
        line(PhoneGlesDriver.logLine(facts.phoneGles, facts.glServer))
        line(SystemDriverGpu.logLine(facts.phoneDriver, facts.systemDriverServer))
        // The one automatic pick (the owner, 2026-11-04): the default for this GPU on Settings →
        // Display → GPU where none is stored (the first run, an update from 0.3.8) or the GPU is
        // another one (a restore on another phone). The device check runs at each start of the
        // app's process, so the first run and an update take the same path.
        // A failed probe (no GPU) is no other GPU (review MED-1).
        val gpuName = DriverPage.gpuKey(gpu)
        val before = AppSettingsStore.load(context)
        val after = DriverPage.withDefault(before, facts.driverOptions, gpuName)
        if (after != before) {
            // The update from 0.3.8 on the route that 0.3.8 used: no card (the owner, "Skip if unchanged").
            DriverPage.unchangedRoute(before, facts.driverOptions, RouteState.lineRecords(context))?.let {
                RouteState.confirm(context, it)
                line("graphics driver: the update keeps route $it, so no card asks")
            }
            AppSettingsStore.update(context) { DriverPage.withDefault(it, facts.driverOptions, gpuName) }
            DriverPage.defaultLine(gpuName, before, after)?.let(::line)
        }
        // The device row follows the stored pick: a stored pick with no route here stays, and the
        // game then does not use the Software driver (review MED-1).
        val result = DeviceCheck.forPick(evaluated, after.driverPick)
        facts.driverOptions.orderReason?.let { Log.i(TAG, "graphics route order: $it") }
        result.findings.forEach { Log.i(TAG, "finding ${it.kind}: ${it.message}") }
        Log.i(TAG, "device check ${if (result.passed) "passed" else "refused"} in ${SystemClock.elapsedRealtime() - began} ms")
        DeviceCheckState.Done(facts, result, Build.VERSION.RELEASE)
    } catch (error: Exception) {
        Log.e(TAG, "device check failed: $error", error)
        DeviceCheckState.Failed(error.message ?: error.toString())
    }
}
