package com.pd2launcher.app

import android.os.ParcelFileDescriptor
import java.io.File

/**
 * The X screen's socket: a Unix socket file in the app's runtime folder ([SessionPaths.xSocket]),
 * which the app binds and Xvfb listens on (`-listenfd`, userland patches/xserver/0004); every X
 * client's DISPLAY is its path. Not Xvfb's own abstract socket `@/tmp/.X11-unix/X<n>`: every app
 * on a device shares that name, so another app's X server on the same number stopped ours
 * ("server already running", issue #26), and any app could reach our X screen.
 */
object XSocket {
    private const val BACKLOG = 128

    /** A listening socket bound to [file] ([Spawner.listen]); the caller owns the result. */
    fun listen(file: File): ParcelFileDescriptor = Spawner.listen(file, BACKLOG)

    /** The DISPLAY of the X screen that listens on [file]: its path (libxcb takes a socket's path). */
    fun display(file: File): String = file.path
}
