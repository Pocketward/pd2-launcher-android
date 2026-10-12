package com.pd2launcher.app

import android.content.Context
import android.os.SystemClock
import android.util.Log
import java.io.FilterInputStream
import java.io.InputStream

/**
 * Debug check of [DeviceCheck] and [UserlandInstaller]:
 * `am start -n com.pd2launcher.app.dev/com.pd2launcher.app.MainActivity --ez unpacktest true [--ez corrupt true]`.
 * Runs the device check, then unpacks the App userland from the APK's assets into
 * files/userland; logs facts, timing and sizes under [TAG] and reports progress to [status].
 * With `corrupt`, one byte of the decompressed stream is flipped and the version is renamed
 * (so the installed one is not taken as complete): the unpack must be refused.
 */
object UnpackTest {
    private const val TAG = "pd2-unpack"
    private const val CORRUPT_OFFSET = 200L shl 20 // late, so a force-stop test can interrupt the run

    private val lock = Any()

    fun run(context: Context, corrupt: Boolean, status: (String) -> Unit) {
        synchronized(lock) {
            try {
                check(context, corrupt, status)
            } catch (error: Exception) {
                Log.e(TAG, "FAIL: $error", error)
                status("Unpack failed: $error")
            }
        }
    }

    private fun check(context: Context, corrupt: Boolean, status: (String) -> Unit) {
        val started = SystemClock.elapsedRealtime()
        val gpu = KgslProbe.probe()
        Log.i(TAG, "kgsl: " + (gpu?.let { "chip_id 0x%08x generation %d name %s".format(it.chipId, it.generation, it.name) } ?: "unavailable"))
        Log.i(TAG, "GL_RENDERER: ${DeviceCheck.glRenderer()}")

        var manifest = UserlandAssets.manifest(context)
        if (corrupt) manifest = manifest.copy(version = manifest.version + "-corrupttest")
        val installer = UserlandInstaller(SessionPaths(DeviceCheck.dataDir(context)).userlandRoot, OwnedProcesses) { Log.i(TAG, it) }
        val required = if (installer.isComplete(manifest.version)) 0 else DeviceCheck.requiredBytes(manifest.fileBytes)
        val facts = DeviceCheck.measure(context, gpu, required)
        Log.i(TAG, "facts: $facts")
        val result = DeviceCheck.evaluate(facts, DeviceCheck::glRenderer)
        result.findings.forEach { Log.i(TAG, "finding ${it.kind}: ${it.message}") }
        Log.i(TAG, "device check ${if (result.passed) "passed" else "refused"} in ${SystemClock.elapsedRealtime() - started} ms")
        if (!result.passed) {
            status("Device check refused: " + result.findings.filter { it.kind.blocks }.joinToString("; ") { it.message })
            return
        }

        Log.i(TAG, "current before: ${installer.currentVersion()}, installing ${manifest.version} " +
            "(${manifest.entries.size} entries, ${manifest.fileBytes} file bytes)")
        status("Preparing ${manifest.version}")
        var lastPercent = -1L
        val unpackStarted = SystemClock.elapsedRealtime()
        val outcome = installer.install(manifest, { openTar(context, corrupt) }) { done, total ->
            val percent = done * 100 / total
            if (percent != lastPercent) {
                lastPercent = percent
                status("Unpacking $percent% (${done shr 20} / ${total shr 20} MiB)")
                if (percent % 10 == 0L) Log.i(TAG, "progress $percent% (${done shr 20} MiB)")
            }
        }
        val elapsed = SystemClock.elapsedRealtime() - unpackStarted
        Log.i(TAG, "install: $outcome in $elapsed ms; current after: ${installer.currentVersion()}")
        if (lastPercent >= 0 && outcome.outcome != InstallResult.Outcome.REFUSED) {
            Log.i(TAG, "unpacked ${manifest.fileBytes} file bytes (${manifest.fileBytes shr 20} MiB) in $elapsed ms")
        }
        status("${outcome.outcome} ${outcome.version} in $elapsed ms ${outcome.detail}".trim())
    }

    private fun openTar(context: Context, corrupt: Boolean): InputStream {
        val tar = UserlandAssets.openTar(context)
        return if (corrupt) FlipByte(tar, CORRUPT_OFFSET) else tar
    }

    /** Flips the lowest bit of the byte at [offset]. */
    private class FlipByte(input: InputStream, private val offset: Long) : FilterInputStream(input) {
        private var position = 0L

        override fun read(): Int {
            val b = super.read()
            if (b >= 0 && position++ == offset) return b xor 1
            return b
        }

        override fun read(buffer: ByteArray, start: Int, length: Int): Int {
            val n = super.read(buffer, start, length)
            if (n > 0 && offset >= position && offset < position + n) {
                val i = start + (offset - position).toInt()
                buffer[i] = (buffer[i].toInt() xor 1).toByte()
                Log.i(TAG, "flipped the byte at decompressed offset $offset")
            }
            if (n > 0) position += n
            return n
        }
    }
}
