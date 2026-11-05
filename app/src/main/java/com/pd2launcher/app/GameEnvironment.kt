package com.pd2launcher.app

import android.content.Context
import android.os.ParcelFileDescriptor
import android.os.SystemClock
import android.system.ErrnoException
import android.system.Os
import com.pd2launcher.app.input.GameWindow
import com.pd2launcher.app.input.GamepadFile
import com.pd2launcher.app.input.PointerBounds
import com.pd2launcher.app.input.PointerImage
import com.pd2launcher.app.input.RelaySender
import com.pd2launcher.app.input.RelayWord
import com.pd2launcher.app.input.RelayWords
import java.io.File
import java.io.FileInputStream
import java.io.IOException
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

/** What the Game view drives: the X screen (and the pointer area its relay has, [pointer]) and the game's pad. */
class GameInput(val relay: RelaySender, val gamepad: GamepadFile, val pointer: PointerBounds)

/**
 * The Android game environment's own programs:
 * Xvfb with its framebuffer in memory ([Framebuffer]: a memfd, `-fbfd`; a `-fbdir` file when no
 * memfd can be made) at the X screen's size,
 * the game's sound ([GameAudio]: the ring that the game's Wine writes through ALSA, played by the
 * app), and x-input-relay, which takes the Game view's touch and keys into the X screen, beside
 * the gamepad file the game's XInput bridge reads. On a route to the phone's own driver ([route])
 * also virglrenderer's vtest server, first: on the Venus route the one through which the game's
 * Vulkan (Zink on Venus) reaches the phone's own Vulkan driver ([SystemDriver]), on the VirGL route
 * the one whose OpenGL renderer draws the game's OpenGL (Mesa's virgl) with the phone's own OpenGL
 * ES driver. Wine (with Box64's wowbox64.dll) is started by the engine, not here. Called from the
 * session's single worker thread.
 */
class GameEnvironment(
    private val paths: SessionPaths,
    private val starter: ProcessStarter,
    private val log: (String) -> Unit,
    private val context: Context,
    /** The running engine's route of the game's graphics ([GraphicsRoute]); a route to the phone's driver needs its server here. */
    private val route: () -> GraphicsRoute? = { null },
) {
    // Cleared by the reaper thread when the process ends.
    @Volatile private var vtest: OwnedProcess? = null
    /** Game contexts that the server gave up ([ContextLossWatch]), counted from its output. */
    private val contextsLost = java.util.concurrent.atomic.AtomicInteger()

    /** The game contexts that the system-driver server gave up since this app process started. */
    fun systemDriverContextsLost(): Int = contextsLost.get()

    /** The X screen's pixels for [pictureHash], mapped once per framebuffer. */
    @Volatile private var pictureSource: Pair<Framebuffer, ScreenPixels>? = null

    /**
     * A hash of the game's picture now ([FrozenPicture.hash]): the game's buffer on the screen
     * while the app shows it ([PresentServer.readShown]; the VirGL route's direct present leaves
     * the X screen still), else the X screen's; null without an X screen or before Xvfb has
     * written its framebuffer, and null while the frames go to the app's buffers but none shows
     * (no Game view: the app in the background or on its own screens), when no picture of the
     * game can be read. While the game's buffer shows but cannot be read yet (its GPU writes have
     * not finished, [Shown.NotReady]), [notReady] runs and the hash is null: the X screen does not
     * show that frame, so it is no picture of the game. For the frozen-picture rule ([FrozenPicture.ROUTES]).
     */
    fun pictureHash(notReady: () -> Unit = {}): Long? {
        when (val shown = PresentServer.current.value?.readShown { FrozenPicture.hash(it.width, it.height, it::pixel) }) {
            is Shown.Read -> return shown.value
            Shown.NotReady -> {
                notReady()
                return null
            }
            null -> Unit
        }
        if (PresentCounter.toTheApp(paths.presentCounter) == true) return null
        val current = screen ?: return null
        val pixels = pictureSource?.takeIf { it.first === current }?.second
            ?: current.map()?.also { pictureSource = current to it }
            ?: return null
        return FrozenPicture.hash(pixels)
    }
    @Volatile private var xvfb: OwnedProcess? = null
    @Volatile private var relay: OwnedProcess? = null
    private var screen: Framebuffer? = null
    private var relaySocket: ParcelFileDescriptor? = null
    private var audio: GameAudio? = null
    @Volatile private var inBackground = false
    @Volatile private var soundInBackground = true
    @Volatile private var gameRunning = false

    /**
     * The game's sound is muted while the app is in the background and "Sound in background" is
     * off; it holds audio focus only while the Game client runs ([gameRunning]).
     */
    fun setBackground(inBackground: Boolean, soundInBackground: Boolean, gameRunning: Boolean) {
        this.inBackground = inBackground
        this.soundInBackground = soundInBackground
        this.gameRunning = gameRunning
        audio?.setBackground(inBackground, soundInBackground, gameRunning)
    }

    /** Whether the X screen is up: Play and Install need it; a lone relay does not count. */
    val running: Boolean get() = xvfb != null

    /**
     * On a route with a server ([GraphicsRoute.server]: Venus, VirGL; RADV and libhybris have
     * none), the X screen is up but the vtest server has ended: start again before a launch.
     */
    fun serverLost(): Boolean = xvfb != null && vtest == null && route()?.server == true

    /** Starts Xvfb at [width]x[height], the game's sound and the input relay; waits until they are ready. */
    fun start(width: Int, height: Int) {
        if (running) {
            log("game environment already running")
            return
        }
        // Xvfb ended but the relay, the sound or the vtest server is left: those go before it starts whole again.
        if (relay != null || audio != null || vtest != null) stop()
        paths.createFolders()
        paths.framebuffer.delete()
        // The libhybris route's status lines are those of this game environment's games only.
        paths.hybrisStatus.delete()
        val memory = framebufferMemory()
        val framebuffer = memory?.let { Framebuffer.inMemory(it) } ?: Framebuffer.inFile(paths.framebuffer)
        screen = framebuffer
        try {
            PresentCounter.ensure(paths.presentCounter)
        } catch (error: IOException) {
            log("no present counter: $error")
        }
        // Before the game: its Mesa asks for the app's buffers when it makes its swapchain.
        if (PresentServer.start(paths.presentSocket, paths.presentCounter) == null) log("no direct present: the X screen only")
        // The virtual display's vertical blank at the player's frame rate (Settings → Display) on the
        // screen's full rate ([ScreenRate]); the Game view sets it again for its screen.
        val clockRate = FrameRateVote.rate(ScreenRate.read(context), AppSettingsStore.load(context).frameRate)
        if (!FrameClock.start(paths.presentCounter, clockRate)) log("no frame clock: the game draws as fast as it can")
        val env = paths.userlandProgramEnvironment()
        val bin = File(paths.userland, "usr/bin")
        try {
            val v = when (route()) {
                GraphicsRoute.VENUS -> startSystemDriver()
                GraphicsRoute.VIRGL -> startVirglServer()
                else -> null
            }
            val xvfbArguments = listOf(
                "Xvfb", DISPLAY, "-screen", "0", "${width}x${height}x24",
            ) + (if (memory != null) listOf("-fbfd", "${Framebuffer.XVFB_DESCRIPTOR}") else listOf("-fbdir", paths.framebufferDir.path)) + listOf(
                "-extension", "MIT-SHM",
                "-nolock", "-nolisten", "tcp",
                "-xkbdir", File(paths.userland, "usr/share/X11/xkb").path,
            )
            // On the little cores (Android's taskset sets its own affinity, which Xvfb keeps):
            // copying every frame there leaves the big cores' cache to the game. Some firmware
            // refuses taskset that affinity: then Xvfb starts without it ([LittleCoresStart]).
            val littleCores = LittleCores.mask(LittleCores.ofDevice())
            val xvfbPath = File(bin, "Xvfb").path
            val pinned = when {
                littleCores == null || !File(TASKSET).canExecute() -> null
                // Debug builds: with this file "taskset" fails at once, as on that firmware.
                BuildConfig.DEBUG && paths.tasksetFailsForTest.exists() ->
                    SHELL to listOf("sh", "-c", TASKSET_FAILS_FOR_TEST, "taskset", littleCores, xvfbPath) + xvfbArguments.drop(1)
                else -> TASKSET to listOf("taskset", littleCores, xvfbPath) + xvfbArguments.drop(1)
            }
            val x = littleCoresStart.start(pinned, xvfbPath to xvfbArguments, log, start = { program, arguments ->
                starter.start("xvfb", program, arguments, env, fd3 = memory, rewrite = XvfbListenerLines()::rewrite) { process, _ ->
                    if (xvfb == process) xvfb = null
                }.also { xvfb = it }
            }) { awaitFramebuffer(it, framebuffer) }
            _framebuffer.value = framebuffer

            // Before any Wine process of the game: Wine's ALSA output finds its configuration and the ring.
            audio = GameAudio(context, paths.soundRing, paths.alsaConfig, paths.alsaPlugin, log).also {
                it.setBackground(inBackground, soundInBackground, gameRunning)
                it.start()
            }
            val r = startInput(bin, env)
            log(
                "game environment started: Xvfb pid ${x.pid} at ${width}x$height (framebuffer ${if (memory != null) "in memory" else "in a file"}), " +
                    "x-input-relay pid ${r.pid}" + (v?.let { ", vtest server pid ${it.pid}" } ?: ""),
            )
        } catch (error: Throwable) {
            log("game environment failed to start: $error")
            stop()
            throw error
        }
    }

    /**
     * The gamepad file (a connected, neutral pad) and x-input-relay on the X display, its
     * standard input the child end of a socket pair whose app end the Game view writes to; the
     * relay has no pointer area until the Game view's first events give it one ([PointerBounds]).
     */
    private fun startInput(bin: File, env: List<String>): OwnedProcess {
        val gamepad = GamepadFile.open(gamepadFile(paths))
        val pair = try {
            Spawner.socketPair()
        } catch (error: IOException) {
            gamepad.close()
            throw error
        }
        val r = try {
            starter.start(
                "x-input-relay", File(bin, "x-input-relay").path, listOf("x-input-relay"),
                env.filterNot { it.startsWith("DISPLAY=") } + "DISPLAY=$DISPLAY", pair.child,
            ) { process, _ -> if (relay == process) relay = null }
        } catch (error: IOException) {
            pair.app.close()
            gamepad.close()
            throw error
        } finally {
            pair.child.close()
        }
        relay = r
        relaySocket = pair.app
        _input.value = GameInput(RelaySender(ParcelFileDescriptor.AutoCloseOutputStream(pair.app)), gamepad, PointerBounds())
        watchWindows(pair.app)
        return r
    }

    /**
     * x-input-relay's words on the same socket ([RelayWords]), each time one changes: on the X
     * screen's windows, the game's window alone and where it is, or not alone (the game's own
     * buffers show only while it is alone, where it is: [PresentServer.setGameWindow], [gameWindow]);
     * on the X screen's pointer, its image while it shows ([pointer]) and where it is ([pointerAt]),
     * which the Game view draws over the game's own buffers. Ends with the socket, leaving the X
     * screen to show and no pointer.
     */
    private fun watchWindows(socket: ParcelFileDescriptor) {
        val input = FileInputStream(socket.fileDescriptor)
        _gameWindow.value = null
        _pointer.value = null
        _pointerAt.value = null
        Thread({
            try {
                while (true) {
                    when (val word = RelayWords.read(input)) {
                        is RelayWord.Windows -> {
                            _gameWindow.value = word.window
                            PresentServer.current.value?.setGameWindow(word.window)
                        }
                        is RelayWord.Pointer -> _pointer.value = word.image
                        RelayWord.PointerHidden -> {
                            _pointer.value = null
                            _pointerAt.value = null
                        }
                        is RelayWord.PointerAt -> _pointerAt.value = word
                    }
                }
            } catch (_: IOException) {
                // The relay and its socket are gone with the game environment.
                PresentServer.current.value?.setGameWindow(null)
                _gameWindow.value = null
                _pointer.value = null
                _pointerAt.value = null
            }
        }, "pd2-windows").apply { isDaemon = true }.start()
    }

    /**
     * virglrenderer's vtest server, Venus only, on [SessionPaths.vtestSocket]: bionic, so started
     * straight from the APK's native-library folder, not through the userland's loader, as the
     * app's user. For each client it starts the render server that `RENDER_SERVER_EXEC_PATH`
     * names (virglrenderer src/proxy/proxy_server.c), which loads the phone's `libvulkan.so`.
     */
    private fun startSystemDriver(): OwnedProcess {
        val libraries = File(context.applicationInfo.nativeLibraryDir)
        val socket = paths.vtestSocket
        socket.delete()
        val v = starter.start(
            "vtest", File(libraries, SystemDriver.SERVER_LIBRARY).path,
            listOf("virgl_test_server", "--venus", "--no-virgl", "--socket-path", socket.path),
            listOf(
                "HOME=${paths.home.path}",
                "TMPDIR=${paths.tmp.path}",
                "RENDER_SERVER_EXEC_PATH=${File(libraries, SystemDriver.RENDER_SERVER_LIBRARY).path}",
            ) + if (BuildConfig.DEBUG) systemDriverDebugValues(SystemDriver.forceFile(GraphicsRoute.VENUS)) else emptyList(),
            mirror = ContextLossWatch().let { watch -> { line: String -> if (watch.feed(line)) contextsLost.incrementAndGet() } },
        ) { process, _ -> if (vtest == process) vtest = null }
        vtest = v
        try {
            awaitFile(v, socket, "the vtest server's socket")
        } catch (error: IOException) {
            throw SystemDriverStartFailed(error.message ?: error.toString(), error)
        }
        return v
    }

    /**
     * The VirGL route's vtest server ([SystemDriver.GL_SERVER_LIBRARY]) on [SessionPaths.vtestSocket],
     * started as the Venus route's: virglrenderer's OpenGL renderer (vrend) on EGL's default
     * display, surfaceless, with OpenGL ES (`--use-egl-surfaceless --use-gles`; not `--no-virgl`).
     * For each client (each Wine process of the game that draws with OpenGL) it forks a child,
     * which loads the phone's `libEGL.so` and `libGLESv2.so` and holds that client's context. A
     * game context that vrend gives up ([VirglContextWatch]) counts as lost. With
     * `PD2_PRESENT_SOCKET` the child copies the game's frames on the GPU into the app's buffers and
     * gives them to [PresentServer] (devdriver patch virglrenderer-gl/0003, Mesa patch 0012);
     * `PD2_VIRGL_APP_PRESENT=0` in the debug file keeps the copy through the X screen.
     */
    private fun startVirglServer(): OwnedProcess {
        val libraries = File(context.applicationInfo.nativeLibraryDir)
        val socket = paths.vtestSocket
        socket.delete()
        val v = starter.start(
            "vtest-gl", File(libraries, SystemDriver.GL_SERVER_LIBRARY).path,
            listOf("virgl_test_server", "--use-egl-surfaceless", "--use-gles", "--socket-path", socket.path),
            listOf(
                "HOME=${paths.home.path}",
                "TMPDIR=${paths.tmp.path}",
                "PD2_PRESENT_SOCKET=${paths.presentSocket.path}",
            ) + if (BuildConfig.DEBUG) systemDriverDebugValues(SystemDriver.forceFile(GraphicsRoute.VIRGL)) else emptyList(),
            mirror = VirglContextWatch().let { watch -> { line: String -> if (watch.feed(line)) contextsLost.incrementAndGet() } },
        ) { process, _ -> if (vtest == process) vtest = null }
        vtest = v
        try {
            awaitFile(v, socket, "the vtest server's socket")
        } catch (error: IOException) {
            throw SystemDriverStartFailed(error.message ?: error.toString(), error)
        }
        return v
    }

    /**
     * The vtest server's children: one for each client, each with its render server in a process
     * group of its own, which holds the phone driver's memory for the game. They end with their
     * client's socket; one hung in the driver, or with its client still there, would outlive the
     * stop. SIGTERM, then SIGKILL, to every process of the app's user that runs the server's
     * programs.
     */
    private fun stopServerChildren() {
        val names = setOf(SystemDriver.SERVER_LIBRARY, SystemDriver.RENDER_SERVER_LIBRARY, SystemDriver.GL_SERVER_LIBRARY)
        val servers = {
            UidCleanup.scan(File("/proc"), Os.getuid(), Os.getpid(), ::owner).filter { process ->
                val exe = try {
                    Os.readlink("/proc/${process.pid}/exe")
                } catch (_: ErrnoException) {
                    ""
                }
                File(exe).name in names
            }
        }
        if (servers().isEmpty()) return
        UidCleanup(servers, ::sendSignal, log, now = SystemClock::elapsedRealtime).run()
    }

    private fun owner(file: File): Int? = try {
        Os.stat(file.path).st_uid
    } catch (_: ErrnoException) {
        null
    }

    /**
     * Debug builds: the `NAME=value` lines of the debug file [name] that forces the route
     * ([SystemDriver.FORCE_FILE], [SystemDriver.FORCE_VIRGL_FILE]) for its server, such as
     * `VIRGL_LOG_LEVEL=debug`.
     */
    private fun systemDriverDebugValues(name: String): List<String> =
        File(context.filesDir, name).takeIf { it.isFile }?.readLines().orEmpty()
            .map { it.trim() }.filter { DEBUG_VALUE.matches(it) }

    /** The framebuffer's memfd; null (the `-fbdir` file instead) when it cannot be made. */
    private fun framebufferMemory(): ParcelFileDescriptor? = try {
        Spawner.memfd("xvfb-screen0")
    } catch (error: IOException) {
        log("no framebuffer in memory, a file instead: $error")
        null
    }

    /** SIGTERM to the relay, Xvfb and the vtest server, SIGKILL after [UidCleanup.GRACE_MS]; waits for their ends. */
    fun stop() {
        closeInput()
        stopAudio()
        PresentServer.stopCurrent()
        FrameClock.stop()
        closeScreen()
        val owned = listOfNotNull(relay, xvfb, vtest)
        if (owned.isEmpty()) {
            stopServerChildren()
            return
        }
        log("stopping game environment: ${owned.joinToString { "${it.pid} ${it.name}" }}")
        val running = { owned.filter { OwnedProcesses.isRunning(it.pid) }.map { UidProcess(it.pid, it.name) } }
        UidCleanup(running, ::sendSignal, log, now = SystemClock::elapsedRealtime).run()
        relay = null
        xvfb = null
        vtest = null
        stopServerChildren()
    }

    /** The processes are gone already (the engine-exit cleanup ended them). */
    fun forget() {
        closeInput()
        stopAudio()
        PresentServer.stopCurrent()
        FrameClock.stop()
        closeScreen()
        relay = null
        xvfb = null
        vtest = null
    }

    /** The views let go of the framebuffer first; their mappings outlive its descriptor. */
    private fun closeScreen() {
        _framebuffer.value = null
        pictureSource = null
        screen?.close()
        screen = null
    }

    private fun stopAudio() {
        audio?.stop()
        audio = null
    }

    /** The Game view stops sending, and the game sees its pad disconnected. */
    private fun closeInput() {
        val input = _input.value
        _input.value = null
        input?.gamepad?.close()
        try {
            relaySocket?.close()
        } catch (_: IOException) {
        }
        relaySocket = null
    }

    /** Until Xvfb has sized [framebuffer] and written its header, as it sets up its screen. */
    private fun awaitFramebuffer(process: OwnedProcess, framebuffer: Framebuffer) {
        val deadline = System.nanoTime() + READY_TIMEOUT_MS * 1_000_000
        while (framebuffer.header() == null) {
            if (!OwnedProcesses.isRunning(process.pid)) throw EndedEarly("${process.name} ended before its framebuffer was written")
            if (System.nanoTime() > deadline) throw IOException("Xvfb's framebuffer was not written within $READY_TIMEOUT_MS ms")
            Thread.sleep(50)
        }
    }

    /** Until [file] exists ([process]'s socket, as it starts to listen). */
    private fun awaitFile(process: OwnedProcess, file: File, what: String) {
        val deadline = System.nanoTime() + READY_TIMEOUT_MS * 1_000_000
        while (!file.exists()) {
            if (!OwnedProcesses.isRunning(process.pid)) throw IOException("${process.name} ended before $what appeared")
            if (System.nanoTime() > deadline) throw IOException("$what did not appear within $READY_TIMEOUT_MS ms")
            Thread.sleep(50)
        }
    }

    companion object {
        /**
         * Xvfb's display: `:0` for the release app, `:1` for the development build (app/build.gradle.kts),
         * as its userland.env says, so both apps' game environments can run at once (Xvfb's abstract socket
         * `@/tmp/.X11-unix/X<n>` is shared by every app on the device).
         */
        const val DISPLAY = BuildConfig.X_DISPLAY
        private const val TASKSET = "/system/bin/taskset"
        /** A `NAME=value` line of the debug file for the vtest server ([systemDriverDebugValues]). */
        private val DEBUG_VALUE = Regex("^[A-Za-z_][A-Za-z0-9_]*=.*$")
        /** Debug builds' failing taskset ([SessionPaths.tasksetFailsForTest]): a shell that writes toybox's line and exits 1. */
        private const val SHELL = "/system/bin/sh"
        private const val TASKSET_FAILS_FOR_TEST = "echo \"taskset: failed to set pid \$\$'s affinity: Operation not permitted (debug switch)\" >&2; exit 1"
        /** For the app's process: after one refusal of taskset, Xvfb starts without it. */
        private val littleCoresStart = LittleCoresStart()
        private const val READY_TIMEOUT_MS = 15_000L

        private val _input = MutableStateFlow<GameInput?>(null)
        /** The running game environment's input, for the Game view; null while it is stopped. */
        val input: StateFlow<GameInput?> = _input.asStateFlow()

        private val _framebuffer = MutableStateFlow<Framebuffer?>(null)
        /** The running game environment's X screen, for the Game view and the Live progress window; null while it is stopped. */
        val framebuffer: StateFlow<Framebuffer?> = _framebuffer.asStateFlow()

        private val _gameWindow = MutableStateFlow<GameWindow?>(null)
        /** The game's window while it is alone on the X screen (x-input-relay's 'F'); null while not, or while stopped. */
        val gameWindow: StateFlow<GameWindow?> = _gameWindow.asStateFlow()

        private val _pointer = MutableStateFlow<PointerImage?>(null)
        /** The X screen's pointer image while it shows (x-input-relay's 'I'), for the Game view; null while it shows nothing, or while stopped. */
        val pointer: StateFlow<PointerImage?> = _pointer.asStateFlow()

        private val _pointerAt = MutableStateFlow<RelayWord.PointerAt?>(null)
        /** Where the X screen's pointer is while it shows (x-input-relay's 'P'); null before the first place, or while it shows nothing. */
        val pointerAt: StateFlow<RelayWord.PointerAt?> = _pointerAt.asStateFlow()

        /** The gamepad file, named to the game's XInput bridge in `PD2_GAMEPAD_FILE`. */
        fun gamepadFile(paths: SessionPaths) = File(paths.files, "gamepad.bin")
    }
}

/** The system-driver mode's vtest server did not start ([GameEnvironment.start]). */
class SystemDriverStartFailed(message: String, cause: Throwable) : IOException(message, cause)
/**
 * Xvfb's four lines at each start about the folder `/tmp/.X11-unix`, which it cannot make (no
 * app may write to `/tmp` on Android; the X screen listens on its abstract socket), as one line
 * in `xvfb.log` ([ProcessStarter.start]'s rewrite): one object for each start.
 */
class XvfbListenerLines {
    private var said = false

    /** [line] as the log writes it: the first of the four as [LINE], the others left out, any other line as it is. */
    fun rewrite(line: String): String? {
        if (EXPECTED.none { line.startsWith(it) }) return line
        if (said) return null
        said = true
        return LINE
    }

    companion object {
        const val LINE = "Xvfb: no /tmp/.X11-unix listener (expected on Android)"
        private val EXPECTED = listOf(
            "_XSERVTransmkdir: ERROR: euid != 0,directory /tmp/.X11-unix will not be created.",
            "_XSERVTransmkdir: ERROR: Cannot create /tmp/.X11-unix",
            "_XSERVTransSocketUNIXCreateListener: mkdir(/tmp/.X11-unix) failed",
            "_XSERVTransMakeAllCOTSServerListeners: failed to create listener for unix",
        )
    }
}
