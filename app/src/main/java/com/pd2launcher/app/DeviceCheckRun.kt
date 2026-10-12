package com.pd2launcher.app

import android.content.Context
import android.os.Build
import android.os.SystemClock
import android.util.Log
import java.util.concurrent.atomic.AtomicBoolean
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

/**
 * Runs the first-start device check once per app process, off the main thread, before anything
 * is unpacked. The screens follow [state].
 */
object DeviceCheckRun {
    private const val TAG = "pd2-device"
    private val started = AtomicBoolean(false)
    private val _state = MutableStateFlow<DeviceCheckState>(DeviceCheckState.Running)
    val state: StateFlow<DeviceCheckState> = _state.asStateFlow()

    fun start(context: Context) {
        if (!started.compareAndSet(false, true)) return
        val app = context.applicationContext
        Thread({ _state.value = run(app) }, "pd2-device-check").start()
    }

    private fun run(context: Context): DeviceCheckState = try {
        val began = SystemClock.elapsedRealtime()
        val gpu = HostGpu.probe()
        val manifest = UserlandAssets.manifest(context)
        val installer = UserlandInstaller(SessionPaths(DeviceCheck.dataDir(context)).userlandRoot, OwnedProcesses)
        val required = if (installer.isComplete(manifest.version)) 0 else DeviceCheck.requiredBytes(manifest.fileBytes)
        val facts = DeviceCheck.measure(context, gpu, required, manifest.placeholder.length)
        val result = DeviceCheck.evaluate(facts, DeviceCheck::glRenderer)
        Log.i(TAG, "facts: $facts")
        result.findings.forEach { Log.i(TAG, "finding ${it.kind}: ${it.message}") }
        Log.i(TAG, "device check ${if (result.passed) "passed" else "refused"} in ${SystemClock.elapsedRealtime() - began} ms")
        DeviceCheckState.Done(facts, result, Build.VERSION.RELEASE)
    } catch (error: Exception) {
        Log.e(TAG, "device check failed: $error", error)
        DeviceCheckState.Failed(error.message ?: error.toString())
    }
}
