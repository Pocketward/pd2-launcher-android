package com.pd2launcher.app

import android.content.Context
import android.opengl.EGL14
import android.opengl.EGLExt
import android.opengl.GLES20
import android.opengl.GLES30
import android.util.Log
import java.io.File
import java.io.IOException

/**
 * The phone's own OpenGL ES driver as the VirGL route ([GraphicsRoute.VIRGL]) needs it: Mesa's
 * virgl driver in the game sends OpenGL over vtest's socket to the APK's second virglrenderer
 * server, whose OpenGL renderer (vrend) draws with this driver. vrend needs (virglrenderer
 * src/vrend/vrend_winsys_egl.c virgl_egl_init and virgl_egl_check_extensions,
 * src/vrend/vrend_renderer.c vrend_renderer_init): EGL_KHR_surfaceless_context and
 * EGL_KHR_create_context ([surfaceless], [createContext]), a pbuffer config for OpenGL ES 2/3
 * ([config]), an OpenGL ES context of version 3.0 or later ([version]) that becomes current
 * without a surface ([current]). [renderer] is the driver's GL_RENDERER, the GPU's name.
 *
 * The game needs more: D2GL asks for an OpenGL 3.3 core context and stops without one ("Requires
 * OpenGL 3.3 or newer!"), and its shaders are GLSL 3.30. [problem] also greys VirGL where the
 * phone cannot give that ([gl33Problem]; the owner, 2026-11-04: "gray out virGL option on phones
 * without opengl 3.3 support").
 */
data class PhoneGlesDriver(
    val renderer: String,
    /** The OpenGL ES context's version (major, minor); (0, 0) when none was made. */
    val version: Pair<Int, Int>,
    val surfaceless: Boolean,
    val createContext: Boolean,
    val config: Boolean,
    val current: Boolean,
    /** Of the context's GL_EXTENSIONS, those that [vrendGlslLevel] reads ([GLSL_EXTENSIONS]). */
    val extensions: Set<String> = emptySet(),
) {
    /** What keeps the VirGL route from running on it, for the device check; null when nothing does. */
    fun problem(): String? = when {
        !surfaceless || !createContext -> "its EGL lacks EGL_KHR_surfaceless_context or EGL_KHR_create_context"
        !config -> "its EGL has no pbuffer configuration for OpenGL ES"
        version.first < 3 -> "its OpenGL ES is version ${version.first}.${version.second}, 3.0 is needed"
        !current -> "its OpenGL ES context does not start without a surface"
        else -> gl33Problem()
    }

    /**
     * The GLSL level that vrend gives the game on this driver (virglrenderer e18f8c00
     * src/vrend/vrend_renderer.c vrend_fill_caps_glsl_version, with the host features of
     * init_features and feature_list): 130 on OpenGL ES 3.0, 310 on 3.1 and later; 400 where the
     * driver has tessellation, geometry and gpu_shader5 shaders (OpenGL ES 3.2, or their
     * extensions), then 410 with separate shader objects and 430 with compute shaders (both in
     * OpenGL ES 3.1).
     */
    fun vrendGlslLevel(): Int {
        val es = version.first * 10 + version.second
        fun has(gles: Int, names: List<String>) = es >= gles || names.any { it in extensions }
        var level = if (es >= 31) 310 else 130
        if (has(32, TESSELLATION) && has(32, GEOMETRY) && has(32, GPU_SHADER5)) {
            level = 400
            if (has(31, SEPARATE_SHADER_OBJECTS)) level = if (has(31, COMPUTE)) 430 else 410
        }
        return level
    }

    /**
     * Why VirGL may not give the game OpenGL 3.3 here; null where it can: vrend gives less than
     * GLSL 3.30 ([vrendGlslLevel]: 130 or 310), the GLSL of OpenGL 3.3. The cause is a GUESS. Facts
     * (Mesa source): Mesa's compute_version (src/mesa/main/version.c) needs GLSL 3.30 for a 3.3
     * context, but the route sets MESA_GL_VERSION_OVERRIDE=3.3 and MESA_GLSL_VERSION_OVERRIDE=330,
     * and `_mesa_override_gl_version_contextless` (version.c:169-193) and `_mesa_override_glsl_version`
     * (version.c:227-241) replace the computed values, so Mesa makes a 3.3 context also on an OpenGL
     * ES 3.0 or 3.1 host. Whether the game's GLSL 3.30 shaders then run through vrend on such a host
     * is not tested (no such phone). The rule stays because the owner asked to grey phones without
     * OpenGL 3.3 support (2026-11-04). To do: check one OpenGL ES 3.1 phone. An Adreno 610 phone's (vrend
     * GLSL 430) passes: its D2GL box "Requires OpenGL 3.3 or newer!" came from D2GL's saved gl_ver
     * 4.0, which never asks for 3.3; the engine (0.1.14) now writes 3.3 before each Glide start.
     */
    fun gl33Problem(): String? {
        val level = vrendGlslLevel()
        return if (level < GLSL_33) {
            "its OpenGL ES ${version.first}.${version.second} gives the game GLSL ${level / 100}.${level % 100} " +
                "through VirGL (no tessellation, geometry and gpu_shader5 shaders), and OpenGL 3.3 needs GLSL 3.30"
        } else {
            null
        }
    }

    companion object {
        /** The GLSL level of OpenGL 3.3. */
        const val GLSL_33 = 330

        // vrend's feature_list: the extensions of each feature (src/vrend/vrend_renderer.c:261-319).
        private val TESSELLATION = listOf("GL_ARB_tessellation_shader", "GL_OES_tessellation_shader", "GL_EXT_tessellation_shader")
        private val GEOMETRY = listOf("GL_EXT_geometry_shader", "GL_OES_geometry_shader")
        private val GPU_SHADER5 = listOf("GL_ARB_gpu_shader5", "GL_EXT_gpu_shader5", "GL_OES_gpu_shader5")
        // vrend's own spelling ("seperate").
        private val SEPARATE_SHADER_OBJECTS = listOf("GL_ARB_seperate_shader_objects")
        private val COMPUTE = listOf("GL_ARB_compute_shader")

        /** The extensions that [vrendGlslLevel] reads; the probe keeps only these. */
        val GLSL_EXTENSIONS: Set<String> = (TESSELLATION + GEOMETRY + GPU_SHADER5 + SEPARATE_SHADER_OBJECTS + COMPUTE).toSet()

        /**
         * The baseline log line of the VirGL way ([DeviceCheckRun]): whether it can run, with the
         * reason, the GLSL level that vrend gives, and the driver; [glServer]: the APK carries the
         * VirGL server.
         */
        fun logLine(gles: PhoneGlesDriver?, glServer: Boolean): String = when {
            !glServer -> "VirGL: cannot run (this app has no VirGL server)"
            gles == null -> "VirGL: not known (the phone's OpenGL ES driver was not read)"
            else -> gles.problem().let { problem ->
                "VirGL: " + (if (problem == null) "can run (OpenGL 3.3 core for the game" else "cannot run ($problem") +
                    "; vrend GLSL ${gles.vrendGlslLevel()}): $gles"
            }
        }
    }
}

/**
 * Reads the phone's own OpenGL ES driver ([PhoneGlesDriver]) in the app's process, once per
 * process: only where the VirGL route may be needed ([HostGpu.phoneGles]: a GPU the built-in
 * driver does not support, a pick of the Phone driver, or a debug build). The app's own drawing uses the same
 * driver, so this loads nothing new. Null when EGL does not answer or the probe fails.
 */
object PhoneGles {
    private const val TAG = "pd2-sysdrv"
    private const val MARKER = "phone-gles-probe.running"
    private var read = false
    private var report: PhoneGlesDriver? = null
    @Volatile private var marker: File? = null

    /**
     * The probe makes a context of the vendor's driver in the app's process; one that crashes or
     * hangs there would end every start. As [PhoneDriver]'s probe: a marker file around the probe,
     * with the install's key and the count of probes that did not finish, and after
     * [PhoneDriver.BLOCK_AFTER] in a row no probe for that install. Call before the first [report].
     */
    fun init(context: Context) {
        if (marker != null) return
        PhoneDriver.init(context)
        marker = File(context.filesDir, MARKER)
    }

    @Synchronized
    fun report(): PhoneGlesDriver? {
        if (!read) {
            report = probeOnce()
            Log.i(TAG, "the phone's OpenGL ES driver: $report")
            read = true
        }
        return report
    }

    private fun probeOnce(): PhoneGlesDriver? {
        val mark = marker
        val key = PhoneDriver.installKey
        val left = try {
            mark?.takeIf { it.isFile }?.readText()
        } catch (_: IOException) {
            null
        }
        if (PhoneDriver.blocked(left, key)) {
            Log.w(TAG, "the OpenGL ES probe did not finish ${PhoneDriver.BLOCK_AFTER} times: no probe for this install")
            return null
        }
        try {
            mark?.writeText(PhoneDriver.marking(left, key))
        } catch (_: IOException) {
        }
        return try {
            probe()
        } catch (error: Throwable) {
            Log.w(TAG, "OpenGL ES probe failed: $error")
            null
        } finally {
            mark?.delete()
        }
    }

    /**
     * The probe, as vrend makes its context: the default display; the highest OpenGL ES 3.x
     * context of 3.2, 3.1 and 3.0 (with EGL_KHR_create_context's version attributes); current
     * with no surface. It makes no surface, gives back the thread's earlier context, destroys its
     * own, and does not terminate the display, which the app's own drawing shares.
     */
    private fun probe(): PhoneGlesDriver? {
        val display = EGL14.eglGetDisplay(EGL14.EGL_DEFAULT_DISPLAY)
        if (display == EGL14.EGL_NO_DISPLAY || !EGL14.eglInitialize(display, IntArray(2), 0, IntArray(2), 1)) return null
        val extensions = EGL14.eglQueryString(display, EGL14.EGL_EXTENSIONS).orEmpty().split(' ').toSet()
        val surfaceless = "EGL_KHR_surfaceless_context" in extensions
        val createContext = "EGL_KHR_create_context" in extensions
        val configs = arrayOfNulls<android.opengl.EGLConfig>(1)
        val count = IntArray(1)
        val attributes = intArrayOf(
            EGL14.EGL_SURFACE_TYPE, EGL14.EGL_PBUFFER_BIT,
            EGL14.EGL_RENDERABLE_TYPE, EGL14.EGL_OPENGL_ES2_BIT,
            EGL14.EGL_RED_SIZE, 1, EGL14.EGL_GREEN_SIZE, 1, EGL14.EGL_BLUE_SIZE, 1,
            EGL14.EGL_NONE,
        )
        val config = EGL14.eglChooseConfig(display, attributes, 0, configs, 0, 1, count, 0) && count[0] == 1
        fun driver(version: Pair<Int, Int> = 0 to 0, current: Boolean = false, renderer: String = "") =
            PhoneGlesDriver(renderer, version, surfaceless, createContext, config, current)
        if (!config || !EGL14.eglBindAPI(EGL14.EGL_OPENGL_ES_API)) return driver()
        var context = EGL14.EGL_NO_CONTEXT
        var asked = 0 to 0
        for (minor in if (createContext) listOf(2, 1, 0) else listOf(0)) {
            val contextAttributes = if (createContext) {
                intArrayOf(EGLExt.EGL_CONTEXT_MAJOR_VERSION_KHR, 3, EGLExt.EGL_CONTEXT_MINOR_VERSION_KHR, minor, EGL14.EGL_NONE)
            } else {
                intArrayOf(EGL14.EGL_CONTEXT_CLIENT_VERSION, 3, EGL14.EGL_NONE)
            }
            context = EGL14.eglCreateContext(display, configs[0], EGL14.EGL_NO_CONTEXT, contextAttributes, 0)
            if (context != EGL14.EGL_NO_CONTEXT) {
                asked = 3 to minor
                break
            }
        }
        if (context == EGL14.EGL_NO_CONTEXT) return driver()
        val previousDisplay = EGL14.eglGetCurrentDisplay()
        val previousContext = EGL14.eglGetCurrentContext()
        val previousDraw = EGL14.eglGetCurrentSurface(EGL14.EGL_DRAW)
        val previousRead = EGL14.eglGetCurrentSurface(EGL14.EGL_READ)
        try {
            if (!surfaceless || !EGL14.eglMakeCurrent(display, EGL14.EGL_NO_SURFACE, EGL14.EGL_NO_SURFACE, context)) {
                return driver(asked)
            }
            // The context's own version: a driver may give more than was asked for.
            val major = IntArray(1)
            val minor = IntArray(1)
            GLES30.glGetIntegerv(GLES30.GL_MAJOR_VERSION, major, 0)
            GLES30.glGetIntegerv(GLES30.GL_MINOR_VERSION, minor, 0)
            val version = if (major[0] >= 3) major[0] to minor[0] else asked
            // Only the extensions of vrend's GLSL rule (PhoneGlesDriver.vrendGlslLevel): the log stays short.
            val extensions = GLES20.glGetString(GLES20.GL_EXTENSIONS).orEmpty().split(' ')
                .filter { it in PhoneGlesDriver.GLSL_EXTENSIONS }.toSet()
            return driver(version, current = true, renderer = GLES20.glGetString(GLES20.GL_RENDERER).orEmpty()).copy(extensions = extensions)
        } finally {
            if (previousDisplay != EGL14.EGL_NO_DISPLAY) {
                EGL14.eglMakeCurrent(previousDisplay, previousDraw, previousRead, previousContext)
            } else {
                EGL14.eglMakeCurrent(display, EGL14.EGL_NO_SURFACE, EGL14.EGL_NO_SURFACE, EGL14.EGL_NO_CONTEXT)
            }
            EGL14.eglDestroyContext(display, context)
        }
    }
}
