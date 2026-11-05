package com.pd2launcher.app

import java.io.File

/**
 * The RADV route ([GraphicsRoute.RADV]): the game's Zink (the product Mesa's) on our RADV for
 * Samsung's Xclipse GPUs, which opens the GPU's DRM node (/dev/dri/renderD128, Samsung's sgpu
 * kernel driver, an amdgpu fork) itself; no server. RADV is the community tree radv-xclipse (MIT)
 * with our WSI patches (the present counter, the direct present into the app's buffers, the app's
 * clock), built by userland/build-radv.sh into the App userland's system-driver part.
 * Only on an Xclipse ([SystemDriverGpu.xclipse]); offered
 * to players there since the owner's decision of 2026-10-28 ([DriverChoice.RADV_FOR_PLAYERS],
 * [allowed]).
 */
object Radv {
    /** RADV in the App userland (the system-driver part); the unpack leaves it out where no route needs it ([DriverChoice.skippedFiles]). */
    const val LIBRARY = "usr/lib/aarch64-linux-gnu/libvulkan_radeon.so"

    /** Its ICD JSON, which userland.env's `PD2_APP_WINE_RADV_VK_DRIVER_FILES` names. */
    const val ICD = "usr/share/vulkan/icd.d/radeon_icd.aarch64.json"

    /**
     * Debug builds only, and only while [DriverChoice.RADV_FOR_PLAYERS] is false: with this file in
     * the app's `files/`, the RADV route comes first on a Samsung Xclipse GPU (the other routes stay
     * after it, for a failure of RADV). With the switch true (since 2026-10-28) RADV is offered
     * without it, and the file changes nothing; it forces no route ([SystemDriver.forced] does).
     * Read once per app process, with the phone driver's report: a change takes effect after the
     * app's next start. Release builds never read it.
     */
    const val DEBUG_FILE = "debug/radv"

    /** Whether the RADV route may be offered: to players ([forPlayers]), or with [DEBUG_FILE] in a debug build ([debug]). */
    fun allowed(files: File, debug: Boolean = BuildConfig.DEBUG, forPlayers: Boolean = DriverChoice.RADV_FOR_PLAYERS): Boolean =
        forPlayers || (debug && File(files, DEBUG_FILE).isFile)

    /** Whether the unpacked App userland at [userland] has RADV ([LIBRARY] and [ICD]). */
    fun filesPresent(userland: File): Boolean = File(userland, LIBRARY).isFile && File(userland, ICD).isFile
}

/**
 * Reads the engine's output (the game's own lines among it, line by line, [feed]) on the RADV
 * route for strong evidence that RADV could not run the game: Zink found no Vulkan device or could
 * not make one on it ("ZINK: failed to choose pdev", "ZINK: vkCreateDevice failed", Mesa
 * src/gallium/drivers/zink/zink_screen.c), or GLX could not load Zink at all ("failed to load
 * driver: zink", src/glx). Each such line counts once per game ([reset] at its start).
 * A game that draws with GDI ([gdiGame], the session's [GdiGame]) has none, also from a line
 * that came before the engine said so in the same Play.
 */
class RadvFailureWatch(private val gdiGame: GdiGame = GdiGame()) {
    private var seen: String? = null

    /** Whether [line] is the first sign in this game that RADV could not make its device. */
    @Synchronized
    fun feed(line: String): Boolean {
        if (seen != null || gdiGame.gdi) return false
        if (SIGNS.none { line.contains(it) }) return false
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
        val SIGNS = listOf("ZINK: failed to choose pdev", "ZINK: vkCreateDevice failed", "failed to load driver: zink")
    }
}
