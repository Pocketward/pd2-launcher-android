package com.pd2launcher.app

import androidx.annotation.StringRes
import java.io.File

/**
 * The three drivers of Settings → App → Graphics driver (the owner's names): the App driver (the
 * launcher's own, Turnip or PanVK in 0.3.8), the Phone driver (the phone's own) and the Software
 * driver (the game drawn by the processor).
 */
enum class PlayerDriver { APP, PHONE, SOFTWARE }

/** The Phone driver's two ways, in the radio rows under it while it is in use. */
enum class PhoneWay(@param:StringRes val label: Int) {
    VIRGL(R.string.driver_way_virgl),
    VENUS(R.string.driver_way_venus),
}

/**
 * The chip as the page sees it: with an App driver, with one that stopped after an error in this
 * app version ([APP_FAILED]: the app uses the Phone driver until the next app update; in 0.3.8 only
 * a debug preview shows it), without one, a PowerVR (whose Phone driver has one way), or one with
 * no usable GPU driver at all ([NONE]: only the Software driver is left).
 */
enum class DriverChip { APP_DRIVER, APP_FAILED, NO_APP_DRIVER, POWERVR, NONE }

/**
 * Settings → Display → GPU, a placeholder in 0.3.8 (the owner, 2026-11-03: "we need
 * placeholders, much like we deal with the loot filters ... They don't have to function right
 * now ... But we still want the UI and its selectors in place"; decision 23: the GPU row at the top
 * of Display opens this sub page): "Your graphics chip: <name>.", then the App driver, the Phone
 * driver (with its VirGL and Venus rows under it while it is in use) and the Software driver.
 * A row greys out where its driver cannot exist on the chip. In 0.3.8 every row is grey
 * ([SELECTABLE] false) and a stored pick is not used; from the 0.4.0 beta the pick
 * ([AppSettings.driverPagePick], [AppSettings.driverPageWay]) is kept across a restart. No route code
 * reads it: every Play runs on today's route (the App driver). Pure, so the JVM tests check it.
 */
object DriverPage {
    /**
     * Whether the rows take a pick in this build: the one switch between "selectable" and "greyed
     * like Loot filters". False in 0.3.8 (the owner, 2026-11-03: "0.3.8 is disabled for now, it'll be
     * enabled in 0.4.0 beta"): set it to true in the 0.4.0 beta line. Greyed, no row and no way row
     * takes the focus or a touch, so nothing writes a pick. The tests follow it, and one pins the
     * shipped value.
     */
    const val SELECTABLE = false

    /** Debug builds only: `files/debug/driver-page-chip` shows the page for another chip (`appfail`, `no-app-driver`, `powervr`, `none` or `mali`, then its name). */
    const val DEBUG_FILE = "debug/driver-page-chip"

    /**
     * The chip of [gpu]: every GPU that 0.3.8 runs (an Adreno through KGSL, a CSF Mali) has the
     * App driver; no GPU, none that the app can use. [debugChip]: the first word of a debug build's
     * [DEBUG_FILE].
     */
    fun chip(gpu: HostGpu?, debugChip: String?): DriverChip = when (debugChip) {
        "appfail" -> DriverChip.APP_FAILED
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
     * picked way has it). [retry]: the failed App driver's row, with "Try again" in place of a radio.
     */
    data class Row(
        val driver: PlayerDriver,
        val selected: Boolean,
        val enabled: Boolean,
        @param:StringRes val body: Int,
        val focusable: Boolean,
        val tag: Boolean = false,
        val retry: Boolean = false,
    )

    /**
     * The three rows, the Phone driver's [ways] while it is in use and rows take a pick, with the
     * one in use ([way], which has the "In use" tag), and a [note] under the chip line (a chip with
     * no usable GPU driver). [better]: the way that "Usually runs better on this graphics chip."
     * (decision 33: the first of the chip's measured order; it stays whatever the pick).
     */
    data class Page(val rows: List<Row>, val ways: List<PhoneWay>, val way: PhoneWay?, @param:StringRes val note: Int? = null) {
        val better: PhoneWay? get() = ways.firstOrNull()
    }

    /** The driver that today's route is: the App driver where the chip has one, the Phone driver where only it runs, none on [DriverChip.NONE]. */
    private fun today(chip: DriverChip): PlayerDriver? = when (chip) {
        DriverChip.APP_DRIVER -> PlayerDriver.APP
        DriverChip.APP_FAILED, DriverChip.NO_APP_DRIVER, DriverChip.POWERVR -> PlayerDriver.PHONE
        DriverChip.NONE -> null
    }

    /**
     * The page for [chip]: the driver in use is the player's [pick] where its row takes one, else
     * today's route; with [selectable] false every row is grey and takes no focus, no way rows show,
     * and today's route shows. The Phone driver's ways in the chip's order ([gpu]: a Mali takes Venus
     * first, the others VirGL first), none on a PowerVR; [way]: the picked one.
     */
    fun page(chip: DriverChip, pick: PlayerDriver?, way: PhoneWay?, gpu: HostGpu?, selectable: Boolean = SELECTABLE): Page {
        val appExists = chip == DriverChip.APP_DRIVER
        val phoneExists = chip != DriverChip.NONE
        fun exists(driver: PlayerDriver) = when (driver) {
            PlayerDriver.APP -> appExists
            PlayerDriver.PHONE -> phoneExists
            PlayerDriver.SOFTWARE -> true
        }
        val inUse = pick?.takeIf { selectable && exists(it) } ?: today(chip)
        val order = if (gpu is MaliGpu) listOf(PhoneWay.VENUS, PhoneWay.VIRGL) else listOf(PhoneWay.VIRGL, PhoneWay.VENUS)
        val ways = if (selectable && inUse == PlayerDriver.PHONE && chip != DriverChip.POWERVR) order else emptyList()
        fun tag(driver: PlayerDriver) = inUse == driver && !(driver == PlayerDriver.PHONE && ways.isNotEmpty())
        val failed = chip == DriverChip.APP_FAILED
        val appBody = when {
            appExists -> R.string.driver_app_body
            failed -> R.string.driver_app_failed
            else -> R.string.driver_app_none
        }
        val rows = listOf(
            Row(PlayerDriver.APP, inUse == PlayerDriver.APP, selectable && appExists, appBody, selectable, tag(PlayerDriver.APP), retry = failed),
            Row(PlayerDriver.PHONE, inUse == PlayerDriver.PHONE, selectable && phoneExists, R.string.driver_phone_body, selectable, tag(PlayerDriver.PHONE)),
            Row(PlayerDriver.SOFTWARE, inUse == PlayerDriver.SOFTWARE, selectable, R.string.driver_software_body, selectable, tag(PlayerDriver.SOFTWARE)),
        )
        return Page(
            rows, ways, if (ways.isEmpty()) null else way?.takeIf { it in ways } ?: ways.first(),
            R.string.driver_no_gpu_driver.takeIf { chip == DriverChip.NONE },
        )
    }

    /**
     * Display's GPU row: the driver in use and, for the Phone driver with its way rows, its way
     * ("Phone driver · VirGL"; a PowerVR's is "Phone driver"); null: none in use.
     */
    fun value(page: Page): Pair<PlayerDriver, PhoneWay?>? =
        page.rows.firstOrNull { it.selected }?.let { it.driver to page.way.takeIf { _ -> it.driver == PlayerDriver.PHONE } }
}
