package com.pd2launcher.app

import android.content.Context
import android.util.Log
import java.io.File

/** Debug probe of the app's seccomp policy. */
object SeccompProbe {
    private const val TAG = "pd2-seccomp"

    init {
        System.loadLibrary("seccompprobe")
    }

    @JvmStatic
    private external fun run(): String

    /** Runs the probe, logs every line under [TAG] and writes files/seccomp-probe.txt. */
    fun runAndRecord(context: Context) {
        val result = run()
        result.lineSequence().filter { it.isNotEmpty() }.forEach { Log.i(TAG, it) }
        val file = File(context.filesDir, "seccomp-probe.txt")
        file.writeText(result)
        Log.i(TAG, "written to ${file.absolutePath}")
    }
}
