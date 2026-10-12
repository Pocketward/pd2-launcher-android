package com.pd2launcher.app

import android.os.ParcelFileDescriptor
import android.util.Log
import androidx.annotation.Keep
import java.io.IOException

/** How a spawned process ended: [code] when it exited, [signal] when a signal killed it. */
data class ExitStatus(val code: Int?, val signal: Int?) {
    override fun toString(): String = if (signal != null) "killed by signal $signal" else "exit status $code"
}

/** The app's end and the child's end of a connected AF_UNIX stream socket pair. */
class SocketPair(val app: ParcelFileDescriptor, val child: ParcelFileDescriptor)

/**
 * Starts the app's programs (libspawner.so): a new session, the given descriptors as standard
 * input and as standard output and error (and one more as descriptor 3, when given), and no other
 * descriptor of the app. Every exit is logged and reported to the spawn's callback.
 */
object Spawner {
    private const val TAG = "pd2-spawner"

    private val lock = Any()
    private val callbacks = HashMap<Int, (ExitStatus) -> Unit>()

    init {
        System.loadLibrary("spawner")
    }

    @JvmStatic
    private external fun nativeSpawn(
        path: String,
        argv: Array<String>,
        envp: Array<String>,
        stdinFd: Int,
        outputFd: Int,
        fd3: Int,
    ): Int

    @JvmStatic
    private external fun nativeMemfd(name: String): Int

    /**
     * A new, empty memfd named [name], close-on-exec (a child gets it only as [spawn]'s `fd3`), sealed
     * against shrinking (it may only grow).
     */
    fun memfd(name: String): ParcelFileDescriptor {
        val fd = nativeMemfd(name)
        if (fd < 0) throw IOException("memfd_create failed: errno ${-fd}")
        return ParcelFileDescriptor.adoptFd(fd)
    }

    @JvmStatic
    private external fun nativeSocketPair(): IntArray?

    /** A connected socket pair; both ends close on exec. */
    fun socketPair(): SocketPair {
        val fds = nativeSocketPair() ?: throw IOException("socketpair failed")
        return SocketPair(ParcelFileDescriptor.adoptFd(fds[0]), ParcelFileDescriptor.adoptFd(fds[1]))
    }

    /**
     * Start [path] with exactly [argv] and [envp]; returns its pid. [onExit] runs on
     * the child's reaper thread when it ends. [output] becomes both standard output
     * and error, [fd3] (when given) its descriptor 3. The caller still owns [stdin], [output]
     * and [fd3] and closes its copies once the child has them.
     */
    fun spawn(
        path: String,
        argv: List<String>,
        envp: List<String>,
        stdin: ParcelFileDescriptor,
        output: ParcelFileDescriptor,
        fd3: ParcelFileDescriptor? = null,
        onExit: (ExitStatus) -> Unit = {},
    ): Int = synchronized(lock) {
        // Held across the spawn, so the reaper cannot report before the callback is in place.
        val pid = nativeSpawn(
            path, argv.toTypedArray(), envp.toTypedArray(), stdin.fd, output.fd, fd3?.fd ?: -1,
        )
        if (pid < 0) throw IOException("posix_spawn $path failed: errno ${-pid}")
        callbacks[pid] = onExit
        Log.i(TAG, "started $path as pid $pid")
        pid
    }

    /** Called by the reaper thread. */
    @Keep
    @JvmStatic
    private fun onExit(pid: Int, exited: Boolean, value: Int) {
        val status = if (exited) ExitStatus(value, null) else ExitStatus(null, value)
        Log.i(TAG, "pid $pid ended: $status")
        val callback = synchronized(lock) { callbacks.remove(pid) }
        callback?.invoke(status)
    }
}
