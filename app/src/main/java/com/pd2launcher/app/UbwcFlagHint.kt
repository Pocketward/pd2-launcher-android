package com.pd2launcher.app

import android.os.Build
import android.util.Log
import java.io.File
import java.io.IOException

// Turnip's TP UBWC flag hint on an Adreno 740 (issue #9 of the Android app: the screen flickers
// while the game runs). All drivers on the GPU write one register, TPL1_DBG_ECO_CNTL1, with its
// TP_UBWC_FLAG_HINT bit: the game's Turnip in its command streams (Mesa
// src/freedreno/vulkan/tu_cmd_buffer.cc, from the device info's enable_tp_ubwc_flag_hint) and the
// phone's own Qualcomm driver for all of Android's drawing (SurfaceFlinger, SystemUI, the app's
// own views). When the two drivers do not agree, Android's drawing shows corruption and flickers.
// The userland's Mesa (config.sh's MESA_COMMIT, src/freedreno/common/freedreno_devices.py,
// a7xx_gen2) keeps the bit off on the Adreno 740. The phone's side differs from phone to phone
// (measured: the 6xx drivers do not all act alike, and Samsung's firmware sets the bit with
// whatever driver number the phone reports). Thus on an Adreno 740 the app sets the bit with
// the rule of [ubwcFlagHint], with Mesa's FD_DEV_FEATURES (src/freedreno/common/freedreno_dev_info.py,
// fd_dev_info_apply_dbg_options), for the game's Wine processes with the standard and the
// Compatibility driver ([SessionPaths.engineEnvironment]; the engine keeps the value for both).

/** The Adreno model that gets the hint ([adrenoModel]). */
const val UBWC_FLAG_HINT_MODEL = 740

/** The first Qualcomm driver number that sets the hint on an Adreno 740 (the 7xx drivers). */
const val UBWC_FLAG_HINT_DRIVER = 700

/** The Qualcomm driver number whose builds from [UBWC_FLAG_HINT_PATCH] set the hint, too. */
const val UBWC_FLAG_HINT_DRIVER_676 = 676

/** The first patch number of driver [UBWC_FLAG_HINT_DRIVER_676] that sets the hint. */
const val UBWC_FLAG_HINT_PATCH = 76

/** Mesa freedreno's device features, which Turnip reads (`<name>=<value>`, joined by `:`). */
const val FD_DEV_FEATURES = "FD_DEV_FEATURES"

/**
 * The [FD_DEV_FEATURES] value for the [hint] (0 or 1). Mesa stops the program for a feature name
 * that it does not know, so the name must be exact.
 */
fun ubwcFlagHintFeature(hint: Int): String = "enable_tp_ubwc_flag_hint=$hint"

/**
 * Qualcomm's driver number in a Vulkan driverVersion: its VK_API_VERSION_MINOR, the 10 bits from
 * bit 12. Qualcomm's form is 512.<driver>.<patch>: 512.676.53 for the driver "V@0676.53".
 */
fun qualcommDriverNumber(driverVersion: Long): Int = ((driverVersion shr 12) and 0x3FF).toInt()

/** Qualcomm's patch number in a Vulkan driverVersion: its VK_API_VERSION_PATCH, the low 12 bits. */
fun qualcommDriverPatch(driverVersion: Long): Int = (driverVersion and 0xFFF).toInt()

/** True when the device's maker ([android.os.Build.MANUFACTURER]) is Samsung, in any case. */
fun isSamsung(maker: String?): Boolean = maker.equals("samsung", ignoreCase = true)

/**
 * The hint for the Adreno [model] with the phone's Vulkan [driverVersion], on an Adreno 740 only:
 * 1 for a [samsung] phone (its firmware sets the bit with any driver number, also when the version
 * is unknown); else 1 for a Qualcomm driver from [UBWC_FLAG_HINT_DRIVER], or for driver
 * [UBWC_FLAG_HINT_DRIVER_676] from patch [UBWC_FLAG_HINT_PATCH]; else 0. Null (Mesa's own value)
 * for a different GPU, or for a phone that is not Samsung when the version is unknown.
 */
fun ubwcFlagHint(model: Int?, driverVersion: Long?, samsung: Boolean = false): Int? = when {
    model != UBWC_FLAG_HINT_MODEL -> null
    samsung -> 1
    driverVersion == null -> null
    qualcommDriverNumber(driverVersion) >= UBWC_FLAG_HINT_DRIVER -> 1
    qualcommDriverNumber(driverVersion) == UBWC_FLAG_HINT_DRIVER_676 &&
        qualcommDriverPatch(driverVersion) >= UBWC_FLAG_HINT_PATCH -> 1
    else -> 0
}

/**
 * The app's choice for the GPU [gpu] (its name, null when no GPU answers) and its Adreno [model]:
 * the phone's Vulkan [driverVersion] (read on an Adreno 740 only), whether the phone is [samsung]
 * and the [hint] (null: Mesa's own value).
 */
data class UbwcChoice(
    val gpu: String?,
    val model: Int?,
    val driverVersion: Long?,
    val hint: Int?,
    val samsung: Boolean = false,
    val driverInfo: String? = null,
) {
    /** For the service log and device.txt. */
    fun describe(): String {
        val name = gpu ?: return "no GPU found: Mesa's own value"
        if (model != UBWC_FLAG_HINT_MODEL) return "$name: Mesa's own value (the app sets it on an Adreno 740 only)"
        val value = hint?.let { "$FD_DEV_FEATURES=${ubwcFlagHintFeature(it)}" } ?: "Mesa's own value"
        val maker = if (samsung) ", Samsung" else ""
        val version = driverVersion ?: return "$name$maker, Vulkan driver version unknown: $value"
        val build = driverInfo?.let(::driverBuild)?.let { ", build $it" } ?: ""
        val vulkan = "${version shr 22}.${qualcommDriverNumber(version)}.${qualcommDriverPatch(version)}$build"
        return "$name$maker, Qualcomm driver ${qualcommDriverNumber(version)} (Vulkan driver version $vulkan): $value"
    }

    /** [describe] on [route]: only the built-in Turnip takes the value ([SessionPaths.engineEnvironment]). */
    fun describeOn(route: GraphicsRoute?): String =
        describe() + if (hint != null && route != null && route != GraphicsRoute.BUILT_IN) " (not used on route $route)" else ""
}

/**
 * The [UbwcChoice] for a GPU and the device's [maker]. The phone's Vulkan driver is read once for
 * each app process, and on an Adreno 740 only ([read], which is [VulkanDriverProbe.read] in the
 * app): the probe loads the phone's driver into the app's process.
 */
object UbwcFlagHint {
    fun choice(gpu: HostGpu?, maker: String? = Build.MANUFACTURER, read: () -> VulkanDriverReading = { VulkanDriverReading(null, null) }): UbwcChoice {
        val model = (gpu as? KgslGpu)?.let { adrenoModel(it.chipId) }
        if (model != UBWC_FLAG_HINT_MODEL) return UbwcChoice(gpu?.name, model, null, null)
        val driver = read().driver
        val samsung = isSamsung(maker)
        return UbwcChoice(
            gpu.name, model, driver?.driverVersion, ubwcFlagHint(model, driver?.driverVersion, samsung), samsung, driver?.driverInfo,
        )
    }
}

/** The facts of the first Vulkan device and its driver. The last three are null when the device has no driver properties. */
data class VulkanDriver(
    val device: String,
    val apiVersion: Long,
    val driverVersion: Long,
    val driverName: String?,
    val driverInfo: String?,
    val conformance: String?,
) {
    /** For device.txt: the Vulkan version, the driver version (major.minor.patch) and what the driver says of itself. */
    fun describe(): String = buildString {
        append("$device, Vulkan ${versionText(apiVersion)}, driver version ${versionText(driverVersion)}")
        driverName?.let { append(", driverName \"$it\"") }
        driverInfo?.let { append(", driverInfo \"$it\"") }
        conformance?.let { append(", conformance $it") }
    }

    companion object {
        /** The probe's "key=value" lines ([VulkanDriverProbe.nativeDriverFacts]); null when a needed key is missing. */
        fun parse(text: String): VulkanDriver? {
            val values = text.lines().filter { '=' in it }.associate { it.substringBefore('=') to it.substringAfter('=') }
            return VulkanDriver(
                device = values["device"] ?: return null,
                apiVersion = values["api"]?.toLongOrNull() ?: return null,
                driverVersion = values["driver"]?.toLongOrNull() ?: return null,
                driverName = values["name"]?.takeIf { it.isNotEmpty() },
                driverInfo = values["info"]?.takeIf { it.isNotEmpty() },
                conformance = values["conf"]?.takeIf { it.isNotEmpty() },
            )
        }
    }
}

/** A Vulkan version number as major.minor.patch (the 10, 10 and 12 bits from the top; the variant bits are dropped). */
fun versionText(version: Long): String = "${(version shr 22) and 0x3FF}.${(version shr 12) and 0x3FF}.${version and 0xFFF}"

/**
 * Qualcomm's build in a driverInfo text, "hash, date", for example "dc3d4da3a2, 05/30/25" from
 * "Driver Build: dc3d4da3a2, Idd45c2c082, 1748543865, Date: 05/30/25"; null when the text has no
 * such form. The date is its digits and slashes only, because the driver's next fact can follow it
 * with no space between ("Date: 12/27/23; Compiler Version: ...").
 */
fun driverBuild(driverInfo: String): String? {
    val hash = Regex("Driver Build: ([0-9A-Za-z]+)").find(driverInfo)?.groupValues?.get(1) ?: return null
    val date = Regex("Date: ([0-9]{1,2}/[0-9]{1,2}/[0-9]{2,4})").find(driverInfo)?.groupValues?.get(1) ?: return null
    return "$hash, $date"
}

/** What a probe gave: the [driver] (null: none answered) and a [note] (null: the probe ran) that says why it did not. */
data class VulkanDriverReading(val driver: VulkanDriver?, val note: String?) {
    /** For device.txt. */
    fun describe(): String = note ?: driver?.describe() ?: "no Vulkan device answers"
}

/**
 * Reads the phone's own Vulkan driver (libvulkandriverprobe.so, `vulkan_driver_probe.c`). A driver
 * that crashes in the probe would crash the app, and with it Share logs, so a marker file guards
 * the call: it is there before the probe and gone after it. A marker that is there at the start
 * (the last probe did not finish) skips the probe.
 */
object VulkanDriverProbe {
    private const val TAG = "pd2-vkdriver"
    const val SKIPPED_UNFINISHED = "driver probe skipped: the last probe did not finish"
    const val SKIPPED_NO_MARKER = "driver probe skipped: the marker file could not be written"

    private var reading: VulkanDriverReading? = null

    /** The reading, made once for each app process, with the [marker] file as the guard. */
    @Synchronized
    fun read(marker: File): VulkanDriverReading = reading ?: probe(marker).also { reading = it }

    private fun probe(marker: File): VulkanDriverReading {
        if (marker.exists()) return VulkanDriverReading(null, SKIPPED_UNFINISHED)
        try {
            marker.parentFile?.mkdirs()
            marker.writeText("probe started\n")
        } catch (error: IOException) {
            Log.w(TAG, "the marker file was not written: $error")
            return VulkanDriverReading(null, SKIPPED_NO_MARKER)
        }
        val driver = driverFacts()
        marker.delete()
        return VulkanDriverReading(driver, null)
    }

    /** The first Vulkan device's facts; null when no Vulkan device answers. */
    private fun driverFacts(): VulkanDriver? = try {
        System.loadLibrary("vulkandriverprobe")
        nativeDriverFacts()?.let(VulkanDriver::parse).also { if (it == null) Log.i(TAG, "no Vulkan device answers") }
    } catch (error: UnsatisfiedLinkError) {
        Log.w(TAG, "the Vulkan driver probe did not load: $error")
        null
    }

    /** The first Vulkan device's facts as "key=value" lines, or null. */
    @JvmStatic
    private external fun nativeDriverFacts(): String?
}
