package com.pd2launcher.app

import android.util.Log
import java.io.File

/**
 * The system-driver mode: the game's GPU path through the phone's own Vulkan driver. Zink draws
 * through Mesa's Venus ICD, which sends Vulkan over vtest's socket to virglrenderer's server in
 * the APK's native-library folder; the server runs it on the phone's own driver
 * Which Zink ([Zink]): the product Mesa's (main, at
 * userland/config.sh's MESA_COMMIT) wherever the driver meets its needs, else the older Mesa
 * 25.1.9 as glvnd's vendor `pd2sysdrv`
 * (libGLX_pd2sysdrv.so.0 and its libgallium).
 */
object SystemDriver {
    /** virglrenderer's vtest server and its render server, bionic, in the APK (build.sh). */
    const val SERVER_LIBRARY = "libvirgl_test_server.so"
    const val RENDER_SERVER_LIBRARY = "libvirgl_render_server.so"

    /**
     * The VirGL route's server ([GraphicsRoute.VIRGL]): a second vtest server, with virglrenderer's
     * OpenGL renderer (vrend) on the phone's own OpenGL ES, bionic, in the APK (build.sh).
     */
    const val GL_SERVER_LIBRARY = "libvirgl_test_server_gl.so"

    /**
     * The mode's largest files in the App userland, which the unpack leaves out in release builds
     * where the mode cannot run (the Venus ICD and the older GL), or the older GL alone where the
     * product Mesa's Zink runs it. The GL library's version is userland/config.sh's
     * MESA_SYSDRV_VERSION (a JVM test checks it).
     */
    const val VENUS_LIBRARY = "usr/lib/aarch64-linux-gnu/libvulkan_virtio.so"
    const val GL_LIBRARY = "usr/lib/aarch64-linux-gnu/libgallium-25.1.9.so"
    val userlandFiles: Set<String> = setOf(VENUS_LIBRARY, GL_LIBRARY)

    /**
     * The libhybris route's files ([GraphicsRoute.LIBHYBRIS], userland build-hybris.sh, in the
     * system-driver part): our Vulkan ICD and its JSON, libhybris-common.so.1 and its linker
     * plugin. The unpack leaves the libraries out in release builds where the route cannot run
     * ([DriverChoice.skippedFiles]).
     */
    const val HYBRIS_ICD = "usr/lib/aarch64-linux-gnu/libvulkan_pd2hybris.so"
    const val HYBRIS_ICD_JSON = "usr/share/vulkan/icd.d/pd2hybris_icd.aarch64.json"
    const val HYBRIS_COMMON = "usr/lib/aarch64-linux-gnu/libhybris-common.so.1"
    const val HYBRIS_LINKER = "usr/lib/aarch64-linux-gnu/libhybris/linker/q.so"
    val hybrisFiles: Set<String> = setOf(HYBRIS_ICD, HYBRIS_COMMON, HYBRIS_LINKER)

    /** Whether the unpacked App userland at [userland] has the libhybris route's files. */
    fun hybrisFilesPresent(userland: File): Boolean =
        (hybrisFiles + HYBRIS_ICD_JSON).all { File(userland, it).isFile }

    /**
     * Whether the APK carries the libhybris route's files: they are in the userland's
     * system-driver part ([UserlandAssets.SYSTEM_DRIVER]); an APK built without the part (build.sh
     * PD2_SYSTEM_DRIVER=0) has no such route. No server: the route needs none.
     */
    fun hybrisInApk(context: android.content.Context): Boolean = UserlandAssets.carries(context, UserlandAssets.SYSTEM_DRIVER)

    /**
     * The Zink that the mode uses: [CURRENT], the product Mesa's (main; glvnd vendor `mesa`),
     * where the phone's driver meets its needs; [OLDER], Mesa 25.1.9's second stack (`pd2sysdrv`,
     * userland.env's `PD2_APP_WINE_SYSDRV_OLDER_ZINK_*` values), only where the product Mesa's
     * Zink refuses the driver.
     */
    enum class Zink { CURRENT, OLDER }

    /**
     * Debug builds only: with this file in the app's `files/`, the mode's Venus route is used on
     * any GPU, also on an Adreno or a Mali that Turnip or PanVK drives, for tests of the mode on
     * our phones. Its `NAME=value` lines go to the vtest server (GameEnvironment). Release builds
     * never read it.
     */
    const val FORCE_FILE = "debug/system-driver"

    /** The same for the VirGL route ([GraphicsRoute.VIRGL]); it wins over [FORCE_FILE]. */
    const val FORCE_VIRGL_FILE = "debug/system-driver-virgl"

    /**
     * The same for the libhybris route ([GraphicsRoute.LIBHYBRIS]); it wins over the other two.
     * Its `NAME=value` lines go to the game's Wine processes (there is no server), such as
     * `PD2_HYBRIS_DEBUG=1` or `PD2_HYBRIS_TEST_FAIL=1` (the ICD then acts as if the phone's driver
     * did not load).
     */
    const val FORCE_LIBHYBRIS_FILE = "debug/system-driver-libhybris"

    /**
     * The same for the mali-panvk route ([GraphicsRoute.MALI_PANVK]), on any Mali, a CSF one too,
     * which the route leaves to our PanVK ([MaliPanvk.gpu]): the A/B of mali-panvk against our
     * PanVK on the CSF Malis (issue 23). It wins over the other three.
     */
    const val FORCE_MALI_PANVK_FILE = "debug/system-driver-mali-panvk"

    /**
     * The route that a debug build ([debug]) forces: [GraphicsRoute.MALI_PANVK] with
     * [FORCE_MALI_PANVK_FILE], else [GraphicsRoute.LIBHYBRIS] with [FORCE_LIBHYBRIS_FILE], else
     * [GraphicsRoute.VIRGL] with [FORCE_VIRGL_FILE], else [GraphicsRoute.VENUS] with
     * [FORCE_FILE]; null without any.
     */
    fun forced(files: File, debug: Boolean = BuildConfig.DEBUG): GraphicsRoute? = when {
        !debug -> null
        File(files, FORCE_MALI_PANVK_FILE).isFile -> GraphicsRoute.MALI_PANVK
        File(files, FORCE_LIBHYBRIS_FILE).isFile -> GraphicsRoute.LIBHYBRIS
        File(files, FORCE_VIRGL_FILE).isFile -> GraphicsRoute.VIRGL
        File(files, FORCE_FILE).isFile -> GraphicsRoute.VENUS
        else -> null
    }

    /** The debug file that forces [route] ([forced]); its `NAME=value` lines go to that route's server (libhybris: the game). */
    fun forceFile(route: GraphicsRoute): String = when (route) {
        GraphicsRoute.VIRGL -> FORCE_VIRGL_FILE
        GraphicsRoute.LIBHYBRIS -> FORCE_LIBHYBRIS_FILE
        GraphicsRoute.MALI_PANVK -> FORCE_MALI_PANVK_FILE
        else -> FORCE_FILE
    }

    /** The vtest socket in the runtime folder; userland.env's `PD2_APP_WINE_SYSDRV_VTEST_SOCKET_NAME` names the same path. */
    fun socket(runtimeDir: File) = File(runtimeDir, "vtest.sock")

    /**
     * Whether the unpacked App userland at [userland] has the mode's files for [zink]: the
     * release unpack leaves them out where the mode could not run ([DriverChoice.skippedFiles]).
     */
    fun filesPresent(userland: File, zink: Zink): Boolean =
        File(userland, VENUS_LIBRARY).isFile && (zink == Zink.CURRENT || File(userland, GL_LIBRARY).isFile)

    /** Whether the APK carries the server: both programs in [nativeLibraryDir]. */
    fun serverInApk(nativeLibraryDir: String): Boolean =
        File(nativeLibraryDir, SERVER_LIBRARY).isFile && File(nativeLibraryDir, RENDER_SERVER_LIBRARY).isFile

    /** Whether the APK carries the VirGL route's server ([GL_SERVER_LIBRARY]) in [nativeLibraryDir]. */
    fun glServerInApk(nativeLibraryDir: String): Boolean = File(nativeLibraryDir, GL_SERVER_LIBRARY).isFile

    /**
     * Whether the APK carries the mode's parts: the server ([serverInApk]) and the App userland's
     * part ([UserlandAssets.SYSTEM_DRIVER]); an APK built without them (build.sh
     * PD2_SYSTEM_DRIVER=0) has neither.
     */
    fun inApk(context: android.content.Context): Boolean =
        serverInApk(context.applicationInfo.nativeLibraryDir) && UserlandAssets.carries(context, UserlandAssets.SYSTEM_DRIVER)

    /**
     * The dma-buf heaps the server's patch 0002 allocates host-visible memory from, when the driver
     * imports dma-bufs but exports none (android/devdriver/patches/virglrenderer/0002): the
     * uncached heap first.
     */
    val HEAPS = listOf("/dev/dma_heap/system-uncached", "/dev/dma_heap/system")
}

/**
 * Reads the server's output (the vtest server's, with its render servers', line by line, [feed])
 * for a game context that the server gave up: a command that the phone's driver failed (a refused
 * pipeline, a lost device after a GPU hang) makes it stop that client ("failed to dispatch context
 * op", virglrenderer server/render_context.c), and it then destroys the context, which names its
 * process ("destroying context <n> (Game.exe) ..."). Only the game's context counts, not one of
 * Wine's helper processes.
 */
class ContextLossWatch {
    private var failed = false

    /** Whether [line] completes a lost context of the game. */
    @Synchronized
    fun feed(line: String): Boolean {
        if (line.contains("failed to dispatch context op")) {
            failed = true
            return false
        }
        if (failed && line.contains("destroying context")) {
            failed = false
            return line.contains("(Game.exe)")
        }
        return false
    }
}

/**
 * Reads the VirGL route's server output ([feed], line by line) for a game context that it gave
 * up: vrend reports an error of a context ("<function>: context error reported <id> "<name>"
 * <error> <value>", virglrenderer src/vrend/vrend_renderer.c vrend_report_context_error_internal)
 * and from then on draws nothing more for it; later commands of that context can report more
 * errors, which count once. With the build's CHECK_GL_ERRORS (meson's check-gl-errors, on by
 * default) an OpenGL ES error of the phone's driver in the context counts too. The name is the
 * client's process: only the game's context counts, not one of Wine's helper processes.
 */
class VirglContextWatch {
    private val lost = mutableSetOf<String>()

    /** Whether [line] reports a newly lost context of the game. */
    @Synchronized
    fun feed(line: String): Boolean {
        val match = ERROR.find(line) ?: return false
        val (id, name) = match.destructured
        return name == "Game.exe" && lost.add(id)
    }

    private companion object {
        val ERROR = Regex("""context error reported (\d+) "([^"]*)"""")
    }
}

/** How virglrenderer's server can share the driver's host-visible memory with the game's Venus. */
enum class SharedMemory {
    /** The driver exports host-visible memory as a dma-buf: virglrenderer's own path. */
    DMA_BUF_EXPORT,

    /** The driver imports dma-bufs and exports nothing, and a dma-buf heap is readable: our patch 0002. */
    DMA_BUF_HEAP,

    /**
     * Neither, but the driver imports AHardwareBuffers (a Qualcomm driver has no dma-buf at all):
     * our patch 0005 allocates host-visible memory as AHardwareBuffer BLOBs.
     */
    AHB,

    /** None of these: the server cannot give the game its memory. */
    NONE,
}

/**
 * The phone's own Vulkan driver as [SystemDriverProbe] reads it in the app's process: its
 * device [name], Vulkan [apiVersion] (major, minor), whether the older Zink 25.1.9 can start on it
 * ([zinkStarts]) and how the server shares memory with it ([memory]); [kernel] is the GPU as its
 * kernel driver reports it when that answers (a Mali that PanVK does not drive), else null.
 */
data class SystemDriverGpu(
    override val name: String,
    val apiVersion: Pair<Int, Int>,
    val zinkStarts: Boolean,
    val memory: SharedMemory,
    val kernel: HostGpu? = null,
    /** The app's buffers can take the game's frames with no copy (dma-buf import, DRM format modifiers). */
    val directPresent: Boolean = false,
    /**
     * The product Mesa's Zink (main; its hard needs are 26.2.3's) can start on it: the mode then
     * uses that GL stack and not the older one ([SystemDriver.Zink]).
     */
    val zinkCurrent: Boolean = false,
    /**
     * The driver's VkDriverId (VkPhysicalDeviceDriverProperties); null where the probe cannot read
     * it (Vulkan 1.1 without VK_KHR_driver_properties: Venus then gives Zink its own id, MESA_VENUS).
     */
    val driverId: Int? = null,
    /** VkPhysicalDeviceProperties.driverVersion as the driver gives it (its vendor's own encoding); null from an older probe. */
    val driverVersion: Long? = null,
    /** The geometryShader feature; null from an older probe. */
    val geometryShader: Boolean? = null,
    /**
     * Zink's base requirements as the probe reads them, by the names that Zink prints
     * ([ZINK_BASE_OLDER], [ZINK_BASE_CURRENT]); an item that the probe did not read (an older
     * probe) is not in it and counts as there.
     */
    val zinkBase: Map<String, Boolean> = emptyMap(),
) : HostGpu {
    /** Venus needs Vulkan 1.1 (Mesa vn_instance.h VN_MIN_RENDERER_VERSION; virglrenderer vkr_instance.c). */
    val vulkan11: Boolean get() = apiVersion.first > 1 || (apiVersion.first == 1 && apiVersion.second >= 1)

    /** The Zink the mode uses here: the product Mesa's where it starts, else the older one. */
    val zink: SystemDriver.Zink get() = if (zinkCurrent) SystemDriver.Zink.CURRENT else SystemDriver.Zink.OLDER

    /** Imagination's own driver for its PowerVR GPUs. */
    val powerVr: Boolean get() = driverId == IMAGINATION_PROPRIETARY

    /**
     * Samsung's own driver for its Xclipse GPUs: by the driver id, or by the device's name where
     * the probe has no id. Zink over Venus read integer vertex attributes as 0 there (Galaxy S24
     * Exynos, Xclipse 940, driver 24.0.545, Zink 25.1.9 and 26.1.8) before the Venus server's
     * patch 0009, and D2GL's text takes its layer and flags from such attributes; VirGL with the
     * integer types of server patch virglrenderer-gl/0004 reads them correctly. Its route order is
     * one switch, [DriverChoice.XCLIPSE_VIRGL_FIRST] ([DriverOptions.virglFirst]): VirGL first.
     */
    val xclipse: Boolean get() = driverId == SAMSUNG_PROPRIETARY || name.contains("Xclipse", ignoreCase = true)

    /**
     * Qualcomm's own driver for its Adreno GPUs: by the driver id, or by "Adreno" in the device's
     * name where the probe has no id. The phone's own driver is the route only where the built-in
     * Turnip cannot run the GPU (an Adreno 710, until Mesa 26.3), or where the player picks it. On an Adreno 740 (Qualcomm
     * driver 512.x) VirGL with the integer types of server patch virglrenderer-gl/0004
     * draws PD2 right (about 45 fps), and Venus draws the text wrong (black boxes, missing
     * glyphs). Its route order is one switch, [DriverChoice.QUALCOMM_VIRGL_FIRST]
     * ([DriverOptions.virglFirst]).
     */
    val qualcomm: Boolean get() = driverId == QUALCOMM_PROPRIETARY || name.contains("Adreno", ignoreCase = true)

    /**
     * Arm's own driver for its Mali GPUs: by the driver id, or by "Mali" in the device's name
     * where the probe has no id. The phone's own driver is the route only where the built-in
     * PanVK cannot run the GPU (Mali-G52, G57, G68, G77 and older). Remote facts of 2026-10-24:
     * Venus failed on all 5 such phones, VirGL drew right on 4 of 5. Its route order is one
     * switch, [DriverChoice.MALI_VIRGL_FIRST] ([DriverOptions.virglFirst]).
     */
    val mali: Boolean get() = driverId == ARM_PROPRIETARY || name.contains("Mali", ignoreCase = true)

    /**
     * Imagination's own driver without geometry shaders (PowerVR GE8320 and GE8322): no Venus
     * route. Mesa 25.1.9's Zink fails on it (zink_screen.c:3411-3415), also through Venus, which
     * gives Zink the driver's real id (VK_KHR_maintenance7's layered properties,
     * vn_physical_device.c); the product Mesa's Zink needs robustness2, which these drivers do not
     * have either. Such a phone goes straight to VirGL where its OpenGL ES driver can run it.
     */
    val powerVrWithoutGeometryShaders: Boolean get() = powerVr && geometryShader == false

    /**
     * Venus is less likely to run on Imagination's own driver older than DDK 24.1: Zink main
     * expects such drivers to fail ("too old to be supported, expect failure", zink_screen.c, the
     * check of the device's driverVersion against [POWERVR_DDK_24_1]), but through Venus that
     * check reads Mesa's own version, which Venus puts there (vn_physical_device.c), so only this
     * probe can tell. The route stays Venus, as Zink itself goes on with such a driver; the device
     * check's finding and the probe's log say so; the player can pick VirGL on the GPU page. Never
     * where Venus is no route anyway
     * ([powerVrWithoutGeometryShaders]).
     */
    val venusLowConfidence: Boolean
        get() = powerVr && !powerVrWithoutGeometryShaders && driverVersion != null && driverVersion < POWERVR_DDK_24_1

    /** What keeps the mode from running on it, for the device check; null when nothing does. */
    fun problem(): String? = when {
        !vulkan11 -> "its Vulkan driver is version ${apiVersion.first}.${apiVersion.second}, 1.1 is needed"
        powerVrWithoutGeometryShaders -> "its Vulkan driver (PowerVR, no geometry shaders) is one that OpenGL on Vulkan (Zink) refuses"
        !zinkStarts && !zinkCurrent -> "its Vulkan driver lacks what OpenGL on Vulkan (Zink) needs (timeline semaphores, imageless framebuffers, render pass 2 or descriptor update templates)"
        // Fact (motorola edge 40 Mali-G77 r32p1, Galaxy A32 Mali-G52 r26p0, 2026-10-24): with no readable
        // dma-buf heap, the AHardwareBuffer path (server patch 0005) finds no host-visible memory type
        // that imports an AHardwareBuffer, and Zink waits until its time limit. The probe makes no
        // device and reads no memory types, so it cannot test this; on Mali the path is not offered
        // (a guess in scope: only these 2 phones show it; Qualcomm's AHardwareBuffer path is unchanged).
        memory == SharedMemory.AHB && mali -> "its Vulkan driver (Arm Mali) has no readable /dev/dma_heap, and its AHardwareBuffer memory has no host-visible type that the server can import"
        memory == SharedMemory.NONE -> "its Vulkan driver cannot share memory with the game as the app needs (no dma-buf, no AHardwareBuffer import)"
        else -> zinkBaseProblem()
    }

    /**
     * Zink's base requirements that the driver does not have, for the Zink that the mode uses here
     * ([zink]): the list of its check_base_requirements ([ZINK_BASE_OLDER], [ZINK_BASE_CURRENT]).
     * Zink only warns there ("Some incorrect rendering might occur because the selected Vulkan
     * device (...) doesn't support base Zink requirements: ...") and goes on; on the RP Mini V2
     * (Adreno 650, only logicOp missing) the game's Venus context was then lost ("vkQueueSubmit
     * resulted in CS error", 2026-11-04), so the owner's rule (2026-11-04, "Grey Venus by Zink's
     * check"): no Venus route where one is missing. Venus gives Zink the driver's features, and the
     * extensions of its passthrough list only where the driver has them (Mesa
     * src/virtio/vulkan/vn_physical_device.c vn_physical_device_init_supported_extensions), so the
     * driver's own values are Zink's.
     */
    fun zinkBaseMissing(): List<String> =
        (if (zink == SystemDriver.Zink.CURRENT) ZINK_BASE_CURRENT else ZINK_BASE_OLDER).filter { zinkBase[it] == false }

    /** Why Venus cannot run by Zink's base requirements ([zinkBaseMissing]); null where none is missing. */
    fun zinkBaseProblem(): String? = zinkBaseMissing().takeIf { it.isNotEmpty() }?.let {
        "its Vulkan driver lacks Zink's base requirements (OpenGL on Vulkan): ${it.joinToString(" ")}"
    }

    private fun zinkName() = if (zink == SystemDriver.Zink.CURRENT) "Zink of Mesa main" else "Zink 25.1.9"

    companion object {
        /** Zink's base requirements by the names that its warning prints (zink_screen.c check_base_requirements). */
        const val LOGIC_OP = "feats.features.logicOp"
        const val FILL_MODE_NON_SOLID = "feats.features.fillModeNonSolid"
        const val SHADER_CLIP_DISTANCE = "feats.features.shaderClipDistance"
        const val SCALAR_BLOCK_LAYOUT = "scalarBlockLayout OR EXT_scalar_block_layout"
        const val MAINTENANCE1 = "have_KHR_maintenance1"
        const val CUSTOM_BORDER_COLOR = "have_EXT_custom_border_color"
        const val LINE_RASTERIZATION = "have_EXT_line_rasterization"

        /** Mesa 25.1.9's list (src/gallium/drivers/zink/zink_screen.c:2722-2729): the older Zink ([SystemDriver.Zink.OLDER]). */
        val ZINK_BASE_OLDER = listOf(
            LOGIC_OP, FILL_MODE_NON_SOLID, SHADER_CLIP_DISTANCE, SCALAR_BLOCK_LAYOUT, MAINTENANCE1, CUSTOM_BORDER_COLOR, LINE_RASTERIZATION,
        )

        /**
         * The product Mesa's list (main at userland/config.sh's MESA_COMMIT, zink_screen.c:2995-3000;
         * [SystemDriver.Zink.CURRENT]): no fillModeNonSolid and no shaderClipDistance.
         */
        val ZINK_BASE_CURRENT = listOf(LOGIC_OP, SCALAR_BLOCK_LAYOUT, MAINTENANCE1, CUSTOM_BORDER_COLOR, LINE_RASTERIZATION)

        /**
         * The baseline log line of the Venus way ([DeviceCheckRun]), as VirGL's
         * ([PhoneGlesDriver.logLine]): whether it can run, with the reason, the Zink and the base
         * requirements that are missing, and the driver; [server]: the APK carries the mode's parts.
         */
        fun logLine(phone: SystemDriverGpu?, server: Boolean): String = when {
            !server -> "Venus: cannot run (this app has no system-driver server)"
            phone == null -> "Venus: not known (the phone's Vulkan driver was not read)"
            else -> phone.problem().let { problem ->
                val missing = if (phone.zinkBase.isEmpty()) "not read" else phone.zinkBaseMissing().joinToString(" ").ifEmpty { "none" }
                // The Zink rule: its list once, not in the reason too (review LOW-1).
                "Venus: " + when (problem) {
                    null -> "can run ("
                    phone.zinkBaseProblem() -> "cannot run ("
                    else -> "cannot run ($problem; "
                } + "${phone.zinkName()}, base requirements missing: $missing): ${phone.name}, Vulkan ${phone.apiVersion.first}.${phone.apiVersion.second}"
            }
        }

        /** VK_DRIVER_ID_IMAGINATION_PROPRIETARY (vulkan_core.h). */
        const val IMAGINATION_PROPRIETARY = 7

        /** VK_DRIVER_ID_ARM_PROPRIETARY (vulkan_core.h). */
        const val ARM_PROPRIETARY = 9

        /** VK_DRIVER_ID_QUALCOMM_PROPRIETARY (vulkan_core.h). */
        const val QUALCOMM_PROPRIETARY = 8

        /** VK_DRIVER_ID_SAMSUNG_PROPRIETARY (vulkan_core.h). */
        const val SAMSUNG_PROPRIETARY = 21

        /**
         * The driverVersion of Imagination's DDK 24.1 (its change number 6554834; DDK 24.2 gives
         * 6643903, "24.2@6643903" in its driverInfo): Zink main's limit for its drivers.
         */
        const val POWERVR_DDK_24_1 = 6_554_834L
    }
}

/**
 * Reads the phone's own Vulkan driver (libsystemdriverprobe.so, `system_driver_probe.c`), only for
 * a GPU that Turnip and PanVK do not drive: it loads the vendor driver into the app's process.
 */
object SystemDriverProbe {
    private const val TAG = "pd2-sysdrv"

    init {
        System.loadLibrary("systemdriverprobe")
    }

    /** `key=value` lines; an `error` line when no Vulkan device answers. */
    @JvmStatic
    private external fun nativeProbe(): String

    /** The driver, or null when no Vulkan device answers. [kernel]: the GPU its kernel driver names, if any. */
    fun probe(kernel: HostGpu?): SystemDriverGpu? =
        SystemDriverReport.parse(report(), kernel, SystemDriver.HEAPS.any { File(it).canRead() })?.also {
            if (it.powerVrWithoutGeometryShaders) Log.i(TAG, "PowerVR driver without geometry shaders: no Venus route (Zink refuses it)")
            if (it.venusLowConfidence) Log.w(TAG, "PowerVR driver older than DDK 24.1 (${it.driverVersion}): Venus is low confidence")
        }

    /** The probe's `key=value` lines (also in logcat). */
    fun report(): String = nativeProbe().also { Log.i(TAG, it.replace('\n', ' ')) }
}

/** The probe's report, read apart from the native library so that the JVM tests can check it. */
object SystemDriverReport {
    /** The probe's lines of Zink's base requirements ([SystemDriverGpu.zinkBase]), by Zink's names. */
    private val ZINK_BASE_KEYS = listOf(
        "logic_op" to SystemDriverGpu.LOGIC_OP,
        "fill_mode_non_solid" to SystemDriverGpu.FILL_MODE_NON_SOLID,
        "shader_clip_distance" to SystemDriverGpu.SHADER_CLIP_DISTANCE,
        "scalar_block_layout" to SystemDriverGpu.SCALAR_BLOCK_LAYOUT,
        "maintenance1" to SystemDriverGpu.MAINTENANCE1,
        "custom_border_color" to SystemDriverGpu.CUSTOM_BORDER_COLOR,
        "line_rasterization" to SystemDriverGpu.LINE_RASTERIZATION,
    )

    /** [text] as the probe gives it; [heap]: a dma-buf heap the app can open. */
    fun parse(text: String, kernel: HostGpu?, heap: Boolean): SystemDriverGpu? {
        val values = text.lineSequence().mapNotNull { line ->
            line.indexOf('=').takeIf { it > 0 }?.let { line.substring(0, it) to line.substring(it + 1) }
        }.toMap()
        if (values.containsKey("error")) return null
        val name = values["name"] ?: return null
        val version = values["api"]?.split('.')?.mapNotNull { it.toIntOrNull() }.orEmpty()
        if (version.size < 2) return null
        fun flag(key: String) = values[key] == "1"
        // Zink 25.1.9's hard needs (zink_device_info.py required=True, zink_screen.c).
        val base = flag("maintenance1") && flag("create_renderpass2") && flag("descriptor_update_template") &&
            flag("timeline_semaphore")
        val zink = base && flag("imageless_framebuffer")
        // The product Mesa's Zink (26.2.3, and main at MESA_COMMIT: the same required=True list and
        // base checks): the same without imageless framebuffers (a00e2722), with dynamic rendering,
        // robustness2's nullDescriptor and maintenance5.
        val current = base && flag("dynamic_rendering") && flag("null_descriptor") && flag("maintenance5")
        // virglrenderer's order (vkr_physical_device.c, vkr_device_memory.c): dma-buf export, else
        // opaque-fd export of any allocation (which vtest refuses; patch 0005 does not count an
        // export of dedicated allocations only), else our heap path for a driver that imports
        // dma-bufs, else our AHardwareBuffer path (patch 0005).
        val opaqueExport = flag("opaque_fd_export") && !flag("opaque_fd_dedicated_only")
        val memory = when {
            flag("dma_buf_export") -> SharedMemory.DMA_BUF_EXPORT
            opaqueExport -> SharedMemory.NONE
            flag("dma_buf_extension") && heap -> SharedMemory.DMA_BUF_HEAP
            flag("ahb") && flag("queue_family_foreign") -> SharedMemory.AHB
            else -> SharedMemory.NONE
        }
        return SystemDriverGpu(
            name, version[0] to version[1], zink, memory, kernel,
            directPresent = flag("dma_buf_import") && flag("drm_format_modifier"),
            zinkCurrent = current,
            driverId = values["driver_id"]?.toIntOrNull(),
            driverVersion = values["driver_version"]?.toLongOrNull(),
            geometryShader = values["geometry_shader"]?.let { it == "1" },
            // An older probe has no logic_op line (nor the other new ones): then nothing is read.
            zinkBase = if ("logic_op" !in values) emptyMap() else ZINK_BASE_KEYS.associate { (key, name) -> name to flag(key) },
        )
    }
}
