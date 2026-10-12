package com.pd2launcher.app

import android.content.Context
import android.media.AudioAttributes
import android.media.AudioFocusRequest
import android.media.AudioFormat
import android.media.AudioManager
import android.media.AudioTrack
import android.os.Process
import android.system.ErrnoException
import android.system.Os
import android.system.OsConstants
import android.system.StructPollfd
import android.util.Log
import java.io.Closeable
import java.io.File
import java.io.FileDescriptor
import java.io.IOException

/**
 * The game's sound format: what PulseAudio's pipe sink writes into the FIFO and what the
 * [GameAudio] track plays, signed 16-bit little-endian stereo at 22.05 kHz: Diablo II's own
 * rate, so Wine's DirectSound mixes the game's 22.05 kHz buffers without resampling them (its FIR
 * resampler is costly under FEX); Android resamples the track natively.
 */
object GameAudioFormat {
    const val RATE = 22_050
    const val CHANNELS = 2
    const val BYTES_PER_FRAME = CHANNELS * 2
    /** The FIFO's capacity once the bridge is connected (`F_SETPIPE_SZ`; the default is 64 KiB, 740 ms): 93 ms. */
    const val PIPE_BYTES = 8 * 1024
    /** The least the track buffers, so a late read does not underrun it. */
    const val TRACK_MS = 40
    /** The most the bridge reads from the FIFO at once. */
    const val CHUNK_BYTES = 4096

    /** `module-pipe-sink` arguments for [fifo] in this format. */
    fun pulseSinkArgs(fifo: File) = "file=${fifo.path} sink_name=pipe format=s16le rate=$RATE channels=$CHANNELS"

    /** The largest absolute sample in the first [size] bytes of [buffer] (s16le), 0 to 32768. */
    fun peak(buffer: ByteArray, size: Int): Int {
        var peak = 0
        var i = 0
        while (i + 1 < size) {
            val sample = (buffer[i].toInt() and 0xFF) or (buffer[i + 1].toInt() shl 8)
            val magnitude = if (sample < 0) -sample else sample
            if (magnitude > peak) peak = magnitude
            i += 2
        }
        return peak
    }

    fun bytesFor(ms: Int): Int = RATE * ms / 1000 * BYTES_PER_FRAME

    fun millisFor(bytes: Long): Long = bytes / BYTES_PER_FRAME * 1000 / RATE

    /** The track's buffer: at least [minBuffer] and [TRACK_MS], in whole frames. */
    fun trackBufferBytes(minBuffer: Int): Int {
        val bytes = maxOf(minBuffer, bytesFor(TRACK_MS))
        return (bytes + BYTES_PER_FRAME - 1) / BYTES_PER_FRAME * BYTES_PER_FRAME
    }
}

/** When the game's track is silent: always muted, never paused, so the FIFO keeps draining. */
object GameAudioMute {
    /** Muted while another app holds audio focus, or while the app is in the background with "Sound in background" off. */
    fun muted(focusLost: Boolean, inBackground: Boolean, soundInBackground: Boolean): Boolean =
        focusLost || (inBackground && !soundInBackground)
}

/**
 * When the game's track asks for audio focus: only while the Game client runs (an install's
 * Wine never pauses the player's music), with the app in the foreground, and not already held.
 * A focus lost for good is asked for again the next time the app comes back to the foreground.
 */
object GameAudioFocus {
    /** [lostForGood]: lost (or refused) since the app last came to the foreground. */
    fun request(gameRunning: Boolean, inBackground: Boolean, held: Boolean, lostForGood: Boolean): Boolean =
        gameRunning && !inBackground && !held && !lostForGood
}

/** The FIFO's read end. */
interface PcmSource : Closeable {
    /** Bytes read into [buffer] at [offset]; 0 when none came within a short wait; -1 when the writer is gone. */
    fun read(buffer: ByteArray, offset: Int, length: Int): Int
}

/** Where the PCM goes; [write] gets whole frames only. */
interface PcmSink {
    fun write(buffer: ByteArray, size: Int)

    /** A line for the periodic statistics. */
    fun describe(): String = ""
}

/**
 * Copies PCM from the FIFO into the sink until [stop]. [open] returns null while there is no
 * FIFO to read (PulseAudio not up yet or gone); the pump retries every [RETRY_MS] ms and
 * connects again when the writer goes away, so a restarted PulseAudio is picked up.
 */
class PcmPump(
    private val open: () -> PcmSource?,
    private val sink: PcmSink,
    private val log: (String) -> Unit,
    private val stats: (String) -> Unit = {},
    private val sleep: (Long) -> Unit = Thread::sleep,
    private val now: () -> Long = System::nanoTime,
) : Runnable {
    @Volatile private var stopped = false
    @Volatile var bytesRead = 0L
        private set
    @Volatile var connections = 0
        private set

    fun stop() {
        stopped = true
    }

    override fun run() {
        val buffer = ByteArray(GameAudioFormat.CHUNK_BYTES)
        var waitingLogged = false
        var nextStats = now() + STATS_NS
        // The loudest sample since the last statistics line: 0 means PulseAudio sent silence.
        var peak = 0
        while (!stopped) {
            val source = open()
            if (source == null) {
                if (!waitingLogged) log("audio: waiting for PulseAudio's FIFO")
                waitingLogged = true
                sleep(RETRY_MS)
                continue
            }
            waitingLogged = false
            connections++
            log("audio: connected to PulseAudio's FIFO (connection $connections)")
            source.use {
                var carry = 0
                while (!stopped) {
                    val n = it.read(buffer, carry, buffer.size - carry)
                    if (n < 0) {
                        log("audio: PulseAudio closed the FIFO after $bytesRead bytes")
                        break
                    }
                    if (n > 0) {
                        bytesRead += n
                        val total = carry + n
                        val whole = total - total % GameAudioFormat.BYTES_PER_FRAME
                        if (whole > 0) {
                            peak = maxOf(peak, GameAudioFormat.peak(buffer, whole))
                            sink.write(buffer, whole)
                        }
                        carry = total - whole
                        if (carry > 0) System.arraycopy(buffer, whole, buffer, 0, carry)
                    }
                    if (now() >= nextStats) {
                        nextStats = now() + STATS_NS
                        val read = "$bytesRead bytes read (${GameAudioFormat.millisFor(bytesRead)} ms)"
                        stats("audio: $read, peak $peak ${sink.describe()}".trimEnd())
                        peak = 0
                    }
                }
            }
            if (!stopped) sleep(RETRY_MS)
        }
    }

    companion object {
        const val RETRY_MS = 200L
        private const val STATS_NS = 5_000_000_000L
    }
}

/** PulseAudio's FIFO, opened non-blocking so the pump never hangs in open or read. */
class FifoSource private constructor(private val fd: FileDescriptor) : PcmSource {
    override fun read(buffer: ByteArray, offset: Int, length: Int): Int {
        try {
            val poll = StructPollfd().apply {
                fd = this@FifoSource.fd
                events = OsConstants.POLLIN.toShort()
            }
            Os.poll(arrayOf(poll), POLL_MS)
            // On a timeout too: no writer at all reads as end of file, a writer with nothing yet as EAGAIN.
            val n = Os.read(fd, buffer, offset, length)
            return if (n == 0) -1 else n
        } catch (error: ErrnoException) {
            if (error.errno == OsConstants.EAGAIN || error.errno == OsConstants.EINTR) return 0
            throw IOException("reading PulseAudio's FIFO: ${error.message}", error)
        }
    }

    override fun close() {
        try {
            Os.close(fd)
        } catch (_: ErrnoException) {
        }
    }

    companion object {
        private const val POLL_MS = 100
        private const val F_SETPIPE_SZ = 1031

        /**
         * The FIFO at [file], its stale contents dropped and its capacity cut to
         * [GameAudioFormat.PIPE_BYTES]; null when it is not there (yet).
         */
        fun open(file: File, log: (String) -> Unit): FifoSource? {
            val fd = try {
                Os.open(file.path, OsConstants.O_RDONLY or OsConstants.O_NONBLOCK or OsConstants.O_CLOEXEC, 0)
            } catch (_: ErrnoException) {
                return null
            }
            try {
                if (!OsConstants.S_ISFIFO(Os.fstat(fd).st_mode)) {
                    log("audio: ${file.path} is not a FIFO")
                    Os.close(fd)
                    return null
                }
                val scratch = ByteArray(GameAudioFormat.CHUNK_BYTES)
                try {
                    while (Os.read(fd, scratch, 0, scratch.size) > 0) {
                    }
                } catch (_: ErrnoException) {
                }
                try {
                    Os.fcntlInt(fd, F_SETPIPE_SZ, GameAudioFormat.PIPE_BYTES)
                } catch (error: ErrnoException) {
                    log("audio: FIFO capacity unchanged: ${error.message}")
                }
            } catch (_: ErrnoException) {
                try {
                    Os.close(fd)
                } catch (_: ErrnoException) {
                }
                return null
            }
            return FifoSource(fd)
        }
    }
}

/**
 * The game's sound: PulseAudio's pipe sink writes the mix into
 * a FIFO, and a thread of ours copies it into a low-latency `USAGE_GAME` AudioTrack. Started
 * once PulseAudio is up, stopped with it. Statistics go to logcat under `pd2-audio` every 5 s.
 */
class GameAudio(context: Context, private val fifo: File, private val log: (String) -> Unit) {
    private val audioManager = context.getSystemService(AudioManager::class.java)
    private var track: AudioTrack? = null
    private var pump: PcmPump? = null
    private var thread: Thread? = null
    private val attributes = AudioAttributes.Builder()
        .setUsage(AudioAttributes.USAGE_GAME)
        .setContentType(AudioAttributes.CONTENT_TYPE_MUSIC)
        .build()
    // Audio focus: guarded by this object; the focus listener runs on the main thread.
    private var focus: AudioFocusRequest? = null
    @Volatile private var focusLost = false
    private var focusLostForGood = false
    @Volatile private var inBackground = false
    @Volatile private var soundInBackground = true
    @Volatile private var gameRunning = false

    private fun applyVolume() {
        track?.setVolume(if (GameAudioMute.muted(focusLost, inBackground, soundInBackground)) 0f else 1f)
    }

    /**
     * The app went to ([inBackground]) or came back from the background; [soundInBackground] is
     * the Launcher setting; [gameRunning]: the Game client runs, so the track holds audio focus.
     */
    fun setBackground(inBackground: Boolean, soundInBackground: Boolean, gameRunning: Boolean) {
        // Back in the foreground: a focus lost for good is asked for again.
        if (this.inBackground && !inBackground) synchronized(this) { focusLostForGood = false }
        this.inBackground = inBackground
        this.soundInBackground = soundInBackground
        this.gameRunning = gameRunning
        updateFocus()
        applyVolume()
    }

    /** Asks for audio focus while the game runs in the foreground; gives it back once the game ended. */
    @Synchronized
    private fun updateFocus() {
        if (track == null) return
        if (!gameRunning) {
            abandonFocus()
            focusLost = false
            focusLostForGood = false
            return
        }
        if (!GameAudioFocus.request(gameRunning, inBackground, held = focus != null, focusLostForGood)) return
        val request = AudioFocusRequest.Builder(AudioManager.AUDIOFOCUS_GAIN)
            .setAudioAttributes(attributes)
            .setOnAudioFocusChangeListener(::onFocusChange)
            .build()
        val granted = audioManager.requestAudioFocus(request) == AudioManager.AUDIOFOCUS_REQUEST_GRANTED
        // Not granted (a call, say): muted, and asked again when the app next comes to the foreground.
        if (granted) focus = request
        focusLost = !granted
        focusLostForGood = !granted
        log("audio: focus ${if (granted) "granted" else "not granted"}")
    }

    /** Muted, not paused, while another app has focus: the FIFO keeps draining. */
    private fun onFocusChange(change: Int) {
        synchronized(this) {
            focusLost = change != AudioManager.AUDIOFOCUS_GAIN
            // Lost for good: no gain follows, so it is asked for again on the next foreground.
            if (change == AudioManager.AUDIOFOCUS_LOSS) {
                abandonFocus()
                focusLostForGood = true
            }
        }
        applyVolume()
    }

    @Synchronized
    private fun abandonFocus() {
        focus?.let { audioManager.abandonAudioFocusRequest(it) }
        focus = null
    }

    fun start() {
        if (thread != null) return
        val minBuffer = AudioTrack.getMinBufferSize(
            GameAudioFormat.RATE, AudioFormat.CHANNEL_OUT_STEREO, AudioFormat.ENCODING_PCM_16BIT,
        )
        val t = try {
            AudioTrack.Builder()
                .setAudioAttributes(attributes)
                .setAudioFormat(
                    AudioFormat.Builder()
                        .setEncoding(AudioFormat.ENCODING_PCM_16BIT)
                        .setSampleRate(GameAudioFormat.RATE)
                        .setChannelMask(AudioFormat.CHANNEL_OUT_STEREO)
                        .build(),
                )
                .setTransferMode(AudioTrack.MODE_STREAM)
                .setPerformanceMode(AudioTrack.PERFORMANCE_MODE_LOW_LATENCY)
                .setBufferSizeInBytes(GameAudioFormat.trackBufferBytes(minBuffer))
                .build()
        } catch (error: RuntimeException) {
            log("audio: no output track: ${error.message}")
            return
        }
        t.play()
        synchronized(this) { track = t }
        updateFocus()
        applyVolume()
        val sink = object : PcmSink {
            override fun write(buffer: ByteArray, size: Int) {
                var written = 0
                while (written < size) {
                    val n = t.write(buffer, written, size - written, AudioTrack.WRITE_BLOCKING)
                    if (n <= 0) return
                    written += n
                }
            }

            override fun describe() = "underruns ${t.underrunCount}"
        }
        val p = PcmPump({ FifoSource.open(fifo, log) }, sink, log, stats = { Log.i(TAG, it) })
        pump = p
        thread = Thread({
            Process.setThreadPriority(Process.THREAD_PRIORITY_URGENT_AUDIO)
            try {
                p.run()
            } catch (error: IOException) {
                log("audio: bridge ended: ${error.message}")
            }
        }, "pd2-audio").apply { start() }
        log(
            "audio: bridge started: ${GameAudioFormat.RATE} Hz s16le stereo, track buffer " +
                "${t.bufferSizeInFrames} frames, ${GameAudioFormat.PIPE_BYTES}-byte FIFO",
        )
    }

    /** Ends the copy thread and releases the track and audio focus (if held). */
    fun stop() {
        val t = thread ?: return
        pump?.stop()
        // stop() lets a blocked write return; the thread then sees the pump stopped.
        track?.stop()
        t.join(JOIN_MS)
        if (t.isAlive) log("audio: bridge thread did not end within $JOIN_MS ms")
        track?.release()
        synchronized(this) {
            abandonFocus()
            track = null
        }
        log("audio: bridge stopped after ${pump?.bytesRead ?: 0} bytes")
        pump = null
        thread = null
    }

    companion object {
        const val TAG = "pd2-audio"
        private const val JOIN_MS = 2_000L
    }
}
