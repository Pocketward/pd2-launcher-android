package com.pd2launcher.app

import android.util.Log

// Turnip's TP UBWC flag hint on an Adreno 740 (issue #9 of the Android app: the screen flickers
// while the game runs). All drivers on the GPU write one register, TPL1_DBG_ECO_CNTL1, with its
// TP_UBWC_FLAG_HINT bit: the game's Turnip in its command streams (Mesa
// src/freedreno/vulkan/tu_cmd_buffer.cc, from the device info's enable_tp_ubwc_flag_hint) and the
// phone's own Qualcomm driver for all of Android's drawing (SurfaceFlinger, SystemUI, the app's
// own views). When the two drivers do not agree, Android's drawing shows corruption and flickers.
// The userland's Mesa (config.sh's MESA_COMMIT, src/freedreno/common/freedreno_devices.py,
// a7xx_gen2) keeps the bit off on the Adreno 740, as Qualcomm's 6xx drivers do; Qualcomm's drivers
// from 700 set it. Thus on an Adreno 740 the app sets the bit from the version of the phone's
// driver ([ubwcFlagHint]) with Mesa's FD_DEV_FEATURES (src/freedreno/common/freedreno_dev_info.py,
// fd_dev_info_apply_dbg_options), for the game's Wine processes with the standard and the
// Compatibility driver ([SessionPaths.engineEnvironment]; the engine keeps the value for both).

/** The Adreno model that gets the hint ([adrenoModel]). */
const val UBWC_FLAG_HINT_MODEL = 740

/** The first Qualcomm driver number that sets the hint on an Adreno 740 (the 7xx drivers). */
const val UBWC_FLAG_HINT_DRIVER = 700

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

/**
 * The hint for the Adreno [model] with the phone's Vulkan [driverVersion]: on an Adreno 740, 1 for
 * a Qualcomm driver from [UBWC_FLAG_HINT_DRIVER] and 0 for an older one; null (Mesa's own value)
 * for a different GPU, or when the version is unknown.
 */
fun ubwcFlagHint(model: Int?, driverVersion: Long?): Int? = when {
    model != UBWC_FLAG_HINT_MODEL || driverVersion == null -> null
    qualcommDriverNumber(driverVersion) >= UBWC_FLAG_HINT_DRIVER -> 1
    else -> 0
}

/**
 * The app's choice for the GPU [gpu] (its name, null when no GPU answers) and its Adreno [model]:
 * the phone's Vulkan [driverVersion] (read on an Adreno 740 only) and the [hint] (null: Mesa's own
 * value).
 */
data class UbwcChoice(val gpu: String?, val model: Int?, val driverVersion: Long?, val hint: Int?) {
    /** For the service log and device.txt. */
    fun describe(): String {
        val name = gpu ?: return "no GPU found: Mesa's own value"
        if (model != UBWC_FLAG_HINT_MODEL) return "$name: Mesa's own value (the app sets it on an Adreno 740 only)"
        val version = driverVersion ?: return "$name, Vulkan driver version unknown: Mesa's own value"
        val vulkan = "${version shr 22}.${qualcommDriverNumber(version)}.${version and 0xFFF}"
        val value = hint?.let { "$FD_DEV_FEATURES=${ubwcFlagHintFeature(it)}" } ?: "Mesa's own value"
        return "$name, Qualcomm driver ${qualcommDriverNumber(version)} (Vulkan driver version $vulkan): $value"
    }
}

/**
 * The [UbwcChoice] for a GPU. The phone's Vulkan driver is read once for each app process, and on
 * an Adreno 740 only ([VulkanDriverProbe]): the probe loads the phone's driver into the app's
 * process.
 */
object UbwcFlagHint {
    private var read = false
    private var driverVersion: Long? = null

    @Synchronized
    fun choice(gpu: HostGpu?): UbwcChoice {
        val model = (gpu as? KgslGpu)?.let { adrenoModel(it.chipId) }
        if (model != UBWC_FLAG_HINT_MODEL) return UbwcChoice(gpu?.name, model, null, null)
        if (!read) {
            driverVersion = VulkanDriverProbe.driverVersion()
            read = true
        }
        return UbwcChoice(gpu.name, model, driverVersion, ubwcFlagHint(model, driverVersion))
    }
}

/** Reads the version of the phone's own Vulkan driver (libvulkandriverprobe.so, `vulkan_driver_probe.c`). */
object VulkanDriverProbe {
    private const val TAG = "pd2-vkdriver"

    /** The driverVersion of the first Vulkan device; null when no Vulkan device answers. */
    fun driverVersion(): Long? = try {
        System.loadLibrary("vulkandriverprobe")
        nativeDriverVersion().takeIf { it >= 0 }.also { if (it == null) Log.i(TAG, "no Vulkan device answers") }
    } catch (error: UnsatisfiedLinkError) {
        Log.w(TAG, "the Vulkan driver probe did not load: $error")
        null
    }

    /** The driverVersion of the first Vulkan device, or -1. */
    @JvmStatic
    private external fun nativeDriverVersion(): Long
}
