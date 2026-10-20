package com.pd2launcher.app

import android.content.Context
import android.os.ParcelFileDescriptor
import android.os.SystemClock
import com.pd2launcher.app.input.GamepadFile
import com.pd2launcher.app.input.RelaySender
import com.pd2launcher.app.input.RelayWindows
import java.io.File
import java.io.FileInputStream
import java.io.IOException
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

/** What the Game view drives: the X screen and the game's pad. */
class GameInput(val relay: RelaySender, val gamepad: GamepadFile)

/**
 * The Android game environment's own programs:
 * Xvfb with its framebuffer in memory ([Framebuffer]: a memfd, `-fbfd`; a `-fbdir` file when no
 * memfd can be made) at the X screen's size,
 * PulseAudio writing into a FIFO that [GameAudio] plays, and x-input-relay, which takes the
 * Game view's touch and keys into the X screen, beside the gamepad file the game's XInput bridge
 * reads. FEX and Wine are started by the engine, not here. Called from the session's single
 * worker thread.
 */
class GameEnvironment(
    private val paths: SessionPaths,
    private val starter: ProcessStarter,
    private val log: (String) -> Unit,
    private val context: Context,
) {
    // Cleared by the reaper thread when the process ends.
    @Volatile private var xvfb: OwnedProcess? = null
    @Volatile private var pulse: OwnedProcess? = null
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

    /** Whether the X screen is up: Play and Install need it; a lone PulseAudio or relay does not count. */
    val running: Boolean get() = xvfb != null

    /** Starts Xvfb at [width]x[height], PulseAudio and the input relay; waits until they are ready. */
    fun start(width: Int, height: Int) {
        if (running) {
            log("game environment already running")
            return
        }
        // Xvfb ended but PulseAudio or the relay is left: those go before it starts whole again.
        if (pulse != null || relay != null) stop()
        paths.createFolders()
        paths.framebuffer.delete()
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
        // Settings, Game: while the game is not on the screen, it draws no more than this.
        if (AppSettingsStore.load(context).lowFrameRateInBackground) {
            log(
                if (BackgroundPacer.start(paths.presentCounter)) {
                    "the game draws at most ${BackgroundPacer.FPS} frames a second while it is not on the screen"
                } else {
                    "no background frame rate: the game draws as fast as it can while it is not on the screen"
                },
            )
        }
        val env = paths.userlandProgramEnvironment()
        val bin = File(paths.userland, "usr/bin")
        try {
            val xvfbArguments = listOf(
                "Xvfb", DISPLAY, "-screen", "0", "${width}x${height}x24",
            ) + (if (memory != null) listOf("-fbfd", "${Framebuffer.XVFB_DESCRIPTOR}") else listOf("-fbdir", paths.framebufferDir.path)) + listOf(
                "-extension", "MIT-SHM",
                "-nolock", "-nolisten", "tcp",
                "-xkbdir", File(paths.userland, "usr/share/X11/xkb").path,
            )
            // On the little cores (Android's taskset sets its own affinity, which Xvfb keeps):
            // copying every frame there leaves the big cores' cache to the game.
            val littleCores = LittleCores.mask(LittleCores.ofDevice())
            val (program, arguments) = if (littleCores != null && File(TASKSET).canExecute()) {
                TASKSET to listOf("taskset", littleCores, File(bin, "Xvfb").path) + xvfbArguments.drop(1)
            } else {
                File(bin, "Xvfb").path to xvfbArguments
            }
            val x = starter.start("xvfb", program, arguments, env, fd3 = memory) { process, _ -> if (xvfb == process) xvfb = null }
            xvfb = x
            awaitFramebuffer(x, framebuffer)
            _framebuffer.value = framebuffer

            val pulseRuntime = File(paths.runtimeDir, "pulse").apply { mkdirs() }
            // A killed PulseAudio leaves its socket behind: the wait below would take it for the new one's.
            File(pulseRuntime, "native").delete()
            File(paths.home, ".config/pulse").mkdirs()
            val script = File(paths.runtimeDir, "pulse.pa")
            val fifo = File(paths.runtimeDir, "pulse-sink")
            script.writeText(
                """
                load-module module-native-protocol-unix auth-anonymous=1
                load-module module-pipe-sink ${GameAudioFormat.pulseSinkArgs(fifo)}
                set-default-sink pipe
                """.trimIndent() + "\n",
            )
            val modules = File(paths.userland, "usr/lib").listFiles().orEmpty()
                .firstOrNull { it.name.startsWith("pulse-") && File(it, "modules").isDirectory }
                ?.let { File(it, "modules") } ?: throw IOException("no PulseAudio module folder in the userland")
            val p = starter.start(
                "pulseaudio", File(bin, "pulseaudio").path,
                listOf(
                    "pulseaudio", "-n", "-F", script.path, "--daemonize=no", "--use-pid-file=no",
                    "--exit-idle-time=-1", "--disable-shm=yes", "-p", modules.path,
                ),
                env,
            ) { process, _ -> if (pulse == process) pulse = null }
            pulse = p
            awaitFile(p, File(pulseRuntime, "native"), "PulseAudio's socket")
            audio = GameAudio(context, fifo, log).also {
                it.setBackground(inBackground, soundInBackground, gameRunning)
                it.start()
            }
            val r = startInput(bin, env)
            log(
                "game environment started: Xvfb pid ${x.pid} at ${width}x$height (framebuffer ${if (memory != null) "in memory" else "in a file"}), " +
                    "PulseAudio pid ${p.pid}, x-input-relay pid ${r.pid}",
            )
        } catch (error: Throwable) {
            log("game environment failed to start: $error")
            stop()
            throw error
        }
    }

    /**
     * The gamepad file (a connected, neutral pad) and x-input-relay on the X display, its
     * standard input the child end of a socket pair whose app end the Game view writes to.
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
        _input.value = GameInput(RelaySender(ParcelFileDescriptor.AutoCloseOutputStream(pair.app)), gamepad)
        watchWindows(pair.app)
        return r
    }

    /**
     * x-input-relay's word on the X screen's windows, each time it changes, on the same socket
     * ([RelayWindows]): the game's window alone and where it is, or not alone. The game's own
     * buffers show only while it is alone, where it is ([PresentServer.setGameWindow]). Ends with
     * the socket, leaving the X screen to show.
     */
    private fun watchWindows(socket: ParcelFileDescriptor) {
        val input = FileInputStream(socket.fileDescriptor)
        Thread({
            try {
                while (true) {
                    val window = RelayWindows.read(input)
                    PresentServer.current.value?.setGameWindow(window)
                }
            } catch (_: IOException) {
                // The relay and its socket are gone with the game environment.
                PresentServer.current.value?.setGameWindow(null)
            }
        }, "pd2-windows").apply { isDaemon = true }.start()
    }

    /** The framebuffer's memfd; null (the `-fbdir` file instead) when it cannot be made. */
    private fun framebufferMemory(): ParcelFileDescriptor? = try {
        Spawner.memfd("xvfb-screen0")
    } catch (error: IOException) {
        log("no framebuffer in memory, a file instead: $error")
        null
    }

    /** SIGTERM to the relay, PulseAudio and Xvfb, SIGKILL after [UidCleanup.GRACE_MS]; waits for their ends. */
    fun stop() {
        closeInput()
        stopAudio()
        PresentServer.stopCurrent()
        BackgroundPacer.stop()
        closeScreen()
        val owned = listOfNotNull(relay, pulse, xvfb)
        if (owned.isEmpty()) return
        log("stopping game environment: ${owned.joinToString { "${it.pid} ${it.name}" }}")
        val running = { owned.filter { OwnedProcesses.isRunning(it.pid) }.map { UidProcess(it.pid, it.name) } }
        UidCleanup(running, ::sendSignal, log, now = SystemClock::elapsedRealtime).run()
        relay = null
        pulse = null
        xvfb = null
    }

    /** The processes are gone already (the engine-exit cleanup ended them). */
    fun forget() {
        closeInput()
        stopAudio()
        PresentServer.stopCurrent()
        BackgroundPacer.stop()
        closeScreen()
        relay = null
        pulse = null
        xvfb = null
    }

    /** The views let go of the framebuffer first; their mappings outlive its descriptor. */
    private fun closeScreen() {
        _framebuffer.value = null
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
            if (!OwnedProcesses.isRunning(process.pid)) throw IOException("${process.name} ended before its framebuffer was written")
            if (System.nanoTime() > deadline) throw IOException("Xvfb's framebuffer was not written within $READY_TIMEOUT_MS ms")
            Thread.sleep(50)
        }
    }

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
        private const val READY_TIMEOUT_MS = 15_000L

        private val _input = MutableStateFlow<GameInput?>(null)
        /** The running game environment's input, for the Game view; null while it is stopped. */
        val input: StateFlow<GameInput?> = _input.asStateFlow()

        private val _framebuffer = MutableStateFlow<Framebuffer?>(null)
        /** The running game environment's X screen, for the Game view and the Live progress window; null while it is stopped. */
        val framebuffer: StateFlow<Framebuffer?> = _framebuffer.asStateFlow()

        /** The gamepad file, named to the game's XInput bridge in `PD2_GAMEPAD_FILE`. */
        fun gamepadFile(paths: SessionPaths) = File(paths.files, "gamepad.bin")
    }
}
