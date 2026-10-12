package com.pd2launcher.app

import android.content.Context
import android.os.ParcelFileDescriptor
import android.system.Os
import android.system.OsConstants
import android.system.StructTimeval
import android.util.Log
import java.io.File
import java.io.FileInputStream
import java.io.FileOutputStream
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit

/**
 * Debug check of [Spawner]: `am start -n com.pd2launcher.app.dev/com.pd2launcher.app.MainActivity --ez spawntest true`.
 * A shell gets one end of a socket pair as standard input and a log file as standard
 * output and error, lists its descriptors,
 * answers one line over the socket and exits 3. Results go to logcat under [TAG].
 */
object SpawnTest {
    private const val TAG = "pd2-spawntest"
    private const val SCRIPT =
        "ls -l /proc/self/fd; echo \"sh fds:\"; ls -l /proc/\$\$/fd; " +
            "read l; echo \"got:\$l\" >&0; echo \"got:\$l\"; exit 3"

    fun run(context: Context) {
        try {
            check(context)
        } catch (error: Exception) {
            Log.e(TAG, "FAIL: $error", error)
        }
    }

    private fun check(context: Context) {
        val logs = File(context.filesDir, "logs").apply { mkdirs() }
        val stdout = File(logs, "spawntest.out").apply { delete() }
        Log.i(TAG, "app has ${File("/proc/self/fd").list()?.size} open descriptors")

        val pair = Spawner.socketPair()
        val output = ParcelFileDescriptor.open(
            stdout, ParcelFileDescriptor.MODE_WRITE_ONLY or ParcelFileDescriptor.MODE_CREATE or ParcelFileDescriptor.MODE_APPEND,
        )
        val ended = CountDownLatch(1)
        var status: ExitStatus? = null
        val pid = Spawner.spawn(
            "/system/bin/sh", listOf("sh", "-c", SCRIPT), listOf("PATH=/system/bin"),
            pair.child, output,
        ) {
            status = it
            ended.countDown()
        }
        pair.child.close()
        output.close()

        val socket = pair.app.fileDescriptor
        Os.setsockoptTimeval(socket, OsConstants.SOL_SOCKET, OsConstants.SO_RCVTIMEO, StructTimeval.fromMillis(5000))
        FileOutputStream(socket).write("hello\n".toByteArray())
        val reply = FileInputStream(socket).bufferedReader().readLine()
        Log.i(TAG, "pid $pid answered: $reply")

        val reaped = ended.await(5, TimeUnit.SECONDS)
        pair.app.close()
        Log.i(TAG, "pid $pid reaped=$reaped status=$status")

        val listing = stdout.readText()
        listing.lineSequence().filter { it.isNotBlank() }.forEach { Log.i(TAG, "out: $it") }
        // Descriptor number to target, per listing: ls's own, then the shell's.
        fun descriptors(text: String) = text.lineSequence()
            .mapNotNull { Regex("""\s(\d+) -> (.*)$""").find(it)?.destructured }
            .associate { (fd, target) -> fd.toInt() to target }
        val ls = descriptors(listing.substringBefore("sh fds:"))
        val sh = descriptors(listing.substringAfter("sh fds:"))
        Log.i(TAG, "ls saw descriptors ${ls.keys}, sh has ${sh.keys}")
        // Beyond 0-2, ls may only hold its own /proc/<pid>/fd directory (toybox opens it twice).
        val noLeak = sh.keys == setOf(0, 1, 2) && ls.keys.containsAll(listOf(0, 1, 2)) &&
            ls.filterKeys { it > 2 }.values.all { Regex("""/proc/\d+/fd""").matches(it) }

        val passed = reply == "got:hello" && status == ExitStatus(3, null) && noLeak
        Log.i(TAG, if (passed) "PASS" else "FAIL")
    }
}
