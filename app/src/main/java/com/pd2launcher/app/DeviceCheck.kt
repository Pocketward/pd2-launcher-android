package com.pd2launcher.app

import android.content.Context
import android.opengl.EGL14
import android.opengl.GLES20
import android.os.Build
import android.os.StatFs
import android.system.ErrnoException
import android.system.Os
import android.system.OsConstants
import java.io.File

/** What the first-start check measures. */
data class DeviceFacts(
    val sdk: Int,
    val pageSize: Long,
    val kernelRelease: String,
    /** Null when neither `/dev/kgsl-3d0` nor a CSF `/dev/mali0` answers ([HostGpu.probe]). */
    val gpu: HostGpu?,
    /** The app's data folder as [DeviceCheck.dataDir] names it. */
    val dataDir: String,
    val freeBytes: Long,
    /** Space the App userland unpack needs; 0 when it is already unpacked. */
    val requiredBytes: Long,
)

/**
 * One problem the check found; only [Kind.GPU_UNKNOWN_TO_DRIVER] and [Kind.KERNEL_UNTESTED] let
 * the app go on. [message] is for the log; [subject] names what the screen shows (the GPU for
 * the GPU kinds).
 */
data class Finding(val kind: Kind, val message: String, val subject: String = "") {
    enum class Kind(val blocks: Boolean) {
        GPU_NOT_SUPPORTED(true),
        GPU_UNKNOWN_TO_DRIVER(false),
        PAGE_SIZE(true),
        ANDROID_TOO_OLD(true),
        NOT_PRIMARY_USER(true),
        NO_SPACE(true),
        KERNEL_UNTESTED(false),
    }
}

data class DeviceCheckResult(val findings: List<Finding>) {
    val passed: Boolean get() = findings.none { it.kind.blocks }
}

/** The first-start check, run before anything is unpacked. */
object DeviceCheck {
    /** Where the device's primary user keeps each app's data folder, named by its package. */
    private const val PRIMARY_DATA_ROOT = "/data/data"
    const val PAGE_SIZE = 4096L
    /** Android 11; 9 and 10 are refused although minSdk is 28. */
    const val MIN_SDK = 30
    private val SUPPORTED_GENERATIONS = 6..8
    /** FEX states it needs 5.15; older kernels are untested, not refused. */
    private const val TESTED_KERNEL = 5 * 1000 + 15

    /**
     * The primary user's data folder of [packageName] (`com.pd2launcher.app`, or the development
     * build's `com.pd2launcher.app.dev`): the App userland's loader and glibc are built for it.
     */
    fun primaryDataDir(packageName: String): String = "$PRIMARY_DATA_ROOT/$packageName"

    /** Space for an unpack of [unpackedBytes]: the files plus a tenth (at least 64 MiB) for metadata and the record. */
    fun requiredBytes(unpackedBytes: Long): Long = unpackedBytes + maxOf(unpackedBytes / 10, 64L shl 20)

    /** [glRenderer] names a GPU the bundled drivers do not take; it is called only then. */
    fun evaluate(facts: DeviceFacts, glRenderer: () -> String?): DeviceCheckResult {
        val findings = mutableListOf<Finding>()
        val gpu = facts.gpu
        if (gpu is MaliGpu && gpu.archMajor in MALI_ARCHES && gpu.uapiMinor >= MALI_MIN_UAPI_MINOR) {
            if (!panvkKnows(gpu)) {
                // A CSF Mali of a supported architecture that the bundled PanVK has no model for: it refuses it.
                findings += Finding(
                    Finding.Kind.GPU_UNKNOWN_TO_DRIVER,
                    "${gpu.name} is not known to the bundled graphics driver (Mesa PanVK): the game may not start",
                    subject = gpu.name,
                )
            }
        } else if (gpu !is KgslGpu || gpu.generation !in SUPPORTED_GENERATIONS) {
            // Empty when neither GLES nor the kernel driver names it.
            val name = glRenderer() ?: gpu?.name ?: ""
            findings += Finding(
                Finding.Kind.GPU_NOT_SUPPORTED,
                "${name.ifEmpty { "an unknown GPU" }} is not supported: an Adreno 6xx, 7xx or 8xx GPU through /dev/kgsl-3d0 " +
                    "or a Mali v10 to v13 GPU through Arm's kbase (CSF, /dev/mali0) is needed",
                subject = name,
            )
        } else if (!turnipKnows(gpu.chipId)) {
            // An Adreno of a supported line that the bundled Turnip has no entry for: it refuses it.
            findings += Finding(
                Finding.Kind.GPU_UNKNOWN_TO_DRIVER,
                "${gpu.name} is not known to the bundled graphics driver (Mesa 26.2.3 Turnip): the game may not start",
                subject = gpu.name,
            )
        }
        if (facts.pageSize != PAGE_SIZE) {
            findings += Finding(Finding.Kind.PAGE_SIZE, "memory page size ${facts.pageSize}, 4096 is needed")
        }
        if (facts.sdk < MIN_SDK) {
            findings += Finding(Finding.Kind.ANDROID_TOO_OLD, "Android SDK ${facts.sdk}, Android 11 (SDK 30) or later is needed")
        }
        // An app's data folder is named by its package, so this is the primary user's of the same app.
        if (facts.dataDir != primaryDataDir(File(facts.dataDir).name)) {
            findings += Finding(
                Finding.Kind.NOT_PRIMARY_USER,
                "data folder ${facts.dataDir}: only the device's primary user is supported",
            )
        }
        if (facts.freeBytes < facts.requiredBytes) {
            findings += Finding(
                Finding.Kind.NO_SPACE,
                "${facts.freeBytes shr 20} MiB free, ${facts.requiredBytes shr 20} MiB needed to unpack",
            )
        }
        val kernel = Regex("""^(\d+)\.(\d+)""").find(facts.kernelRelease)?.destructured
            ?.let { (major, minor) -> major.toInt() * 1000 + minor.toInt() }
        if (kernel == null || kernel < TESTED_KERNEL) {
            findings += Finding(
                Finding.Kind.KERNEL_UNTESTED,
                "kernel ${facts.kernelRelease} is older than 5.15: the device is untested",
            )
        }
        return DeviceCheckResult(findings)
    }

    /** Measures this device; [gpu] comes from [HostGpu.probe]. */
    fun measure(context: Context, gpu: HostGpu?, requiredBytes: Long): DeviceFacts {
        val files = context.filesDir
        return DeviceFacts(
            sdk = Build.VERSION.SDK_INT,
            pageSize = Os.sysconf(OsConstants._SC_PAGESIZE),
            kernelRelease = Os.uname().release,
            gpu = gpu,
            dataDir = dataDir(context),
            freeBytes = StatFs(files.path).availableBytes,
            requiredBytes = requiredBytes,
        )
    }

    /**
     * The app's data folder as the App userland reaches it: its [primaryDataDir] when that path is
     * the same folder as `Context.getDataDir()`, else the latter's canonical path. A path
     * comparison is not enough: with app data isolation (Android 11+) `/data/user/0` is a bind
     * mount in the app's mount namespace, not a link to `/data/data`.
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
