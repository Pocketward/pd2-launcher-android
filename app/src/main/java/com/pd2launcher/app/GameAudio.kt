package com.pd2launcher.app

import android.content.Context
import android.media.AudioAttributes
import android.media.AudioFocusRequest
import android.media.AudioManager
import android.util.Log
import java.io.File

/**
 * The game's sound format and its ALSA configuration. Wine's winealsa.drv writes the game's
 * DirectSound mix through our ALSA plugin (runtime/pd2-audio/pcm_pd2.c) into a ring file, float
 * stereo at 22.05 kHz: Diablo II's own rate, so DirectSound mixes the game's 22.05 kHz sounds
 * without resampling them (its resampler is 32-bit x86 code, which Box64 emulates); the app
 * resamples to the phone's rate natively (game_audio.c).
 */
object GameAudioFormat {
    const val RATE = 22_050

    /**
     * The plugin's ALSA period in frames. Wine's DirectSound keeps three times winealsa's latency
     * (this period + Wine's 10 ms period + 4 ms) queued ahead of the phone: 64 frames give 50.6 ms.
     */
    const val PERIOD_FRAMES = 64

    fun millisFor(frames: Long): Long = frames * 1000 / RATE

    /**
     * The ALSA configuration Wine reads from the session's runtime folder (`asound.conf` in
     * `XDG_RUNTIME_DIR`, Wine patch 0003): the default PCM is the [ring] through the [plugin].
     */
    fun alsaConfig(ring: File, plugin: File): String {
        for (path in listOf(ring.path, plugin.path)) {
            require('"' !in path && '\\' !in path && '\n' !in path) { "not an ALSA string: $path" }
        }
        return """
            |pcm.!default {
            |    type pd2
            |    ring "${ring.path}"
            |    period $PERIOD_FRAMES
            |}
            |pcm_type.pd2 {
            |    lib "${plugin.path}"
            |}
            |""".trimMargin()
    }
}

/** When the game's sound is silent: always muted, never paused, so the ring keeps draining. */
object GameAudioMute {
    /** Muted while another app holds audio focus, or while the app is in the background with "Sound in background" off. */
    fun muted(focusLost: Boolean, inBackground: Boolean, soundInBackground: Boolean): Boolean =
        focusLost || (inBackground && !soundInBackground)
}

/**
 * When the game's sound asks for audio focus: only while the Game client runs (an install's
 * Wine never pauses the player's music), with the app in the foreground, and not already held.
 * A focus lost for good is asked for again the next time the app comes back to the foreground.
 */
object GameAudioFocus {
    /** [lostForGood]: lost (or refused) since the app last came to the foreground. */
    fun request(gameRunning: Boolean, inBackground: Boolean, held: Boolean, lostForGood: Boolean): Boolean =
        gameRunning && !inBackground && !held && !lostForGood
}

/** The native player of the game's sound (libgameaudio.so, game_audio.c). */
object GameSound {
    init {
        System.loadLibrary("gameaudio")
    }

    /** Makes the ring file at [ring] and starts playing it; 0 when there is no output. */
    @JvmStatic external fun nativeStart(ring: String): Long

    @JvmStatic external fun nativeSetMuted(handle: Long, muted: Boolean)

    /** See game_audio.c's nativeStats. */
    @JvmStatic external fun nativeStats(handle: Long): LongArray

    @JvmStatic external fun nativeStop(handle: Long)
}

/**
 * The game's sound: the native player ([GameSound]) plays the ring that the game's Wine writes,
 * on a low-latency `USAGE_GAME` stream. Started with the game environment, before any Wine
 * process of it, and stopped with it. Statistics go to logcat under `pd2-audio` every 5 s.
 * [ring] and [config] are in the session's runtime folder; [plugin] is the runtime's ALSA plugin.
 */
class GameAudio(
    context: Context,
    private val ring: File,
    private val config: File,
    private val plugin: File,
    private val log: (String) -> Unit,
) {
    private val audioManager = context.getSystemService(AudioManager::class.java)
    @Volatile private var handle = 0L
    private var stats: Thread? = null
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

    /** Under the lock: [stop] clears [handle] under it too, so the player is never used once freed. */
    @Synchronized
    private fun applyVolume() {
        val h = handle
        if (h != 0L) GameSound.nativeSetMuted(h, GameAudioMute.muted(focusLost, inBackground, soundInBackground))
    }

    /**
     * The app went to ([inBackground]) or came back from the background; [soundInBackground] is
     * the Launcher setting; [gameRunning]: the Game client runs, so the sound holds audio focus.
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
        if (handle == 0L) return
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

    /** Muted, not paused, while another app has focus: the ring keeps draining. */
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

    /**
     * Starts the player, then writes the ALSA configuration that names its ring. Without an output
     * there is no configuration and no ring: Wine finds no sound device and the game runs silent.
     */
    fun start() {
        if (handle != 0L) return
        config.delete()
        // Made before the player starts: a path ALSA cannot take leaves no player behind.
        val text = try {
            GameAudioFormat.alsaConfig(ring, plugin)
        } catch (e: IllegalArgumentException) {
            log("audio: no ALSA configuration (${e.message}): the game is silent")
            return
        }
        val h = GameSound.nativeStart(ring.path)
        if (h == 0L) {
            ring.delete()
            log("audio: no output: the game is silent")
            return
        }
        try {
            config.writeText(text)
        } catch (e: Exception) {
            GameSound.nativeStop(h)
            ring.delete()
            config.delete()
            log("audio: no ALSA configuration (${e.message}): the game is silent")
            return
        }
        synchronized(this) { handle = h }
        updateFocus()
        applyVolume()
        stats = Thread({ report(h) }, "pd2-audio-stats").apply {
            isDaemon = true
            start()
        }
        log("audio: started: the game's sound goes from Wine's ALSA output through ${ring.name} to the phone")
    }

    /**
     * Every 5 s: the frames read, the loudest sample, short reads (the ring ran dry while the game's
     * stream played), the stream's own underruns, its buffer, the output delay and the cushion.
     */
    private fun report(h: Long) {
        try {
            while (handle == h) {
                Thread.sleep(STATS_MS)
                if (handle != h) break
                val s = GameSound.nativeStats(h)
                val delay = if (s[7] >= 0) "${s[7] / 1000} ms" else "unknown"
                Log.i(
                    TAG,
                    "audio: ${s[0]} frames read (${GameAudioFormat.millisFor(s[0])} ms), peak ${s[6]}, short reads ${s[1]}, " +
                        "stream underruns ${s[2]}, buffer ${s[3]} frames (burst ${s[4]} at ${s[5]} Hz), output delay $delay, " +
                        "restarts ${s[8]}, cushion ${GameAudioFormat.millisFor(s[9])} ms",
                )
            }
        } catch (_: InterruptedException) {
        }
    }

    /** Stops the player and gives back audio focus (if held). */
    fun stop() {
        val h = handle
        if (h == 0L) return
        synchronized(this) {
            handle = 0L
            abandonFocus()
        }
        // The statistics thread ends before the player goes: it must not read a freed player.
        stats?.interrupt()
        stats?.join()
        stats = null
        config.delete()
        val s = GameSound.nativeStats(h)
        GameSound.nativeStop(h)
        ring.delete()
        log("audio: stopped after ${s[0]} frames, short reads ${s[1]}, stream underruns ${s[2]}")
    }

    companion object {
        const val TAG = "pd2-audio"
        private const val STATS_MS = 5_000L
    }
}
