package com.pd2launcher.app

import java.io.File

/**
 * The built-in route ([GraphicsRoute.BUILT_IN]): our Turnip on an Adreno, our PanVK on a CSF Mali.
 * Its failures ([BuiltInFailureWatch], and the session's no-frame and frozen-picture rules) make
 * Play tell of the error ([PhoneDriverFailure.tells]); the route stays.
 */
object BuiltInDriver {
    /**
     * Debug builds only: with this file in the app's `files/`, each game that ends on the built-in
     * route counts as a failure of it on strong evidence, as a line of [BuiltInFailureWatch] would,
     * so a test can show Play's error line and its button on a phone whose driver works. The route
     * stays. It changes nothing for a forced route ([SystemDriver.forced]). Read at each game's end.
     * Release builds never read it.
     */
    const val DEBUG_FILE = "debug/built-in-fails"

    /** Whether [DEBUG_FILE] is in [files] in a debug build ([debug]). */
    fun failsOnPurpose(files: File, debug: Boolean = BuildConfig.DEBUG): Boolean = debug && File(files, DEBUG_FILE).isFile

    /**
     * Debug builds only: with this file in the app's `files/`, the app reads the phone's own drivers
     * and leaves files out of the unpack as a release build does ([debugRules]), so a test can show
     * a release build's probe and unpack on a phone. It goes in before the app's process starts: the
     * phone drivers' reports and the options are read once there, and a change of the unpack's files
     * takes effect at the next unpack. A pick of the Phone driver on Settings → Display → GPU also
     * reads the phone's own drivers. Release builds never read it.
     */
    const val RELEASE_RULES_FILE = "debug/built-in-release-rules"

    /** Whether the debug build's rules apply to the probe and the unpack: a debug build ([debug]) without [RELEASE_RULES_FILE]. */
    fun debugRules(files: File, debug: Boolean = BuildConfig.DEBUG): Boolean = debug && !File(files, RELEASE_RULES_FILE).isFile
}

/**
 * Reads the engine's output (the game's own lines among it, line by line, [feed]) on the built-in
 * route for strong evidence that the driver failed in the game: Zink found no Vulkan device or
 * could not make one on it ("ZINK: failed to choose pdev", "ZINK: vkCreateDevice failed", Mesa
 * src/gallium/drivers/zink/zink_screen.c), GLX could not load Zink at all ("failed to load driver:
 * zink", src/glx); the GPU context was lost or the GPU hung: Zink's lost device ("zink: DEVICE
 * LOST!", zink_screen.h, and each Zink line with `VK_ERROR_DEVICE_LOST`, such as "ZINK:
 * vkQueueSubmit failed (VK_ERROR_DEVICE_LOST)", zink_batch.c; also a line of the driver's own
 * with that result, which a release Mesa prints only with Vulkan's debug output on), or a fatal
 * error of our PanVK's queue group on kbase ("kbase: queue group <n> error type <0 to 3>:", the
 * group's or a queue's FATAL error, a TIMEOUT or a TILER_HEAP_OOM, and "kbase: submit to group <n>
 * after its fatal error", mesa-kbase's kbase_kmod_sync.c and kbase_kmod_csf.c; the queue fault,
 * error type 4, is not fatal on its own). A loading screen or a crash of the game before its first
 * frame is no such sign: a game with no frame counts only by the no-frame rule
 * ([PhoneDriverFailure.failed]), after [PhoneDriverFailure.NO_FRAME_GAMES_TO_LEAVE] games in a
 * row. Each such line counts once per game ([reset] at its start).
 * A game that draws with GDI ([gdiGame], the session's [GdiGame]) has none, also from a line
 * that came before the engine said so in the same Play.
 */
class BuiltInFailureWatch(private val gdiGame: GdiGame = GdiGame()) {
    private var seen: String? = null

    /** Whether [line] is the first sign in this game that the built-in driver failed. */
    @Synchronized
    fun feed(line: String): Boolean {
        if (seen != null || gdiGame.gdi) return false
        if (SIGNS.none { line.contains(it) } && !GROUP_FATAL.containsMatchIn(line)) return false
        seen = line.trim()
        return true
    }

    /** The line that showed it in this game, or null. */
    @get:Synchronized
    val failure: String? get() = seen?.takeUnless { gdiGame.gdi }

    /** A new game: nothing seen yet. */
    @Synchronized
    fun reset() {
        seen = null
    }

    private companion object {
        val SIGNS = listOf(
            "ZINK: failed to choose pdev", "ZINK: vkCreateDevice failed", "failed to load driver: zink",
            "zink: DEVICE LOST!", "VK_ERROR_DEVICE_LOST", "after its fatal error",
        )

        /** A fatal error of a queue group: error types 0 to 3 of kbase's uAPI (4, a queue fault, is not fatal). */
        val GROUP_FATAL = Regex("""kbase: queue group \d+ error type [0-3]:""")
    }
}
