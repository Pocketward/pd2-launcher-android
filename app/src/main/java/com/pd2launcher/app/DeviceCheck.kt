package com.pd2launcher.app

import android.content.Context
import android.opengl.EGL14
import android.opengl.GLES20
import android.os.StatFs
import android.system.ErrnoException
import android.system.Os
import android.system.OsConstants
import java.io.File

/** What the first-start check measures. */
data class DeviceFacts(
    val pageSize: Long,
    /** For the check's log only: the check does not look at the kernel version. */
    val kernelRelease: String,
    /** Null when neither `/dev/kgsl-3d0` nor `/dev/mali0` (CSF or the job manager) answers ([HostGpu.probe]). */
    val gpu: HostGpu?,
    /** The app's data folder as [DeviceCheck.dataDir] names it. */
    val dataDir: String,
    /** The longest data folder path the App userland takes: its placeholder's length ([Relocation]). */
    val dataDirLimit: Int,
    val freeBytes: Long,
    /** Space the App userland unpack needs; 0 when it is already unpacked. */
    val requiredBytes: Long,
    /** The APK carries the system-driver mode's server ([SystemDriver.serverInApk]). */
    val systemDriverServer: Boolean = true,
    /** The App userland has the system-driver mode's files, or is not unpacked yet ([SystemDriver.filesPresent]). */
    val systemDriverFiles: Boolean = true,
    /**
     * The phone's own Vulkan driver ([HostGpu.phoneDriver]): [gpu] itself when only it can run the
     * game, else its report beside the built-in driver's GPU; null when none answers.
     */
    val phoneDriver: SystemDriverGpu? = null,
    /** A debug build's forced route ([SystemDriver.forced]); null in release builds and without the file. */
    val forcedRoute: GraphicsRoute? = null,
    /** The APK carries the VirGL route's server ([SystemDriver.glServerInApk]). */
    val glServer: Boolean = true,
    /** The phone's own OpenGL ES driver ([HostGpu.phoneGles]); null where it was not read or EGL did not answer. */
    val phoneGles: PhoneGlesDriver? = null,
    /** The APK carries the libhybris route's files ([SystemDriver.hybrisInApk]). */
    val hybris: Boolean = false,
    /** The RADV route is offered here ([Radv.allowed]) and the APK carries the part it is in. */
    val radv: Boolean = false,
    /** The mali-panvk route is offered here ([MaliPanvk.allowed]) and the APK carries the part it is in. */
    val maliPanvk: Boolean = false,
    /**
     * The phone's own drivers were read ([HostGpu.phoneDriver] and [HostGpu.phoneGles] probed them):
     * no route of the Phone driver then means it cannot run here (the GPU page greys it, decision 40).
     */
    val phoneRead: Boolean = false,
    /**
     * The APK's engine can run the Software driver ([DriverPage.BUNDLED_ENGINE_SOFTWARE]): where
     * no GPU route runs, the check then passes with the Software driver as the default
     * ([Finding.Kind.SOFTWARE_ONLY]).
     */
    val software: Boolean = false,
) {
    /** Which drivers can run the game here ([DriverChoice.options]). */
    val driverOptions: DriverOptions
        get() = DriverChoice.options(
            gpu, phoneDriver, systemDriverServer, systemDriverFiles, phoneGles, glServer,
            hybrisInApk = hybris, radvAllowed = radv, radvFiles = radv,
            maliPanvkAllowed = maliPanvk, maliPanvkFiles = maliPanvk,
        )

    /** Whether the app warns that the game takes the phone's own driver ([PhoneDriverAdvice]). */
    val advice: PhoneDriverAdvice? get() = PhoneDriverAdvice.of(driverOptions, forcedRoute)
}

/**
 * One problem the check found; [Kind.GPU_UNKNOWN_TO_DRIVER] lets the app go on, and
 * [Kind.PHONE_DRIVER_ONLY] does once the player has chosen Try anyway ([Kind.acceptable]).
 * [message] is for the log; [subject] names what the screen shows (the GPU for the GPU kinds).
 */
data class Finding(val kind: Kind, val message: String, val subject: String = "") {
    enum class Kind(val blocks: Boolean, val acceptable: Boolean = false) {
        GPU_NOT_SUPPORTED(true),
        GPU_UNKNOWN_TO_DRIVER(false),
        /** The built-in drivers do not take the GPU, but the phone's own driver can try. */
        PHONE_DRIVER_ONLY(true, acceptable = true),
        /**
         * No GPU route runs, but the engine can run the Software driver, the default then
         * ([DriverPage.defaultPick]): the check passes with this warning.
         */
        SOFTWARE_ONLY(false),
        /**
         * [SOFTWARE_ONLY] where the stored pick is another driver ([DeviceCheck.forPick]): that pick
         * stays (no automatic change) and has no route here, so the game does not use the Software
         * driver (review MED-1).
         */
        PICK_HAS_NO_ROUTE(false),
        PAGE_SIZE(true),
        DATA_FOLDER(true),
        NO_SPACE(true),
    }
}

/** [accepted]: the player chose Try anyway ([AppSettings.phoneDriverAccepted]). */
data class DeviceCheckResult(val findings: List<Finding>, val accepted: Boolean = false) {
    val passed: Boolean get() = findings.none { it.kind.blocks && !(accepted && it.kind.acceptable) }

    /** Only findings that Try anyway accepts keep the app from going on: the screen offers it. */
    val tryAnyway: Boolean get() = !passed && findings.filter { it.kind.blocks }.all { it.kind.acceptable }
}

/** The first-start check, run before anything is unpacked. */
object DeviceCheck {
    /** Where the device's main user keeps each app's data folder, named by its package. */
    private const val PRIMARY_DATA_ROOT = "/data/data"
    const val PAGE_SIZE = 4096L
    private val SUPPORTED_GENERATIONS = 6..8

    /**
     * The main user's data folder of [packageName] (`com.pd2launcher.app`, or the development
     * build's `com.pd2launcher.app.dev`).
     */
    fun primaryDataDir(packageName: String): String = "$PRIMARY_DATA_ROOT/$packageName"

    /** Space for an unpack of [unpackedBytes]: the files plus a tenth (at least 64 MiB) for metadata and the record. */
    fun requiredBytes(unpackedBytes: Long): Long = unpackedBytes + maxOf(unpackedBytes / 10, 64L shl 20)

    /**
     * Whether the bundled Turnip (an Adreno 6xx, 7xx or 8xx through KGSL) or PanVK (a CSF Mali v10
     * to v13 through kbase) drives [gpu]; any other GPU takes the system-driver mode if its own
     * Vulkan driver can ([HostGpu.probe]), and a Mali of kbase's job manager also the mali-panvk
     * route ([MaliPanvk], a route of that part).
     */
    fun bundledDriverTakes(gpu: HostGpu): Boolean = when (gpu) {
        is KgslGpu -> gpu.generation in SUPPORTED_GENERATIONS
        is MaliGpu -> !gpu.jobManager && gpu.archMajor in MALI_ARCHES && gpu.uapiMinor >= MALI_MIN_UAPI_MINOR
        is SystemDriverGpu -> false
    }

    /**
     * Whether the bundled Turnip or PanVK both takes [gpu] ([bundledDriverTakes]) and knows its
     * model: a GPU it takes but does not know gets a warning ([Finding.Kind.GPU_UNKNOWN_TO_DRIVER]),
     * and the driver refuses it, so the phone's own driver is its only one where that can run.
     */
    fun bundledDriverKnows(gpu: HostGpu): Boolean = bundledDriverTakes(gpu) && when (gpu) {
        is KgslGpu -> turnipKnows(gpu.chipId)
        is MaliGpu -> panvkKnows(gpu)
        is SystemDriverGpu -> false
    }

    /** [glRenderer] names a GPU the bundled drivers do not take; it is called only then. */
    fun evaluate(facts: DeviceFacts, glRenderer: () -> String?): DeviceCheckResult {
        val findings = mutableListOf<Finding>()
        val gpu = facts.gpu
        // The VirGL route on the phone's own OpenGL ES driver, where the built-in drivers cannot run.
        val gles = facts.phoneGles?.takeIf { !facts.driverOptions.builtIn && facts.driverOptions.virgl }
        val glesDriver = gles?.let { "the phone's own OpenGL ES driver (${it.renderer}, ${it.version.first}.${it.version.second}) can try" }
        if (gpu is SystemDriverGpu) {
            // The phone's own Vulkan driver, through the system-driver mode: what keeps it from
            // running there, if anything; else the player may try it (Try anyway), as also where
            // only its OpenGL ES driver can (VirGL).
            val problem = gpu.problem() ?: if (!facts.systemDriverServer) "this app has no system-driver server" else null
            // An Xclipse where RADV is offered: RADV first (the default), the phone's own driver for a pick of it
            // (DriverOptions.radv); the same for mali-panvk on a job-manager Mali (DriverOptions.maliPanvk).
            val launcher = when {
                facts.driverOptions.radv -> "RADV for Xclipse can try first; "
                facts.driverOptions.maliPanvk -> "mali-panvk can try first; "
                else -> ""
            }
            findings += when {
                // PowerVR: the phone's Vulkan driver in the game's process first (libhybris), the
                // routes through a server as its other ways (DriverOptions.libhybris).
                facts.driverOptions.libhybris -> Finding(
                    Finding.Kind.PHONE_DRIVER_ONLY,
                    "${gpu.name} is not supported by the built-in graphics drivers; the phone's own Vulkan driver can try in the game's " +
                        "process (libhybris)" + (facts.driverOptions.routes.drop(1).takeIf { it.isNotEmpty() }?.let { " first, then ${it.joinToString(", ")}" } ?: ""),
                    subject = gpu.name,
                )
                problem == null -> Finding(
                    Finding.Kind.PHONE_DRIVER_ONLY,
                    "${gpu.name} is not supported by the built-in graphics drivers; $launcher" +
                        // Xclipse, Qualcomm and Mali: VirGL first, Venus second (DriverOptions.virglFirst).
                        (if (facts.driverOptions.virglFirst && glesDriver != null) "$glesDriver first, its Vulkan driver second" else "the phone's own Vulkan driver can try") +
                        if (gpu.venusLowConfidence) " (low confidence: a PowerVR driver older than DDK 24.1)" else "",
                    subject = gpu.name,
                )
                facts.driverOptions.radv || facts.driverOptions.maliPanvk -> Finding(
                    Finding.Kind.PHONE_DRIVER_ONLY,
                    "${gpu.name} is not supported by the built-in graphics drivers, and its Vulkan driver cannot run the game " +
                        "($problem); ${if (facts.driverOptions.radv) "RADV for Xclipse" else "mali-panvk"} can try" +
                        (glesDriver?.let { ", then $it" } ?: ""),
                    subject = gpu.name,
                )
                glesDriver != null -> Finding(
                    Finding.Kind.PHONE_DRIVER_ONLY,
                    "${gpu.name} is not supported by the built-in graphics drivers, and its Vulkan driver cannot run the game " +
                        "($problem); $glesDriver",
                    subject = gpu.name,
                )
                else -> Finding(
                    Finding.Kind.GPU_NOT_SUPPORTED,
                    "${gpu.name} is not supported: $problem" + (facts.phoneGles?.problem()?.let { "; $it" } ?: ""),
                    subject = gpu.name,
                )
            }
        } else if (gpu is MaliGpu && gpu.jobManager && facts.driverOptions.maliPanvk) {
            // A job-manager Mali whose own Vulkan driver gave no report: mali-panvk, which opens the
            // GPU itself, can try first, the phone's own OpenGL ES driver for a pick of it.
            findings += Finding(
                Finding.Kind.PHONE_DRIVER_ONLY,
                "${gpu.name} is not supported by the built-in graphics drivers; mali-panvk can try" +
                    (glesDriver?.let { ", then $it" } ?: ""),
                subject = gpu.name,
            )
        } else if ((gpu == null || !bundledDriverTakes(gpu)) && gles != null && glesDriver != null) {
            // No Vulkan driver that answers, and a GPU the built-in drivers do not take: only the
            // phone's own OpenGL ES driver can try (VirGL).
            val name = gpu?.name ?: gles.renderer
            findings += Finding(
                Finding.Kind.PHONE_DRIVER_ONLY,
                "${name.ifEmpty { "an unknown GPU" }} is not supported by the built-in graphics drivers; $glesDriver",
                subject = name,
            )
        } else if (gpu is MaliGpu && bundledDriverTakes(gpu)) {
            if (!panvkKnows(gpu)) {
                // A CSF Mali of a supported architecture that the bundled PanVK has no model for: it refuses it.
                findings += unknownToDriver(facts, gpu, "Mesa PanVK")
            }
        } else if (gpu !is KgslGpu || !bundledDriverTakes(gpu)) {
            // Empty when neither GLES nor the kernel driver names it.
            val name = glRenderer() ?: gpu?.name ?: ""
            findings += Finding(
                Finding.Kind.GPU_NOT_SUPPORTED,
                "${name.ifEmpty { "an unknown GPU" }} is not supported: an Adreno 6xx, 7xx or 8xx GPU through /dev/kgsl-3d0, " +
                    "a Mali v10 to v13 GPU through Arm's kbase (CSF, /dev/mali0), or with the mali-panvk route a Mali of " +
                    "kbase's job manager that mali-panvk knows is needed",
                subject = name,
            )
        } else if (!turnipKnows(gpu.chipId)) {
            // An Adreno of a supported line that the bundled Turnip has no entry for: it refuses it.
            findings += unknownToDriver(facts, gpu, "Mesa's Turnip")
        }
        // A debug build's forced route where the device would take another: the same question, marked as forced.
        if (facts.advice == PhoneDriverAdvice.FORCED && findings.none { it.kind == Finding.Kind.PHONE_DRIVER_ONLY }) {
            findings += Finding(
                Finding.Kind.PHONE_DRIVER_ONLY,
                "${gpu?.name ?: "the GPU"}: a debug build forces the phone's own driver " +
                    "(${SystemDriver.forceFile(facts.forcedRoute ?: GraphicsRoute.VENUS)})",
                subject = gpu?.name.orEmpty(),
            )
        }
        // No GPU route: the Software driver where the engine can run it (REMOVE-AUTO-FALLBACK §1, the
        // owner, 2026-11-04: the App driver, else the Phone driver, else the Software driver, else
        // the "no driver" outcome), so the session starts and the default can run.
        if (facts.software && facts.driverOptions.routes.isEmpty()) {
            findings.replaceAll {
                if (it.kind != Finding.Kind.GPU_NOT_SUPPORTED) it
                else it.copy(kind = Finding.Kind.SOFTWARE_ONLY, message = "${it.message}; the Software driver can run the game (DirectDraw with GDI)")
            }
        }
        if (facts.pageSize != PAGE_SIZE) {
            findings += Finding(Finding.Kind.PAGE_SIZE, "memory page size ${facts.pageSize}, 4096 is needed")
        }
        // The App userland is rewritten for the data folder as it unpacks, wherever Android keeps it;
        // the Unix sockets the session makes below it must fit sun_path.
        (Relocation.problem(facts.dataDir, facts.dataDirLimit) ?: socketProblem(facts.dataDir))
            ?.let { findings += Finding(Finding.Kind.DATA_FOLDER, it) }
        if (facts.freeBytes < facts.requiredBytes) {
            findings += Finding(
                Finding.Kind.NO_SPACE,
                "${facts.freeBytes shr 20} MiB free, ${facts.requiredBytes shr 20} MiB needed to unpack",
            )
        }
        return DeviceCheckResult(findings)
    }

    /**
     * [result] for the pick that the device check stored ([DriverPage.withDefault]): a
     * [Finding.Kind.SOFTWARE_ONLY] says that the game uses the Software driver, so it stays only
     * where [pick] is the Software driver (or none is stored); a stored pick of another driver
     * stays, has no route here, and gets [Finding.Kind.PICK_HAS_NO_ROUTE] (review MED-1).
     */
    fun forPick(result: DeviceCheckResult, pick: PlayerDriver?): DeviceCheckResult {
        if (pick == null || pick == PlayerDriver.SOFTWARE) return result
        return result.copy(
            findings = result.findings.map {
                if (it.kind != Finding.Kind.SOFTWARE_ONLY) it
                else it.copy(kind = Finding.Kind.PICK_HAS_NO_ROUTE, message = "${it.message}; the stored pick (${pick.name.lowercase()} driver) has no route here")
            },
        )
    }

    /**
     * A GPU that the bundled [driver] takes but has no model for (it refuses it): where the phone's
     * own driver can run Venus, the route is that driver ([DriverOptions.route]), so the player
     * chooses Try anyway first ([Finding.Kind.PHONE_DRIVER_ONLY]); else the warning that the game
     * may not start ([Finding.Kind.GPU_UNKNOWN_TO_DRIVER], which does not block, as before the mode).
     */
    private fun unknownToDriver(facts: DeviceFacts, gpu: HostGpu, driver: String): Finding =
        if (facts.driverOptions.venus || facts.driverOptions.virgl) {
            Finding(
                Finding.Kind.PHONE_DRIVER_ONLY,
                "${gpu.name} is not known to the bundled graphics driver ($driver); the phone's own " +
                    "${if (facts.driverOptions.venus) "Vulkan" else "OpenGL ES"} driver can try",
                subject = gpu.name,
            )
        } else {
            Finding(
                Finding.Kind.GPU_UNKNOWN_TO_DRIVER,
                "${gpu.name} is not known to the bundled graphics driver ($driver): the game may not start",
                subject = gpu.name,
            )
        }

    /** The longest path a Unix socket takes: `sun_path`'s 108 bytes, its terminating NUL included. */
    private const val SOCKET_PATH_MAX = 107

    /** Why a session socket below [dataDir] would not fit `sun_path`, or null. */
    private fun socketProblem(dataDir: String): String? =
        SessionPaths(dataDir).sockets.map { it.path }.firstOrNull { it.toByteArray().size > SOCKET_PATH_MAX }
            ?.let { "data folder $dataDir: the socket $it is longer than the $SOCKET_PATH_MAX bytes a Unix socket takes" }

    /**
     * Measures this device; [gpu] comes from [HostGpu.probe], [phoneDriver] from
     * [HostGpu.phoneDriver], [dataDirLimit] from the App userland's manifest.
     */
    fun measure(
        context: Context,
        gpu: HostGpu?,
        requiredBytes: Long,
        dataDirLimit: Int,
        phoneDriver: SystemDriverGpu? = null,
        systemDriverFiles: Boolean = true,
        forcedRoute: GraphicsRoute? = null,
        phoneGles: PhoneGlesDriver? = null,
        phoneRead: Boolean = false,
    ): DeviceFacts {
        val files = context.filesDir
        return DeviceFacts(
            pageSize = Os.sysconf(OsConstants._SC_PAGESIZE),
            kernelRelease = Os.uname().release,
            gpu = gpu,
            dataDir = dataDir(context),
            dataDirLimit = dataDirLimit,
            freeBytes = StatFs(files.path).availableBytes,
            requiredBytes = requiredBytes,
            systemDriverServer = SystemDriver.inApk(context),
            systemDriverFiles = systemDriverFiles,
            phoneDriver = phoneDriver,
            forcedRoute = forcedRoute,
            glServer = SystemDriver.glServerInApk(context.applicationInfo.nativeLibraryDir),
            phoneGles = phoneGles,
            phoneRead = phoneRead,
            software = DriverPage.BUNDLED_ENGINE_SOFTWARE,
            hybris = SystemDriver.hybrisInApk(context),
            radv = Radv.allowed(files) && UserlandAssets.carries(context, UserlandAssets.SYSTEM_DRIVER),
            maliPanvk = MaliPanvk.allowed(files) && UserlandAssets.carries(context, UserlandAssets.SYSTEM_DRIVER),
        )
    }

    /**
     * The app's data folder, the one the App userland is rewritten for: its [primaryDataDir] when
     * that path is the same folder as `Context.getDataDir()` (the main user's usual path), else the
     * latter's canonical path (another user, a work profile, adopted storage). A path comparison is
     * not enough: with app data isolation (Android 11+) `/data/user/0` is a bind mount in the app's
     * mount namespace, not a link to `/data/data`.
     */
    fun dataDir(context: Context): String {
        val own = File(context.dataDir.path)
        val primaryDir = primaryDataDir(context.packageName)
        return try {
            val mine = Os.stat(own.path)
            val primary = Os.stat(primaryDir)
            if (mine.st_dev == primary.st_dev && mine.st_ino == primary.st_ino) primaryDir else own.canonicalPath
        } catch (_: ErrnoException) {
            own.canonicalPath
        }
    }

    /** Android's GLES `GL_RENDERER` from a throwaway 1x1 pbuffer context, or null if EGL fails. */
    fun glRenderer(): String? {
        val display = EGL14.eglGetDisplay(EGL14.EGL_DEFAULT_DISPLAY)
        // No eglTerminate: the display is shared with the UI's renderer in this process.
        if (display == EGL14.EGL_NO_DISPLAY || !EGL14.eglInitialize(display, IntArray(2), 0, IntArray(2), 1)) return null
        val configs = arrayOfNulls<android.opengl.EGLConfig>(1)
        val count = IntArray(1)
        val attributes = intArrayOf(
            EGL14.EGL_RENDERABLE_TYPE, EGL14.EGL_OPENGL_ES2_BIT,
            EGL14.EGL_SURFACE_TYPE, EGL14.EGL_PBUFFER_BIT,
            EGL14.EGL_NONE,
        )
        if (!EGL14.eglChooseConfig(display, attributes, 0, configs, 0, 1, count, 0) || count[0] == 0) return null
        val config = configs[0]
        val context = EGL14.eglCreateContext(
            display, config, EGL14.EGL_NO_CONTEXT,
            intArrayOf(EGL14.EGL_CONTEXT_CLIENT_VERSION, 2, EGL14.EGL_NONE), 0,
        )
        if (context == EGL14.EGL_NO_CONTEXT) return null
        val surface = EGL14.eglCreatePbufferSurface(
            display, config, intArrayOf(EGL14.EGL_WIDTH, 1, EGL14.EGL_HEIGHT, 1, EGL14.EGL_NONE), 0,
        )
        try {
            if (surface == EGL14.EGL_NO_SURFACE || !EGL14.eglMakeCurrent(display, surface, surface, context)) return null
            return GLES20.glGetString(GLES20.GL_RENDERER)
        } finally {
            EGL14.eglMakeCurrent(display, EGL14.EGL_NO_SURFACE, EGL14.EGL_NO_SURFACE, EGL14.EGL_NO_CONTEXT)
            if (surface != EGL14.EGL_NO_SURFACE) EGL14.eglDestroySurface(display, surface)
            EGL14.eglDestroyContext(display, context)
        }
    }
}
