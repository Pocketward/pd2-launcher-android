package com.pd2launcher.app

import java.io.File

/**
 * The mali-panvk route ([GraphicsRoute.MALI_PANVK]): the game's Zink (the product Mesa's) on the
 * community PanVK tree mali-panvk (MIT) for Arm Mali GPUs on kbase's job manager (Bifrost v6/v7,
 * Valhall v9, such as the Mali-G52, G57, G68, G77, G78), which opens `/dev/mali0` itself; no server.
 * Built by userland/build-mali-panvk.sh with our WSI patches (the present counter, the direct
 * present into the app's buffers, the app's clock) and our fixes for the tree, into the App
 * userland's system-driver part, beside our PanVK, which stays the driver of the CSF Malis. Only
 * on a job-manager Mali that the tree knows ([maliPanvkKnows]); offered to players while
 * [DriverChoice.MALI_PANVK_FOR_PLAYERS] is true ([allowed]).
 */
object MaliPanvk {
    /**
     * mali-panvk in the App userland (the system-driver part), in a folder of its own beside our
     * PanVK; the unpack leaves it out where no route needs it ([DriverChoice.skippedFiles]).
     */
    const val LIBRARY = "usr/lib/aarch64-linux-gnu/mali-panvk/libvulkan_panfrost.so"

    /** Its ICD JSON, outside icd.d, which userland.env's `PD2_APP_WINE_MALI_PANVK_VK_DRIVER_FILES` names. */
    const val ICD = "usr/share/mali-panvk/mali_panvk_icd.aarch64.json"

    /**
     * Debug builds only, and only while [DriverChoice.MALI_PANVK_FOR_PLAYERS] is false: with this
     * file in the app's `files/`, the mali-panvk route comes first on a job-manager Mali that the
     * tree knows (the other routes stay after it, for a failure of mali-panvk). With the switch
     * true the route is offered without it, and the file changes nothing; it forces no route
     * ([SystemDriver.forced] does). Read once per app process: a change takes effect after the
     * app's next start. Release builds never read it.
     */
    const val DEBUG_FILE = "debug/mali-panvk"

    /** Whether the mali-panvk route may be offered: to players ([forPlayers]), or with [DEBUG_FILE] in a debug build ([debug]). */
    fun allowed(files: File, debug: Boolean = BuildConfig.DEBUG, forPlayers: Boolean = DriverChoice.MALI_PANVK_FOR_PLAYERS): Boolean =
        forPlayers || (debug && File(files, DEBUG_FILE).isFile)

    /** Whether the unpacked App userland at [userland] has mali-panvk ([LIBRARY] and [ICD]). */
    fun filesPresent(userland: File): Boolean = File(userland, LIBRARY).isFile && File(userland, ICD).isFile

    /**
     * The JM Mali that the route is for: [gpu] itself (no report of the phone's own driver), or the
     * GPU its kernel driver named beside that report ([SystemDriverGpu.kernel]); null where it is
     * neither a job-manager Mali nor one that mali-panvk knows.
     */
    fun gpu(gpu: HostGpu?, phone: SystemDriverGpu?): MaliGpu? =
        ((gpu as? MaliGpu) ?: (phone?.kernel as? MaliGpu))?.takeIf { it.jobManager && maliPanvkKnows(it) }
}

/**
 * Reads the engine's output (the game's own lines among it, line by line, [feed]) on the mali-panvk
 * route for strong evidence that the driver failed in the game: Zink found no Vulkan device or
 * could not make one on it ("ZINK: failed to choose pdev", "ZINK: vkCreateDevice failed", Mesa
 * src/gallium/drivers/zink/zink_screen.c), GLX could not load Zink at all ("failed to load driver:
 * zink", src/glx), or the GPU faulted: a submission did not complete (mali-panvk's
 * src/panfrost/vulkan/jm/panvk_vX_gpu_queue.c, after "kbase: atom N finished with event code X,
 * not DONE"), which our patch mali-panvk/0005 makes a lost device, or the queue's work sent no
 * event in the time limit of our patches mali-panvk/0006 and 0009 ("panvk: the queue's work did
 * not report", at a wait for that work). Not alone: the session's
 * no-frame and frozen-picture rules ([PhoneDriverFailure.failed], [FrozenPicture]) count on this
 * route too, as on a job-manager Mali a fault can also show as a still or a wrong picture. Each
 * such line counts once per game ([reset] at its start).
 * A game that draws with GDI ([gdiGame], the session's [GdiGame]) has none, also from a line
 * that came before the engine said so in the same Play.
 */
class MaliPanvkFailureWatch(private val gdiGame: GdiGame = GdiGame()) {
    private var seen: String? = null

    /** Whether [line] is the first sign in this game that mali-panvk failed. */
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
        val SIGNS = listOf(
            "ZINK: failed to choose pdev", "ZINK: vkCreateDevice failed", "failed to load driver: zink",
            "panvk: the kbase submission did not complete", "panvk: the queue's work did not report",
        )
    }
}
