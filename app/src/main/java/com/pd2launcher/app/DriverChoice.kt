package com.pd2launcher.app

import android.content.Context
import android.util.Log
import com.pd2launcher.app.control.Renderer
import java.io.File
import java.io.IOException

/**
 * The ways the game's graphics can reach the GPU, in the app's measured order
 * ([DriverOptions.route]): the order gives the default pick of Settings → Display → GPU
 * ([DriverPage.defaultPick]) and the Phone driver's first way; the pick is the route
 * ([DriverPage.route]; the owner, 2026-11-02: "why don't we just let people choose?"), and no
 * failure changes it (the owner, 2026-11-04).
 *
 * Where the built-in driver supports the GPU, it is the route of a player on the App driver; a
 * debug build can force another route ([SystemDriver.forced]). Of the routes to the phone's own driver, [VENUS] comes first;
 * [VIRGL] is for a phone whose Vulkan cannot run Zink. On Samsung's Xclipse GPUs the order is the other way
 * round ([DriverOptions.virglFirst]); so it is on Qualcomm's Adreno GPUs that take the phone's
 * own driver ([DriverChoice.QUALCOMM_VIRGL_FIRST], the user's decision of 2026-10-24). On a Samsung
 * Xclipse GPU, [RADV] comes before them where the App userland has it ([DriverChoice.RADV_FOR_PLAYERS],
 * the owner's decision of 2026-10-28). On Imagination's PowerVR GPUs [LIBHYBRIS] comes before both
 * (the owner's decision of 2026-10-28, "Productize libhybris").
 * On an Arm Mali GPU of kbase's job manager (Bifrost v6/v7, Valhall v9), [MALI_PANVK] comes before
 * them where the App userland has it ([DriverChoice.MALI_PANVK_FOR_PLAYERS]).
 */
enum class GraphicsRoute(
    /**
     * A route for a GPU that the built-in drivers do not support (the phone-driver work, in the
     * system-driver part): Play shows its amber line ([com.pd2launcher.app.ui.Play.driverView]).
     */
    val phoneDriver: Boolean,
    /**
     * The route needs a server of the APK in the game environment (virglrenderer's vtest server,
     * [GameEnvironment]): Venus and VirGL; RADV, mali-panvk and libhybris have none.
     */
    val server: Boolean = false,
    /**
     * The route takes the phone's own driver as it is, through a server: Venus and VirGL (the owner,
     * 2026-10-29, "virGL and Venus the only agnostic solutions"). Player texts do not use this: they take their
     * words from [player] (App driver or Phone driver, the owner's names of 2026-11-02; libhybris is
     * a Phone driver, decision 21). No driver names in a player's text, except Play's driver label
     * ([DriverPage.label]).
     */
    val nativeDriver: Boolean = false,
) {
    /** The driver made for this app: Turnip on an Adreno, PanVK on a Mali. */
    BUILT_IN(phoneDriver = false),

    /**
     * Our RADV for Samsung's Xclipse GPUs ([Radv]): the product Mesa's Zink on it, on the GPU's DRM
     * node, with no server; the direct present and the app's clock as with Turnip.
     */
    RADV(phoneDriver = true),

    /**
     * The community PanVK tree mali-panvk for Arm Mali GPUs on kbase's job manager ([MaliPanvk]):
     * the product Mesa's Zink on it, on `/dev/mali0`, with no server; the direct present and the
     * app's clock as with our PanVK, which stays the CSF Malis' driver ([BUILT_IN]).
     */
    MALI_PANVK(phoneDriver = true),

    /** The phone's own Vulkan driver: Zink on Mesa's Venus, over vtest to virglrenderer's server in the APK. */
    VENUS(phoneDriver = true, server = true, nativeDriver = true),

    /**
     * The phone's own OpenGL ES driver: Mesa's virgl driver, over vtest to the APK's second
     * virglrenderer server, whose OpenGL renderer (vrend) draws with that driver. Slower than
     * [VENUS] (each frame comes back from the server to the X screen), and none of the Vulkan
     * present's pacing or direct present.
     */
    VIRGL(phoneDriver = true, server = true, nativeDriver = true),

    /**
     * The phone's own Vulkan driver in the game's own process: Zink on our ICD
     * (libvulkan_pd2hybris.so, userland build-hybris.sh), which loads the vendor's Vulkan HAL
     * through libhybris's copy of bionic's linker and gives Zink an X11 swapchain of its own. No
     * server; a fault of the vendor's code ends the game itself ([LibhybrisStatus] tells a driver
     * that did not start). For PowerVR, where Venus has no memory path and VirGL drew nothing.
     */
    LIBHYBRIS(phoneDriver = true, server = false),

    /**
     * The Software driver (the owner, 2026-11-03: "software driver renders the game using software
     * can be really slow"): no GPU driver in the game's environment (no Vulkan driver,
     * [SessionPaths.engineEnvironment]), and the game in DirectDraw with cnc-ddraw's GDI renderer
     * ([GlideFirst.commands]; the engine also writes `adjmouse=true` with it). Never in
     * [DriverOptions.routes]: only the pick takes it ([DriverPage.route]; the default where no GPU
     * route can run, [DriverPage.defaultPick]), on every GPU, and no failure rule follows it.
     */
    SOFTWARE(phoneDriver = false),
    ;

    /** The row of Settings → Display → GPU that the route belongs to ([DriverPage]). libhybris loads the phone's own driver: a Phone driver (decision 21). */
    val player: PlayerDriver
        get() = when (this) {
            BUILT_IN, RADV, MALI_PANVK -> PlayerDriver.APP
            LIBHYBRIS, VENUS, VIRGL -> PlayerDriver.PHONE
            SOFTWARE -> PlayerDriver.SOFTWARE
        }
}

/** Which routes can run the game on this device ([DriverChoice.options]). */
data class DriverOptions(
    /** The built-in Turnip or PanVK drives the GPU and knows its model ([DeviceCheck.bundledDriverKnows]). */
    val builtIn: Boolean,
    /** The phone's own Vulkan driver can run Zink over Venus, and the APK and the App userland have the mode's parts. */
    val venus: Boolean,
    /** The phone's own OpenGL ES driver meets vrend's needs ([PhoneGlesDriver.problem]), and the APK has its server. */
    val virgl: Boolean = false,
    /**
     * VirGL before Venus where both can run ([DriverChoice.options]): Samsung's Xclipse driver
     * ([SystemDriverGpu.xclipse]), Qualcomm's ([qualcomm]) and Arm's outside the built-in PanVK's
     * range ([mali]): the Phone driver's first way.
     */
    val virglFirst: Boolean = false,
    /** [virglFirst] is for a Qualcomm Adreno GPU ([SystemDriverGpu.qualcomm]), not Xclipse; only for the log text. */
    val qualcomm: Boolean = false,
    /** [virglFirst] is for an Arm Mali GPU ([SystemDriverGpu.mali]) outside the built-in PanVK range; only for the log text. */
    val mali: Boolean = false,
    /**
     * Our RADV can run the game: a Samsung Xclipse GPU ([SystemDriverGpu.xclipse]), RADV offered
     * here ([Radv.allowed]) and in the App userland ([Radv.filesPresent]). It comes first.
     */
    val radv: Boolean = false,
    /**
     * mali-panvk can run the game: an Arm Mali GPU of kbase's job manager that the tree knows
     * ([MaliPanvk.gpu]), the route offered here ([MaliPanvk.allowed]) and in the App userland
     * ([MaliPanvk.filesPresent]). It comes first.
     */
    val maliPanvk: Boolean = false,
    /**
     * The phone's own Vulkan driver can run Zink in the game's process through libhybris
     * ([GraphicsRoute.LIBHYBRIS]): Imagination's PowerVR driver that the product Zink takes, and
     * the APK and the App userland have the route's files ([DriverChoice.options]). First of the
     * routes to the phone's own driver.
     */
    val libhybris: Boolean = false,
) {
    /** The routes that can run here, in the app's order ([GraphicsRoute], [radv], [maliPanvk], [libhybris], [virglFirst]). */
    val routes: List<GraphicsRoute>
        get() = buildList {
            if (builtIn) add(GraphicsRoute.BUILT_IN)
            if (radv) add(GraphicsRoute.RADV)
            if (maliPanvk) add(GraphicsRoute.MALI_PANVK)
            if (libhybris) add(GraphicsRoute.LIBHYBRIS)
            if (virglFirst && virgl) add(GraphicsRoute.VIRGL)
            if (venus) add(GraphicsRoute.VENUS)
            if (!virglFirst && virgl) add(GraphicsRoute.VIRGL)
        }

    /** Why the routes to the phone's own driver are in another order than Venus, then VirGL, for the logs; null where they are not. */
    val orderReason: String?
        get() = listOfNotNull(
            radvOrderReason,
            maliPanvkOrderReason,
            libhybrisOrderReason,
            phoneOrderReason,
        ).joinToString("; ").ifEmpty { null }

    /** Why RADV comes first on an Xclipse, for the logs; null where it does not. */
    private val radvOrderReason: String?
        get() = if (radv) {
            val after = routes.dropWhile { it != GraphicsRoute.RADV }.drop(1)
            "Samsung Xclipse GPU: RADV first (radv-xclipse), " +
                "${if (after.isEmpty()) "no other route" else "the Phone driver's ways ${after.joinToString(", ")} for a pick of it"} " +
                "(DriverChoice.RADV_FOR_PLAYERS; the owner's decision of 2026-10-28 after the Test Lab game runs on the S24 Exynos: " +
                "60/60 fps, 24.0 ms of CPU a frame against VirGL's 25.2 and Venus's 31.5, 0 faults in 5 game starts)"
        } else {
            null
        }

    /** Why mali-panvk comes first on a job-manager Mali, for the logs; null where it does not. */
    private val maliPanvkOrderReason: String?
        get() = if (maliPanvk) {
            val after = routes.dropWhile { it != GraphicsRoute.MALI_PANVK }.drop(1)
            "Arm Mali GPU of kbase's job manager: mali-panvk first, " +
                "${if (after.isEmpty()) "no other route" else "the Phone driver's ways ${after.joinToString(", ")} for a pick of it"} " +
                "(DriverChoice.MALI_PANVK_FOR_PLAYERS)"
        } else {
            null
        }

    /** Why libhybris comes first on a PowerVR, for the logs; null where it does not. */
    private val libhybrisOrderReason: String?
        get() = if (libhybris && !builtIn) {
            val after = routes.dropWhile { it != GraphicsRoute.LIBHYBRIS }.drop(1)
            "Imagination PowerVR GPU on the phone's own driver: libhybris first (the phone's Vulkan driver in the game's process), " +
                "${if (venus || virgl) "then ${after.joinToString(", ")} as the Phone driver's other ways" else "no other route"} " +
                "(DriverChoice.options; the owner's decision of 2026-10-28: on the Pixel 10 only this route showed the game)"
        } else {
            null
        }

    /** Why VirGL comes before Venus, for the logs; null where it does not. */
    private val phoneOrderReason: String?
        get() = if (virglFirst && qualcomm) {
            "Qualcomm Adreno GPU: on the phone's own driver VirGL first, Venus second (DriverChoice.QUALCOMM_VIRGL_FIRST; " +
                "Venus drew the text wrong on Qualcomm's Vulkan driver 512.x, VirGL with the integer types draws it right)"
        } else if (virglFirst && mali) {
            "Arm Mali GPU on the phone's own driver (outside the built-in PanVK range): VirGL first, Venus second (DriverChoice.MALI_VIRGL_FIRST; " +
                "Venus failed on 5 of 5 such phones, VirGL drew right on 4 of 5)"
        } else if (virglFirst) {
            "Samsung Xclipse GPU: VirGL first, Venus second (DriverChoice.XCLIPSE_VIRGL_FIRST; " +
                "Test Lab game runs on the S24 Exynos: the same 60 fps, about 24.6 ms of CPU a frame against Venus's 31.8, less memory)"
        } else {
            null
        }

    /** The route the app takes: the first that can run; null where none can. */
    val route: GraphicsRoute? get() = routes.firstOrNull()

    /**
     * The App driver for a chip that Turnip and PanVK do not support tries first ([radv] or
     * [maliPanvk]): the device check says that this driver is new for the chip, not that the app has
     * none ([com.pd2launcher.app.ui.PhoneDriverDialog]). libhybris is a Phone driver (decision 21):
     * a PowerVR gets the "no driver" texts.
     */
    val newLauncherDriver: Boolean get() = !builtIn && (radv || maliPanvk)
}

/**
 * Why the app warns before and during setup that the game takes a route for a GPU that Turnip and
 * PanVK do not support (the user's request of 2026-10-24): the device check's dialog with Try
 * anyway ([com.pd2launcher.app.ui.PhoneDriverDialog]), and on the source page a line under
 * Blizzard's installers that they are not recommended, as they are the least likely to work on
 * such a phone (the owner, 2026-10-29: Fast download and an import are equal, so no text prefers
 * either; [com.pd2launcher.app.ui.SourceScreen]). Play's amber line follows the same rule
 * ([com.pd2launcher.app.ui.Play.driverView]).
 */
enum class PhoneDriverAdvice {
    /** The built-in drivers do not support the GPU, and a route to the phone's own driver can run. */
    UNSUPPORTED,

    /** A debug build forced a route to the phone's own driver ([SystemDriver.forced]) that the device would not take. */
    FORCED,
    ;

    companion object {
        /** The advice for [options] and a debug build's [forced] route; null where the game takes the built-in route, or none. */
        fun of(options: DriverOptions, forced: GraphicsRoute? = null): PhoneDriverAdvice? {
            val route = DriverChoice.route(options, forced) ?: return null
            return when {
                !route.phoneDriver -> null
                forced != null && route != options.route -> FORCED
                else -> UNSUPPORTED
            }
        }
    }
}

/** The route of the game's graphics on this device; pure, so the JVM tests check it. */
object DriverChoice {
    /**
     * The order of the routes to the phone's own driver on Samsung's Xclipse GPUs
     * ([SystemDriverGpu.xclipse]), the one switch of that policy, which the owner decides: true,
     * VirGL first and Venus second ([DriverOptions.virglFirst]); false, Venus
     * first and VirGL second. True since the owner's change of 2026-10-28 ("if
     * now our post estimation of running real application is now virgl first, then we should use
     * virgl first"); the Venus-first rule of 2026-10-24 came before the game was validated. In the
     * Test Lab game runs on a Galaxy S24 Exynos (Xclipse 940), the coordinating session's report:
     * both routes ran PD2 at 60 fps, VirGL with about 24.6 ms of CPU a frame against 31.8 ms for
     * Venus, and with less memory. Zink over Venus read vertex attributes as 0 on Samsung's driver
     * (driver 24.0.545), which D2GL's text uses, until the Venus server's patch 0009
     * (VK_EXT_vertex_input_dynamic_state hidden on driver id 21); VirGL with server patch
     * virglrenderer-gl/0004 reads them correctly too. RADV comes before both where the App userland
     * has it ([RADV_FOR_PLAYERS]): RADV, then VirGL, then Venus. The tests check both orders.
     */
    const val XCLIPSE_VIRGL_FIRST = true

    /**
     * The order of the routes to the phone's own driver on Qualcomm's Adreno GPUs
     * ([SystemDriverGpu.qualcomm]), the one switch of that
     * policy (the user's decision of 2026-10-24): true, VirGL first and Venus second
     * ([DriverOptions.virglFirst]); false, Venus first. On an Adreno 740 (Qualcomm
     * driver 512.x) VirGL with the integer types draws PD2 correctly (about 45 fps), and Venus
     * draws the text wrong (black boxes, missing glyphs). A GPU the built-in driver supports
     * keeps the built-in route first; these routes come after it, for its failures.
     */
    const val QUALCOMM_VIRGL_FIRST = true

    /**
     * The order of the routes to the phone's own driver on Arm Mali GPUs ([SystemDriverGpu.mali])
     * that the built-in PanVK cannot run (Mali-G52, G57, G68, G77 and older), the one switch of
     * that policy (the user's decision of 2026-10-24, "VirGL first"): true, VirGL first and Venus
     * second ([DriverOptions.virglFirst]); false, Venus first. Remote facts of
     * 2026-10-24: Venus failed on all 5 such phones (a shader-compiler crash in the render server
     * on r32p1 and r38p1, nothing drawn on r38p1, no memory path without /dev/dma_heap); VirGL
     * was correct on 4 of 5. A GPU the built-in driver supports keeps the built-in route.
     */
    const val MALI_VIRGL_FIRST = true

    /**
     * Whether the RADV route ([GraphicsRoute.RADV], our RADV for Samsung's Xclipse GPUs) is offered
     * to players, the one switch of that policy, which the owner decides: true, first on every
     * Xclipse where the APK and the unpacked App userland have RADV, then VirGL, then Venus
     * ([XCLIPSE_VIRGL_FIRST]); false, only where a debug build has [Radv.DEBUG_FILE]. True since the
     * owner's decision of 2026-10-28 ("Yes, RADV first for players"), after the Test Lab game runs
     * on a Galaxy S24 Exynos (Xclipse 940) with the merged APK: with the direct present and the
     * app's clock, RADV ran PD2 at 60/60 fps with 24.0 ms of CPU a frame (18.5 the game, 5.5 the
     * app) and walking gaps of 17 ms at most, against 25.2 ms and gaps of 50 ms for VirGL and
     * 31.5 ms for Venus, and 0 GPU faults or resets in 5 game starts.
     */
    const val RADV_FOR_PLAYERS = true

    /**
     * Whether the mali-panvk route ([GraphicsRoute.MALI_PANVK], the community PanVK tree for Arm
     * Mali GPUs on kbase's job manager) is offered to players, the one switch of that policy: true,
     * first on every job-manager Mali that the tree knows where the APK and the unpacked App
     * userland have it, then VirGL, then Venus ([MALI_VIRGL_FIRST]); false, only where a debug
     * build has [MaliPanvk.DEBUG_FILE].
     */
    const val MALI_PANVK_FOR_PLAYERS = true

    /**
     * The options for [gpu] ([HostGpu.probe]) and [phone] (the phone driver's report,
     * [PhoneDriver.report]; null when it was not read or no Vulkan device answered). The built-in
     * driver counts only where it also knows the GPU's model ([DeviceCheck.bundledDriverKnows]);
     * Venus only where the APK has the server ([serverInApk]) and [modeFiles]: the unpacked App
     * userland has the mode's files ([SystemDriver.filesPresent]; true before the first unpack,
     * which writes them where the mode can run, [skippedFiles], and a later unpack writes back
     * those an earlier one left out, [skippableFiles]). VirGL where the phone's OpenGL ES
     * driver ([gles], [PhoneGles]; null when it was not read or EGL did not answer) meets vrend's
     * needs and the APK has its server ([glServerInApk]); its userland part, the virgl driver,
     * is in the product Mesa, which every unpack writes. VirGL comes before Venus on Samsung's
     * Xclipse driver ([SystemDriverGpu.xclipse], [DriverOptions.virglFirst]) where both can run
     * and [xclipseVirglFirst] ([XCLIPSE_VIRGL_FIRST]) says so; the same on Qualcomm's Adreno
     * driver ([SystemDriverGpu.qualcomm]) with [qualcommVirglFirst] ([QUALCOMM_VIRGL_FIRST]), also
     * after the built-in route on an Adreno that Turnip runs (the Phone driver's ways there),
     * and on Arm's Mali driver ([SystemDriverGpu.mali]) with [maliVirglFirst] ([MALI_VIRGL_FIRST])
     * only outside the built-in PanVK's range.
     * The libhybris route ([DriverOptions.libhybris], [libhybrisRuns]) on Imagination's PowerVR
     * driver that the product Mesa's Zink takes, where the APK carries the route's userland files
     * ([hybrisInApk], in the system-driver part) and the unpacked userland has them ([hybrisFiles],
     * [SystemDriver.hybrisFilesPresent]; true before the first unpack); never where the built-in
     * driver runs the GPU.
     * RADV ([DriverOptions.radv]) on Samsung's Xclipse driver where it is offered ([radvAllowed],
     * [Radv.allowed]) and the App userland has it ([radvFiles]; true before the first unpack, as
     * [modeFiles]), never where the built-in driver supports the GPU. mali-panvk
     * ([DriverOptions.maliPanvk]) on a job-manager Mali that the tree knows ([MaliPanvk.gpu]: [gpu],
     * or the kernel's GPU beside the [phone] report) where it is offered ([maliPanvkAllowed],
     * [MaliPanvk.allowed]) and the App userland has it ([maliPanvkFiles], as [radvFiles]).
     */
    fun options(
        gpu: HostGpu?,
        phone: SystemDriverGpu?,
        serverInApk: Boolean,
        modeFiles: Boolean = true,
        gles: PhoneGlesDriver? = null,
        glServerInApk: Boolean = false,
        xclipseVirglFirst: Boolean = XCLIPSE_VIRGL_FIRST,
        qualcommVirglFirst: Boolean = QUALCOMM_VIRGL_FIRST,
        maliVirglFirst: Boolean = MALI_VIRGL_FIRST,
        hybrisInApk: Boolean = false,
        hybrisFiles: Boolean = true,
        radvAllowed: Boolean = RADV_FOR_PLAYERS,
        radvFiles: Boolean = true,
        maliPanvkAllowed: Boolean = MALI_PANVK_FOR_PLAYERS,
        maliPanvkFiles: Boolean = true,
    ): DriverOptions {
        val venus = serverInApk && modeFiles && phone != null && phone.problem() == null
        val virgl = glServerInApk && gles != null && gles.problem() == null
        val xclipse = xclipseVirglFirst && phone?.xclipse == true
        val builtIn = gpu != null && DeviceCheck.bundledDriverKnows(gpu)
        // Qualcomm: VirGL first also after the built-in Turnip, which stays first where it runs the GPU.
        val qualcomm = qualcommVirglFirst && phone?.qualcomm == true
        val mali = maliVirglFirst && phone?.mali == true && !builtIn
        return DriverOptions(
            builtIn = builtIn,
            venus = venus,
            virgl = virgl,
            virglFirst = venus && virgl && (xclipse || qualcomm || mali),
            qualcomm = venus && virgl && qualcomm && !xclipse,
            mali = venus && virgl && mali && !xclipse && !qualcomm,
            libhybris = hybrisInApk && hybrisFiles && !builtIn && phone != null && libhybrisRuns(phone),
            radv = radvAllowed && radvFiles && !builtIn && phone?.xclipse == true,
            maliPanvk = maliPanvkAllowed && maliPanvkFiles && !builtIn && MaliPanvk.gpu(gpu, phone) != null,
        )
    }

    /**
     * Whether the libhybris route can run on [phone]: Imagination's own driver
     * ([SystemDriverGpu.powerVr], the GPUs it was proven on)
     * with Vulkan 1.1 and what the product Mesa's Zink needs ([SystemDriverGpu.zinkCurrent]: Zink
     * runs on the driver itself on this route, not through Venus).
     */
    fun libhybrisRuns(phone: SystemDriverGpu): Boolean = phone.powerVr && phone.vulkan11 && phone.zinkCurrent

    /**
     * The route the next Play (and the official PD2 launcher) takes: the one of the pick in
     * Settings → Display → GPU ([chosen], [DriverPage.route]), or the one a debug build forces
     * ([forced], [SystemDriver.forced]) on any device, for tests of a route on our phones; the
     * forced one wins; where the pick has no route here, the first of [options] that can run
     * ([DriverOptions.route]). Null: none can run.
     */
    fun route(options: DriverOptions, forced: GraphicsRoute? = null, chosen: GraphicsRoute? = null): GraphicsRoute? =
        forced ?: chosen ?: options.route

    /**
     * Whether the app reads the phone's own drivers ([HostGpu.phoneDriver], [HostGpu.phoneGles])
     * also on a GPU that the built-in driver runs: for a debug build's [forced] route; in a debug
     * build ([debug]); where the player picked the Phone driver ([phonePicked], [DriverPage.route]),
     * for its routes. Never on a start of a release build on the App driver, which loads no vendor
     * driver there.
     */
    fun readsPhoneDriver(forced: Boolean, debug: Boolean = BuildConfig.DEBUG, phonePicked: Boolean = false): Boolean =
        forced || debug || phonePicked

    /**
     * The App userland files that a device never loads, which the unpack leaves out: the other
     * GPU family's Vulkan driver (PanVK on an Adreno, Turnip on a Mali; both where only the phone's
     * own driver runs), and in release builds ([debug] false; debug builds keep them for
     * [SystemDriver.forced]) the system-driver mode's Venus and older GL where the app takes the
     * built-in route, or where the phone driver's [report] says Venus cannot run (no report, as
     * after a failed probe of a GPU the built-in driver does not support, keeps them), or only the
     * older GL where the product Mesa's Zink runs Venus ([SystemDriver.Zink.CURRENT]); the same
     * where only the phone's own drivers run (a PowerVR without geometry shaders, say, takes VirGL
     * and never loads Venus). In release builds also RADV ([Radv.LIBRARY]), except where the RADV
     * route can run ([DriverOptions.radv]: a Samsung Xclipse GPU by the [report] that the built-in
     * driver does not support) and RADV is offered to players ([radvForPlayers],
     * [RADV_FOR_PLAYERS], true): every other GPU, and an Xclipse with no report, leaves it out.
     * The same for mali-panvk ([MaliPanvk.LIBRARY]): kept where its route can run
     * ([DriverOptions.maliPanvk]: a job-manager Mali that the tree knows) and it is offered to
     * players ([maliPanvkForPlayers], [MALI_PANVK_FOR_PLAYERS], true). A job-manager Mali with no
     * report of the phone's own driver ([MaliGpu.jobManager]) leaves out both bundled drivers, as a
     * GPU of the phone's own driver does. Nothing where no GPU answers. A later unpack writes back
     * what it no longer leaves out ([skippableFiles]). Where the player picked the Phone driver,
     * [options] come without the App driver's routes ([DriverPage.withoutApp]), so the unpack writes
     * the files of the Phone driver's routes.
     */
    fun skippedFiles(
        gpu: HostGpu?,
        options: DriverOptions,
        report: SystemDriverGpu?,
        debug: Boolean,
        radvForPlayers: Boolean = RADV_FOR_PLAYERS,
        maliPanvkForPlayers: Boolean = MALI_PANVK_FOR_PLAYERS,
    ): Set<String> {
        val mode = when {
            debug -> emptySet()
            options.route == GraphicsRoute.BUILT_IN -> SystemDriver.userlandFiles
            report == null -> emptySet()
            !options.venus -> SystemDriver.userlandFiles
            report.zink == SystemDriver.Zink.CURRENT -> setOf(SystemDriver.GL_LIBRARY)
            else -> emptySet()
        }
        // The libhybris route's libraries: left out the same way, where the route cannot run
        // (a GPU of the built-in driver, or a phone driver that is not PowerVR).
        val hybris = when {
            debug -> emptySet()
            options.libhybris -> emptySet()
            options.route == GraphicsRoute.BUILT_IN -> SystemDriver.hybrisFiles
            report == null -> emptySet()
            !libhybrisRuns(report) -> SystemDriver.hybrisFiles
            else -> emptySet()
        }
        val radv = if (debug || (radvForPlayers && options.radv && report?.xclipse == true)) emptySet() else setOf(Radv.LIBRARY)
        val maliPanvk = if (debug || (maliPanvkForPlayers && options.maliPanvk)) emptySet() else setOf(MaliPanvk.LIBRARY)
        return when (gpu) {
            is MaliGpu -> (if (gpu.jobManager) setOf(TURNIP, PANVK) else setOf(TURNIP)) + mode + hybris + radv + maliPanvk
            is KgslGpu -> setOf(PANVK) + mode + hybris + radv + maliPanvk
            is SystemDriverGpu -> setOf(TURNIP, PANVK) + mode + hybris + radv + maliPanvk
            null -> emptySet()
        }
    }

    private const val TURNIP = "usr/lib/aarch64-linux-gnu/libvulkan_freedreno.so"
    private const val PANVK = "usr/lib/aarch64-linux-gnu/libvulkan_panfrost.so"

    /**
     * Every file that [skippedFiles] can leave out. A complete App userland that lacks one of them
     * which the unpack no longer leaves out is unpacked again before the engine starts
     * ([UserlandInstaller]'s `skippable`): after an app update or a new probe result that gives a
     * route needing it (Venus after VirGL, say), within the same userland version.
     */
    val skippableFiles: Set<String> = setOf(TURNIP, PANVK, Radv.LIBRARY, MaliPanvk.LIBRARY) + SystemDriver.userlandFiles + SystemDriver.hybrisFiles
}

/**
 * What the session does with its running engine when the route that the game needs ([wanted])
 * may differ from the engine's ([running]; SessionService's applyGraphicsRoute): a pick in
 * Settings → Display → GPU, which the session follows at once, so that one press of
 * Play then starts the game on it (the owner, 2026-10-29); a debug
 * build's forced route, at the next Play. Pure, so the JVM tests check it.
 */
enum class RouteChange {
    /** Nothing: the route is the same, or the session ends ([ending]: a Close or a stop). */
    NONE,

    /** The game, a job, a launch or Play's update check runs ([busy]): the change waits for its end. */
    WAIT,

    /** The game environment and the engine start again on [wanted] now. */
    RESTART,
    ;

    companion object {
        /** [busy] is asked only where the route changes. */
        fun of(wanted: GraphicsRoute?, running: GraphicsRoute?, ending: Boolean, busy: () -> Boolean): RouteChange = when {
            wanted == running || ending -> NONE
            busy() -> WAIT
            else -> RESTART
        }
    }
}

/**
 * The libhybris route's status file ([SessionPaths.hybrisStatus], the ICD's
 * `PD2_HYBRIS_STATUS_FILE`): one line per event, `<process> <pid> ok <what>` when the phone's
 * Vulkan driver made a device, `<process> <pid> failed <why>` when it did not load, made no
 * instance or no device, `<process> <pid> fault <what>` when the ICD's fault watch saw a crash in
 * the phone's driver, libhybris or the ICD. Pure, so the JVM tests check it.
 */
object LibhybrisStatus {
    /** The game's process name as Wine gives it (/proc/<pid>/comm). */
    private const val GAME = "Game.exe"

    /**
     * Why the phone's driver failed in the game ([text], the status file of one game): a `fault`
     * line of any of the game environment's Wine processes (a crash in the phone's driver,
     * libhybris or the ICD, also after an `ok` line: Wine's explorer.exe loads the driver too, and
     * a crash there leaves Game.exe hanging before it makes a device; the Test Lab run
     * 20261028-071040), else the reason of Game.exe's first `failed` line where Game.exe has no
     * `ok` line; null where Game.exe made a device and nothing crashed, or where it wrote nothing
     * (another rule then: no frame). In a game that draws with GDI ([gdi], [GdiGame]) only a crash
     * counts: the game does not use the GPU, and Wine's OpenGL start in it may fail.
     */
    fun gameFailure(text: String?, gdi: Boolean = false): String? {
        val all = lines(text)
        all.firstOrNull { it.fault }?.let { return "a crash in the phone's Vulkan driver (${it.process}: ${it.what})" }
        if (gdi) return null
        val game = all.filter { it.process == GAME }
        if (game.any { it.ok }) return null
        return game.firstOrNull { !it.ok }?.what
    }

    /** One line of the file: `ok`, `failed`, or `fault` ([fault]: a crash, written by the ICD's signal handler). */
    data class Line(val process: String, val pid: Long, val ok: Boolean, val what: String, val fault: Boolean = false)

    /** The lines of [text] in order; lines of another form are skipped. */
    fun lines(text: String?): List<Line> = text.orEmpty().lineSequence().mapNotNull { line ->
        val parts = line.trim().split(' ', limit = 4)
        if (parts.size < 3) return@mapNotNull null
        val pid = parts[1].toLongOrNull() ?: return@mapNotNull null
        when (parts[2]) {
            "ok" -> Line(parts[0], pid, true, parts.getOrElse(3) { "" })
            "failed" -> Line(parts[0], pid, false, parts.getOrElse(3) { "" })
            "fault" -> Line(parts[0], pid, false, parts.getOrElse(3) { "" }, fault = true)
            else -> null
        }
    }.toList()
}

/**
 * One fact per game of the app's Play: the engine said at this game's start that DirectDraw draws
 * with GDI ([PhoneDriverFailure.drawsWithGdi], [feed] with each line of its output). Such a game
 * does not use the GPU, so no line of a route's failure watch ([BuiltInFailureWatch],
 * [MaliPanvkFailureWatch], [RadvFailureWatch]) and no failed start of the phone's driver on the
 * libhybris route ([LibhybrisStatus.gameFailure]) counts in it, also one that came earlier in
 * the same Play. [reset] at each Play.
 */
class GdiGame {
    @Volatile private var seen = false

    /** Whether this game draws with GDI. */
    val gdi: Boolean get() = seen

    /** One line of the engine's output. */
    fun feed(line: String) {
        if (PhoneDriverFailure.drawsWithGdi(line)) seen = true
    }

    /** A new game: no fact yet. */
    fun reset() {
        seen = false
    }
}

/**
 * The game's driver failed: Play says which driver had an error, in amber, with a button to
 * Settings → Display → GPU ([tells]; [com.pd2launcher.app.ui.Play.failureLine]). The route stays:
 * no failure changes the driver (the owner, 2026-11-04: "our recovery screen already takes them
 * back to settings so they can switch it back thats better than automatically changing stuff for
 * them"). On the first game on a new driver a card over the game asks the player ([asks]); its
 * Quit stops the game and opens the GPU page. Pure, so the JVM tests check it.
 */
object PhoneDriverFailure {
    /**
     * Games in a row with no frame after which Play tells of an error ([tells]): one such game can
     * also be a crash of the game itself before its first frame.
     */
    const val NO_FRAME_GAMES_TO_TELL = 2

    /**
     * Whether [line] of the engine's output is its line at this game's start that DirectDraw draws
     * with GDI (`process.rs`, "DirectDraw draws with gdi (ddraw.ini …)"): such a game does not use
     * the GPU, so the GL and Zink failure lines that come with it (Wine's OpenGL start, which fails
     * where no Vulkan driver is) are no failure of the route's driver ([BuiltInFailureWatch],
     * [MaliPanvkFailureWatch], [RadvFailureWatch]). Glide and DirectDraw on the GPU (OpenGL,
     * Direct3D 9) do not show it.
     */
    fun drawsWithGdi(line: String): Boolean = line.startsWith("pd2-launcher: DirectDraw draws with gdi ")

    /**
     * Whether Play tells of the failure: on [strong] evidence of a driver failure (the server gave
     * up a game context or did not start, the picture froze, a route's failure watch saw its line,
     * libhybris's driver failed) at once, else after [NO_FRAME_GAMES_TO_TELL] games in a row with
     * no frame ([noFrameGames], this one counted). Never for a debug build's [forced] route: the
     * test's own logs tell it.
     */
    fun tells(forced: Boolean, strong: Boolean, noFrameGames: Int): Boolean =
        !forced && (strong || noFrameGames >= NO_FRAME_GAMES_TO_TELL)

    /**
     * Whether the running engine's route ([engineRoute]) is the pick now ([chosenRoute],
     * [DriverPage.route]), read at its use (bug B1), for the shared logs; never with a debug build's
     * [forced] route.
     */
    fun chosen(engineRoute: GraphicsRoute?, chosenRoute: GraphicsRoute?, forced: Boolean): Boolean =
        !forced && engineRoute != null && chosenRoute == engineRoute

    /**
     * Whether a game on [route] shows the card that asks if it works
     * ([com.pd2launcher.app.ui.RouteCard]): the first game on a new driver (the owner, 2026-11-04,
     * "First game on a new driver": the default of the device check, or a new pick), that is a
     * route that no game [confirmed] yet ([RouteState.confirmed]: It works, or a game with frames).
     * Never a [forced] route, and only with Glide ([renderer]): a DirectDraw game's GDI renderer
     * does not use the driver.
     */
    fun asks(route: GraphicsRoute?, confirmed: GraphicsRoute?, forced: Boolean, renderer: Renderer?): Boolean =
        !forced && renderer == Renderer.GLIDE && route != null && route != confirmed

    /**
     * Whether It works on the card counts: the game sent a frame since its start ([presentsAtStart],
     * [presentsNow]). The answer closes the card in any case, but over a game with no frame it writes
     * no record, so the card comes back in the next game.
     */
    fun answerCounts(presentsAtStart: Long?, presentsNow: Long?): Boolean =
        presentsAtStart != null && presentsNow != null && presentsAtStart != presentsNow

    /**
     * Whether the present counter tells if a game on [route] with [renderer] sent frames ([failed]):
     * on the built-in route only with Glide, as a DirectDraw game's GDI renderer counts no presents
     * ([FrameWatch]) and says nothing about the driver; never on the Software driver
     * ([GraphicsRoute.SOFTWARE]), which has no GPU driver.
     */
    fun countsFrames(route: GraphicsRoute?, renderer: Renderer?): Boolean =
        route != null && route != GraphicsRoute.SOFTWARE && (renderer == Renderer.GLIDE || route != GraphicsRoute.BUILT_IN)

    /** A game in the mode with no frame for this long is hung, also when a Stop ends it ([failed]). */
    const val NO_FRAME_HANG_MS = 60_000L

    /**
     * Whether a game of the app's own Play that ran on a route whose failures the app watches
     * ([systemDriver]: a route to the phone's driver, or an App driver's) for [ranMs] failed in
     * it: the server gave up the game's context while it ran ([contextsLost],
     * [ContextLossWatch]: the phone's driver failed a command or the GPU hung), or it sent no frame
     * (the present counter at its start and at its end, [PresentCounter.read]; Mesa counts each
     * frame, direct or to the X screen; without a count at both ends, no frame never counts) and
     * either ended by itself or was stopped ([stoppedByApp]) after [NO_FRAME_HANG_MS] (a Stop
     * before that can come while the first frame is still being made), or its picture froze while
     * frames came ([frozen], [FrozenPicture]; the VirGL, libhybris, mali-panvk and built-in
     * routes), also when a Stop ended it.
     */
    fun failed(
        systemDriver: Boolean,
        presentsAtStart: Long?,
        presentsAtEnd: Long?,
        contextsLost: Int = 0,
        stoppedByApp: Boolean = false,
        ranMs: Long = 0,
        frozen: Boolean = false,
    ): Boolean {
        if (!systemDriver) return false
        if (contextsLost > 0 || frozen) return true
        val noFrame = presentsAtStart != null && presentsAtEnd != null && presentsAtStart == presentsAtEnd
        return noFrame && (!stoppedByApp || ranMs >= NO_FRAME_HANG_MS)
    }
}

/**
 * The phone's own Vulkan driver's report ([SystemDriverProbe]), read once per app process: the
 * probe loads the vendor driver into the app's process. Null when no Vulkan device answers or
 * the probe fails.
 */
object PhoneDriver {
    private const val TAG = "pd2-sysdrv"
    private const val MARKER = "phone-driver-probe.running"
    /** Unfinished probes in a row of one install that stop the probe ([blocked]). */
    const val BLOCK_AFTER = 2
    private var read = false
    private var report: SystemDriverGpu? = null
    @Volatile private var marker: File? = null
    @Volatile private var key: String = BuildConfig.VERSION_CODE.toString()

    /** The install's key ([init]: the version code and the install time), for [PhoneGles]'s marker too. */
    val installKey: String get() = key

    /**
     * The probe loads the vendor's driver into the app's process; one that crashes or hangs there
     * would end every start at the device check. A marker file is written before the probe and
     * removed after it, with the install's key (the version code and the install time, so an
     * update or a reinstall starts again) and the count of probes that did not finish: after
     * [BLOCK_AFTER] in a row (not one kill of the app during the probe), no probe for that
     * install ([blocked]). Call before the first [report].
     */
    fun init(context: Context) {
        if (marker != null) return
        key = try {
            "${BuildConfig.VERSION_CODE}/${context.packageManager.getPackageInfo(context.packageName, 0).lastUpdateTime}"
        } catch (_: Exception) {
            BuildConfig.VERSION_CODE.toString()
        }
        marker = File(context.filesDir, MARKER)
    }

    /** The probes of the install [key] that did not finish, by a leftover marker's [markerText]. */
    fun unfinished(markerText: String?, key: String): Int {
        val parts = markerText?.trim()?.split(' ') ?: return 0
        return if (parts.size == 2 && parts[0] == key) parts[1].toIntOrNull() ?: 0 else 0
    }

    /** Whether a leftover marker ([markerText]) stops the probe of the install [key]. */
    fun blocked(markerText: String?, key: String): Boolean = unfinished(markerText, key) >= BLOCK_AFTER

    /** The marker's text while a probe of the install [key] runs. */
    fun marking(markerText: String?, key: String): String = "$key ${unfinished(markerText, key) + 1}"

    @Synchronized
    fun report(kernel: HostGpu?): SystemDriverGpu? {
        if (!read) {
            report = probeOnce(kernel)
            read = true
        }
        return report
    }

    private fun probeOnce(kernel: HostGpu?): SystemDriverGpu? {
        val mark = marker
        val left = try {
            mark?.takeIf { it.isFile }?.readText()
        } catch (_: IOException) {
            null
        }
        if (blocked(left, key)) {
            Log.w(TAG, "the probe of the phone's own driver did not finish $BLOCK_AFTER times: no probe for this install")
            return null
        }
        try {
            mark?.writeText(marking(left, key))
        } catch (_: IOException) {
        }
        return try {
            SystemDriverProbe.probe(kernel)
        } catch (error: Throwable) {
            // A vendor driver that fails in the probe, or a missing probe library: no such mode.
            Log.w(TAG, "system driver probe failed: $error")
            null
        } finally {
            mark?.delete()
        }
    }
}
