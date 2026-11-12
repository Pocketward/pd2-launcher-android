package com.pd2launcher.app

import androidx.annotation.StringRes
import java.io.File

/**
 * The three drivers of Settings → Display → GPU (the owner's names): the App driver (the
 * launcher's own: Turnip, PanVK, RADV, mali-panvk), the Phone driver (the phone's own: VirGL,
 * Venus, libhybris) and the Software driver (the game drawn by the processor,
 * [GraphicsRoute.SOFTWARE]). [GraphicsRoute.player] gives a route's driver.
 */
enum class PlayerDriver { APP, PHONE, SOFTWARE }

/** The Phone driver's two ways, in the radio rows under it while it is in use. */
enum class PhoneWay(@param:StringRes val label: Int, val route: GraphicsRoute) {
    VIRGL(R.string.driver_way_virgl, GraphicsRoute.VIRGL),
    VENUS(R.string.driver_way_venus, GraphicsRoute.VENUS),
}

/**
 * The chip as the page sees it: with an App driver, without one, a PowerVR (whose Phone driver
 * has one way), or one with no usable GPU driver at all ([NONE]: only the Software driver is left).
 */
enum class DriverChip { APP_DRIVER, NO_APP_DRIVER, POWERVR, NONE }

/**
 * Settings → Display → GPU (decision 23: the GPU row at the top of Display opens this sub page;
 * decision 39, the owner, 2026-11-04: "Wire the GPU page first"): "Your graphics chip: <name>.",
 * then the App driver, the Phone driver (with its VirGL and Venus rows under it while it is in use)
 * and the Software driver. A row greys out where its driver cannot run on the chip. The pick
 * ([AppSettings.driverPick], [AppSettings.driverWay]) is the route of every Play ([route]): the
 * device check stores the default for the GPU ([withDefault]), and only the player changes it (the
 * owner, 2026-11-04: no automatic change after errors; the error line on Play and the card's Quit
 * open this page). The page shows the route that the next Play takes ([onRoute]). Pure, so the JVM
 * tests check it.
 */
object DriverPage {
    /**
     * Whether the rows take a pick in this build: the one switch between "selectable" and "greyed
     * like Loot filters". False in 0.3.8 (the owner, 2026-11-03: "0.3.8 is disabled for now, it'll be
     * enabled in 0.4.0 beta"); true on the 0.4.0 line. Greyed, no row and no way row takes the focus
     * or a touch, so nothing writes a pick. The tests follow it, and one pins the line's value.
     */
    const val SELECTABLE = true

    /**
     * The route of the [pick] ([DriverChoice.route]'s `chosen`) on the device's [options]: the
     * Software driver on any device where the engine can run it ([software], [softwareRuns]); the
     * App driver's route where the chip has one (Turnip or PanVK, RADV, mali-panvk); the Phone
     * driver's route: libhybris on a PowerVR, else the picked [way] where it can run, else the
     * chip's first way. Null where the pick has no route here (none stored yet, the phone's drivers
     * not read yet, or the driver cannot run): the app's own order ([DriverOptions.route]). No
     * failure changes it.
     */
    fun route(options: DriverOptions, pick: PlayerDriver?, way: PhoneWay?, software: Boolean = true): GraphicsRoute? = when (pick) {
        PlayerDriver.SOFTWARE -> GraphicsRoute.SOFTWARE.takeIf { software }
        // Test 23: the Phone driver's pick is Approach 2 (mali-panvk) on a CSF Mali.
        PlayerDriver.PHONE -> if (Test23.approaches(options)) GraphicsRoute.MALI_PANVK else {
            val phone = options.routes.filter { it.player == PlayerDriver.PHONE }
            if (GraphicsRoute.LIBHYBRIS in phone) GraphicsRoute.LIBHYBRIS else way?.route?.takeIf { it in phone } ?: phone.firstOrNull()
        }
        PlayerDriver.APP -> options.routes.firstOrNull { it.player == PlayerDriver.APP }
        null -> null
    }

    /** The device's [options] have a route of the App driver (Turnip or PanVK, RADV, mali-panvk). */
    fun hasApp(options: DriverOptions): Boolean = options.builtIn || options.radv || options.maliPanvk

    /**
     * The one automatic pick (the owner, 2026-11-04: "only this automatic pick on the check first
     * step ... we set a default based on their gpu detection"), from the device's [options]: the App
     * driver on a chip that has one; else the Phone driver with the chip's measured-first way
     * ([DriverOptions.routes]; no way on a PowerVR, whose Phone driver is libhybris); else the
     * Software driver.
     */
    fun defaultPick(options: DriverOptions): Pair<PlayerDriver, PhoneWay?> {
        if (hasApp(options)) return PlayerDriver.APP to null
        val phone = options.routes.filter { it.player == PlayerDriver.PHONE }
        if (phone.isEmpty()) return PlayerDriver.SOFTWARE to null
        if (GraphicsRoute.LIBHYBRIS in phone) return PlayerDriver.PHONE to null
        return PlayerDriver.PHONE to PhoneWay.entries.firstOrNull { it.route == phone.first() }
    }

    /**
     * [settings] after the device check on the GPU [gpu] ([gpuKey]; null: no GPU answered) with the
     * device's [options] ([DeviceCheckRun], at each start of the app's process): with no pick stored
     * (the first run, an update from 0.3.8, whose keys are not read) or a pick stored for another
     * GPU that answered (a restore on another phone), the [defaultPick] for this GPU; a pick of an
     * earlier 0.4.0 build (no GPU stored) stays, and the GPU is stored with it; else no change. A
     * failed probe ([gpu] null) is no other GPU (review MED-1): it only fills a missing pick, marked
     * as a default with no GPU ([AppSettings.driverDefaultNoGpu]), and it never replaces a stored
     * GPU. The first start where a GPU answers then computes that default again, unless the player
     * picked a driver since (re-review MED-A).
     */
    fun withDefault(settings: AppSettings, options: DriverOptions, gpu: String?): AppSettings = when {
        settings.driverPick == null ||
            (gpu != null && settings.driverDefaultNoGpu) ||
            (gpu != null && settings.driverGpu != null && settings.driverGpu != gpu) -> {
            val (pick, way) = defaultPick(options)
            settings.copy(driverPick = pick, driverWay = way, driverGpu = gpu ?: settings.driverGpu, driverDefaultNoGpu = gpu == null)
        }
        gpu != null && settings.driverGpu == null -> settings.copy(driverGpu = gpu)
        else -> settings
    }

    /**
     * The device check's log line of the default that [withDefault] stored ([before] to [after])
     * for the GPU [gpu] ([gpuKey]); null where the pick did not change.
     */
    fun defaultLine(gpu: String?, before: AppSettings, after: AppSettings): String? {
        if (after.driverPick == before.driverPick && after.driverWay == before.driverWay) return null
        return "graphics driver: the default for ${gpu ?: "no GPU"} is the ${after.driverPick?.name?.lowercase()} driver" +
            (after.driverWay?.let { " (${it.name.lowercase()})" } ?: "") +
            (before.driverPick?.let { " (was ${it.name.lowercase()} for ${before.driverGpu})" } ?: " (no pick stored)")
    }

    /**
     * The GPU's key for [AppSettings.driverGpu], from one stable source (review MED-1): the kernel
     * driver's name where it answers (KGSL, kbase; also inside the phone driver's report of a Mali
     * that PanVK does not take, [SystemDriverGpu.kernel], so a failed vendor probe gives the same
     * key), else the phone driver's name (Xclipse, PowerVR); null: no GPU answered.
     */
    fun gpuKey(gpu: HostGpu?): String? = when (gpu) {
        is SystemDriverGpu -> gpu.kernel?.name ?: gpu.name
        null -> null
        else -> gpu.name
    }

    /**
     * The route that the update from 0.3.8 keeps (the owner, 2026-11-04, review LOW-2: "Skip if
     * unchanged"): 0.3.8 ran only the built-in Turnip or PanVK, so where the default of [options]
     * is that route and [before] (the settings before the default; no pick stored, a game played,
     * and no route records of the 0.4.0 line, [lineRecords], [RouteState.lineRecords]; re-review
     * LOW-A) comes from a 0.3.8 player, the route is confirmed and the card does not show. Null: a
     * new install, a stored pick, an earlier build of the 0.4.0 line, or a driver that really changes.
     */
    fun unchangedRoute(before: AppSettings, options: DriverOptions, lineRecords: Boolean = false): GraphicsRoute? {
        if (before.driverPick != null || !before.gamePlayed || lineRecords) return null
        val (pick, way) = defaultPick(options)
        return route(options, pick, way).takeIf { it == GraphicsRoute.BUILT_IN }
    }

    /** The engine's capability for the Software driver in its welcome (review MED-3): `adjmouse=true` with `renderer=gdi`. */
    const val SOFTWARE_CAPABILITY = "gdi_adjmouse"

    /**
     * The APK's engine has [SOFTWARE_CAPABILITY]: the device check needs it before the engine's
     * first welcome (the session starts only after the check passed), for a device where no GPU
     * route runs ([DeviceFacts.software]). The 0.4.0 line's engine.lock pins v0.1.13 or newer,
     * whose welcome names it (a JVM test checks the pin); at run time the welcome still decides
     * ([softwareRuns]).
     */
    const val BUNDLED_ENGINE_SOFTWARE = true

    /** Whether the engine of [welcome] can run the Software driver ([SOFTWARE_CAPABILITY]); engine 0.1.12 cannot. */
    fun softwareRuns(welcome: com.pd2launcher.app.control.EngineMessage.Welcome): Boolean = SOFTWARE_CAPABILITY in welcome.capabilities

    /** [options] without the App driver's routes: what a player on the Phone driver needs unpacked ([DriverChoice.skippedFiles]). */
    fun withoutApp(options: DriverOptions): DriverOptions = options.copy(builtIn = false, radv = false, maliPanvk = false)

    /**
     * Play's driver label in the top bar, as on the GPU page (the owner, 2026-11-04: "Like the GPU
     * page"): "App driver", "Phone driver · VirGL" or "Phone driver · Venus" (the way names as on the
     * page), "Phone driver" on a PowerVR (libhybris), "Software driver". No technology names: those
     * stay in About and in the logs.
     */
    fun label(route: GraphicsRoute): Line = Line(
        when (route) {
            GraphicsRoute.VIRGL -> R.string.driver_label_virgl
            GraphicsRoute.VENUS -> R.string.driver_label_venus
            GraphicsRoute.LIBHYBRIS -> R.string.driver_phone
            GraphicsRoute.SOFTWARE -> R.string.driver_software
            GraphicsRoute.BUILT_IN, GraphicsRoute.RADV, GraphicsRoute.MALI_PANVK -> R.string.driver_app
        },
    )

    /** Debug builds only: `files/debug/driver-page-chip` shows the page for another chip (`no-app-driver`, `powervr`, `none` or `mali`, then its name). */
    const val DEBUG_FILE = "debug/driver-page-chip"

    /**
     * The chip of a debug build's preview ([DEBUG_FILE]: [debugChip], its first word); without it,
     * the App driver where a GPU answers ([gpu]), else none. The page of the line takes the chip
     * from the routes ([onRoute]).
     */
    fun chip(gpu: HostGpu?, debugChip: String?): DriverChip = when (debugChip) {
        "no-app-driver" -> DriverChip.NO_APP_DRIVER
        "powervr" -> DriverChip.POWERVR
        "none" -> DriverChip.NONE
        else -> if (gpu != null) DriverChip.APP_DRIVER else DriverChip.NONE
    }

    /** The GPU whose way order the page shows: a debug build's `mali` word shows a Mali's (Venus first) on any chip. */
    fun orderGpu(gpu: HostGpu?, debugChip: String?): HostGpu? = if (debugChip == "mali") MaliGpu(0L, 0) else gpu

    /** The debug build's [DEBUG_FILE]: its chip word and the chip's name; null in other builds or without the file. */
    fun debugChip(filesDir: File, debug: Boolean = BuildConfig.DEBUG): Pair<String, String?>? {
        if (!debug) return null
        val text = try {
            File(filesDir, DEBUG_FILE).takeIf { it.isFile }?.readText()?.trim()
        } catch (_: java.io.IOException) {
            null
        } ?: return null
        val parts = text.split(' ', limit = 2)
        return parts[0] to parts.getOrNull(1)?.trim()?.ifEmpty { null }
    }

    /**
     * One row: its driver, in use ([selected]), not greyed ([enabled]), its line, and whether it
     * takes the focus ([focusable]: a grey row does while rows take a pick, as the kit's other grey
     * rows; none does while they take none, as Loot filters). [tag]: "In use" beside its name
     * (decision 33: the driver in use, but not the Phone driver while its ways show; then the
     * picked way has it).
     */
    data class Row(
        val driver: PlayerDriver,
        val selected: Boolean,
        val enabled: Boolean,
        @param:StringRes val body: Int,
        val focusable: Boolean,
        val tag: Boolean = false,
    )

    /**
     * The three rows, the Phone driver's [ways] while it is in use and rows take a pick, with the
     * one in use ([way], which has the "In use" tag), and a [note] under the chip line (a chip with
     * no usable GPU driver). [better]: the way that "Usually runs better on this graphics chip."
     * (decision 33: the first of the chip's measured order; it stays whatever the pick).
     */
    data class Page(
        val rows: List<Row>,
        val ways: List<PhoneWay>,
        val way: PhoneWay?,
        @param:StringRes val note: Int? = null,
        /** Way rows that show grey under [ways], with "Does not run on this graphics chip." ([InUse.greyWays]). */
        val greyWays: List<PhoneWay> = emptyList(),
        /** Test 23: the rows are the App driver's Approach 1 and 2 ([InUse.approaches]). */
        val approaches: Boolean = false,
    ) {
        val better: PhoneWay? get() = ways.firstOrNull()
    }

    /** The driver that today's route is: the App driver where the chip has one, the Phone driver where only it runs, none on [DriverChip.NONE]. */
    private fun today(chip: DriverChip): PlayerDriver? = when (chip) {
        DriverChip.APP_DRIVER -> PlayerDriver.APP
        DriverChip.NO_APP_DRIVER, DriverChip.POWERVR -> PlayerDriver.PHONE
        DriverChip.NONE -> null
    }

    /**
     * What the page shows of the route that the next Play takes ([onRoute]): the chip, the driver
     * in use (null: none, with no route) and its way, and the Phone driver's ways that can run here
     * in the app's order ([DriverOptions.routes]: the measured order of the chip).
     */
    data class InUse(
        val chip: DriverChip,
        val driver: PlayerDriver?,
        val way: PhoneWay?,
        val ways: List<PhoneWay> = emptyList(),
        /** The Phone driver can run here, or its drivers were not read yet (decision 40: else its row is grey, with a reason). */
        val phoneRuns: Boolean = true,
        /** The engine can run the Software driver ([softwareRuns]; review MED-3: else its row is grey, with a reason). */
        val softwareRuns: Boolean = true,
        /**
         * The Phone driver's ways that the chip has but that one rule of their own keeps from
         * running ([greyWays]: VirGL without OpenGL 3.3, Venus without Zink's base requirements):
         * grey rows under the ways that run.
         */
        val greyWays: List<PhoneWay> = emptyList(),
        /** Test 23: the App driver's two approaches on a CSF Mali ([Test23]). */
        val approaches: Boolean = false,
    ) {
        /** The same with the rows of [grey] shown as they are where their driver cannot run ([debugGrey]). */
        fun greyed(grey: Set<PlayerDriver>): InUse =
            copy(phoneRuns = phoneRuns && PlayerDriver.PHONE !in grey, softwareRuns = softwareRuns && PlayerDriver.SOFTWARE !in grey)
    }

    /** Debug builds only: `files/debug/driver-page-grey` with the words `phone` and/or `software` shows those rows grey ([greyed]). */
    const val DEBUG_GREY_FILE = "debug/driver-page-grey"

    /** The rows that a debug build's [DEBUG_GREY_FILE] greys; none in other builds or without the file. */
    fun debugGrey(filesDir: File, debug: Boolean = BuildConfig.DEBUG): Set<PlayerDriver> {
        if (!debug) return emptySet()
        val words = try {
            File(filesDir, DEBUG_GREY_FILE).takeIf { it.isFile }?.readText()?.lowercase()?.split(Regex("\\s+")).orEmpty()
        } catch (_: java.io.IOException) {
            emptyList()
        }
        return setOfNotNull(PlayerDriver.PHONE.takeIf { "phone" in words }, PlayerDriver.SOFTWARE.takeIf { "software" in words })
    }

    /**
     * Whether a change of the pick ([before] to [after], each the driver and its way) clears Play's
     * error line of the route that the player leaves (a phone check of 2026-11-04: the old VirGL
     * notice stayed after a pick of the Software driver); [before] is the stored pick at the start.
     */
    fun pickClearsNotice(before: Pair<PlayerDriver?, PhoneWay?>, after: Pair<PlayerDriver?, PhoneWay?>): Boolean = before != after

    /**
     * The page on the 0.4.0 line, from the route that the next Play takes ([route],
     * [DriverChoice.route], the player's pick in it) on the device's routes ([device], before the
     * app left any): the built-in Turnip or PanVK, RADV and mali-panvk are the App driver; VirGL
     * and Venus the Phone driver with that way; libhybris the Phone driver with no way, as on a
     * PowerVR; [GraphicsRoute.SOFTWARE] the Software driver. The chip has the App driver where
     * one of those three can run; without one, libhybris is a PowerVR's; with no route at all,
     * [NONE][DriverChip.NONE] (no driver in use, not the App driver). [phoneRead]: the phone's own
     * drivers were read, so no route of the Phone driver means it cannot run here. [software]: the
     * engine can run the Software driver.
     */
    fun onRoute(
        device: DriverOptions,
        route: GraphicsRoute?,
        phoneRead: Boolean = false,
        software: Boolean = true,
        greyWays: List<PhoneWay> = emptyList(),
    ): InUse {
        val way = when (route) {
            GraphicsRoute.VIRGL -> PhoneWay.VIRGL
            GraphicsRoute.VENUS -> PhoneWay.VENUS
            else -> null
        }
        val ways = device.routes.mapNotNull { r -> PhoneWay.entries.firstOrNull { it.route == r } }
        val chip = when {
            hasApp(device) -> DriverChip.APP_DRIVER
            device.libhybris -> DriverChip.POWERVR
            device.routes.isEmpty() -> DriverChip.NONE
            else -> DriverChip.NO_APP_DRIVER
        }
        val phoneRuns = !phoneRead || device.routes.any { it.player == PlayerDriver.PHONE }
        if (Test23.approaches(device)) {
            // Test 23: Approach 2 is in the Phone driver's row; no Software driver.
            val driver = if (route == GraphicsRoute.MALI_PANVK) PlayerDriver.PHONE else route?.player
            return InUse(chip, driver, null, emptyList(), phoneRuns = true, softwareRuns = false, approaches = true)
        }
        return InUse(chip, route?.player, way, ways, phoneRuns, software, greyWays.filter { it !in ways })
    }

    /**
     * The ways that [onRoute] shows grey: VirGL where the APK has its server ([glServer]) and the
     * phone's OpenGL ES driver ([gles]) meets vrend's needs but cannot give the game OpenGL 3.3
     * ([PhoneGlesDriver.gl33Problem]; the owner, 2026-11-04: "gray out virGL option on phones
     * without opengl 3.3 support"); Venus where the APK has the mode's parts ([venusServer]) and
     * the phone's Vulkan driver ([phone]) meets the mode's needs but not Zink's base requirements
     * ([SystemDriverGpu.zinkBaseProblem]; the owner, 2026-11-04: "Grey Venus by Zink's check").
     */
    fun greyWays(gles: PhoneGlesDriver?, glServer: Boolean, phone: SystemDriverGpu? = null, venusServer: Boolean = false): List<PhoneWay> =
        listOfNotNull(
            PhoneWay.VIRGL.takeIf { glServer && gles != null && gles.gl33Problem() != null && gles.problem() == gles.gl33Problem() },
            PhoneWay.VENUS.takeIf { venusServer && phone != null && phone.zinkBaseProblem() != null && phone.problem() == phone.zinkBaseProblem() },
        )

    /**
     * The page for [chip]. With [onRoute] (the line's page, [DriverPage.onRoute]): its chip, the
     * driver in use and its way, and the ways in the chip's measured order. Without it (a debug
     * build's preview chip): the player's [pick] where its row takes one, else today's route, and
     * the ways in [gpu]'s order (a Mali takes Venus first, the others VirGL first). With
     * [selectable] false every row is grey and takes no focus, and no way rows show. Way rows show
     * only where two ways can run, never on a PowerVR; [way]: the picked one.
     */
    fun page(
        chip: DriverChip,
        pick: PlayerDriver?,
        way: PhoneWay?,
        gpu: HostGpu?,
        selectable: Boolean = SELECTABLE,
        onRoute: InUse? = null,
    ): Page {
        val route = onRoute
        val shown = route?.chip ?: chip
        val appExists = shown == DriverChip.APP_DRIVER
        val phoneExists = shown != DriverChip.NONE && route?.phoneRuns != false
        val softwareExists = route?.softwareRuns != false
        fun exists(driver: PlayerDriver) = when (driver) {
            PlayerDriver.APP -> appExists
            PlayerDriver.PHONE -> phoneExists
            PlayerDriver.SOFTWARE -> softwareExists
        }
        val inUse = if (route != null) route.driver else pick?.takeIf { selectable && exists(it) } ?: today(shown)
        val order = route?.ways ?: if (gpu is MaliGpu) listOf(PhoneWay.VENUS, PhoneWay.VIRGL) else listOf(PhoneWay.VIRGL, PhoneWay.VENUS)
        // A way that its own rule keeps from running shows grey under the ones that run (route?.greyWays).
        val grey = route?.greyWays.orEmpty().filter { it !in order }
        val waysShow = selectable && inUse == PlayerDriver.PHONE && shown != DriverChip.POWERVR && order.isNotEmpty() && order.size + grey.size >= 2
        val ways = if (waysShow) order else emptyList()
        fun tag(driver: PlayerDriver) = inUse == driver && !(driver == PlayerDriver.PHONE && ways.isNotEmpty())
        val appBody = if (appExists) R.string.driver_app_body else R.string.driver_app_none
        val rows = listOf(
            Row(PlayerDriver.APP, inUse == PlayerDriver.APP, selectable && appExists, appBody, selectable, tag(PlayerDriver.APP)),
            Row(
                PlayerDriver.PHONE, inUse == PlayerDriver.PHONE, selectable && phoneExists,
                when {
                    route?.approaches == true -> R.string.driver_app_body
                    shown != DriverChip.NONE && !phoneExists -> R.string.driver_phone_none
                    else -> R.string.driver_phone_body
                },
                selectable, tag(PlayerDriver.PHONE),
            ),
            Row(
                PlayerDriver.SOFTWARE, inUse == PlayerDriver.SOFTWARE, selectable && softwareExists,
                when {
                    route?.approaches == true -> R.string.driver_off_in_test_23
                    softwareExists -> R.string.driver_software_body
                    else -> R.string.driver_software_needs_update
                },
                selectable, tag(PlayerDriver.SOFTWARE),
            ),
        )
        return Page(
            rows, ways, if (ways.isEmpty()) route?.way?.takeIf { inUse == PlayerDriver.PHONE } else (route?.way ?: way)?.takeIf { it in ways } ?: ways.first(),
            R.string.driver_no_gpu_driver.takeIf { shown == DriverChip.NONE },
            if (waysShow) grey else emptyList(),
            approaches = route?.approaches == true,
        )
    }

    /**
     * Display's GPU row: the driver in use and, for the Phone driver with its way rows, its way
     * ("Phone driver · VirGL"; a PowerVR's is "Phone driver"); null: none in use.
     */
    fun value(page: Page): Pair<PlayerDriver, PhoneWay?>? =
        page.rows.firstOrNull { it.selected }?.let { it.driver to page.way.takeIf { _ -> it.driver == PlayerDriver.PHONE } }
}
