package com.pd2launcher.app

import android.util.Log

/**
 * The GPU as Arm's kbase driver reports it on its CSF interface: [gpuId], the GPU_ID register
 * (arch major:4 arch minor:4 arch rev:4 product major:4 version major:4 version minor:8 status:4,
 * from the top), and the interface version 1.[uapiMinor] the driver agreed to.
 */
data class MaliGpu(val gpuId: Long, val uapiMinor: Int) : HostGpu {
    /** The architecture: 10 for the Mali-G610 and G310, 11 for the G715 and G615, 12, 13. */
    val archMajor: Int get() = ((gpuId shr 28) and 0xF).toInt()

    /** Mesa's product id (PAN_PROD_ID: arch major, arch minor, product major), as PanVK matches it. */
    val productId: Int get() = (archMajor shl 16) or (((gpuId shr 24) and 0xF).toInt() shl 8) or ((gpuId shr 16) and 0xF).toInt()

    override val name: String get() = panvkModels[productId] ?: "Mali v%d (GPU id 0x%08x)".format(archMajor, gpuId)
}

/**
 * Reads the Mali GPU id from `/dev/mali0` (libmaliprobe.so, in the APK's native-library folder,
 * so it runs before the App userland is unpacked), with the CSF handshake our PanVK backend uses.
 */
object MaliProbe {
    private const val TAG = "pd2-mali"

    init {
        System.loadLibrary("maliprobe")
    }

    /** {errno (0 on success), gpu_id, uAPI major, uAPI minor}. */
    @JvmStatic
    private external fun nativeProbe(): LongArray

    /** The GPU, or null when `/dev/mali0` is absent, does not answer or is not a CSF kbase. */
    fun probe(): MaliGpu? {
        val (error, gpuId, major, minor) = nativeProbe()
        if (error != 0L) {
            Log.i(TAG, "/dev/mali0: no CSF kbase GPU (errno $error, uAPI $major.$minor)")
            return null
        }
        return MaliGpu(gpuId, minor.toInt())
    }
}

/** The CSF Malis the device check takes: v10 to v13, the architectures our kbase backend is for. */
val MALI_ARCHES = 10..13

/** The oldest kbase CSF interface our backend runs on (1.9: MEM_ALLOC_EX; kbase_kmod.c). */
const val MALI_MIN_UAPI_MINOR = 9

/**
 * The CSF Malis (v10 to v13) the App userland's PanVK knows, by Mesa's product id, as the App
 * userland's Mesa (config.sh MESA_COMMIT) src/panfrost/model/pan_model.c (pan_model_list) lists
 * them; PanVK refuses any other GPU (pan_get_model). The G310's five variants share one product
 * id, and so do the Immortalis entries (G715, G720, G925) Mesa added in 7c648edb with their
 * non-Immortalis models.
 */
private val panvkModels = mapOf(
    0x0A0807 to "Mali-G610",
    0x0A0C04 to "Mali-G310",
    0x0B0802 to "Mali-G715",
    0x0B0803 to "Mali-G615",
    0x0C0800 to "Mali-G720",
    0x0D0800 to "Mali-G725",
)

/** Whether the App userland's PanVK drives this Mali ([panvkModels]). */
fun panvkKnows(gpu: MaliGpu): Boolean = gpu.productId in panvkModels
