package com.pd2launcher.app

import android.util.Log

/**
 * The GPU as Arm's kbase driver reports it: [gpuId], the GPU_ID register (arch major:4 arch
 * minor:4 arch rev:4 product major:4 version major:4 version minor:8 status:4, from the top), and
 * the interface version the driver agreed to: 1.[uapiMinor] on a CSF kbase (v10 and later), or
 * 11.[uapiMinor] on a job-manager one ([jobManager]: Bifrost v6/v7, Valhall v9, whose GPU id the
 * probe makes from its product id the way mali-panvk does, so it reads the same).
 */
data class MaliGpu(val gpuId: Long, val uapiMinor: Int, val jobManager: Boolean = false) : HostGpu {
    /** The architecture: 10 for the Mali-G610 and G310, 11 for the G715 and G615, 12, 13; on the job manager 6, 7 (Bifrost) or 9 (Valhall). */
    val archMajor: Int get() = ((gpuId shr 28) and 0xF).toInt()

    /** Mesa's product id (PAN_PROD_ID: arch major, arch minor, product major), as PanVK matches it. */
    val productId: Int get() = (archMajor shl 16) or (((gpuId shr 24) and 0xF).toInt() shl 8) or ((gpuId shr 16) and 0xF).toInt()

    override val name: String
        get() = (if (jobManager) maliPanvkModel(this) else panvkModels[productId]) ?: "Mali v%d (GPU id 0x%08x)".format(archMajor, gpuId)

    /**
     * Arm's kernel interface that the driver agreed to, for a report: the CSF one or the job
     * manager's, with its version. A fault of one kbase variant (issue 23: CSF uAPI 1.20, where
     * 1.18 worked) shows by this alone.
     */
    val kernelInterface: String
        get() = if (jobManager) "kbase job manager, uAPI 11.$uapiMinor" else "kbase CSF, uAPI 1.$uapiMinor"
}

/**
 * Reads the Mali GPU id from `/dev/mali0` (libmaliprobe.so, in the APK's native-library folder,
 * so it runs before the App userland is unpacked), with the CSF handshake our PanVK backend uses,
 * and after a refusal the job manager's, as mali-panvk makes it ([maliGpuOf]).
 */
object MaliProbe {
    private const val TAG = "pd2-mali"

    init {
        System.loadLibrary("maliprobe")
    }

    /** {errno (0 on success), gpu_id, uAPI major, uAPI minor}. */
    @JvmStatic
    private external fun nativeProbe(): LongArray

    /**
     * The GPU, or null when `/dev/mali0` is absent, does not answer, is neither a CSF nor a
     * job-manager kbase, or is a job-manager Mali that mali-panvk does not know ([maliGpuOf]).
     */
    fun probe(): MaliGpu? {
        val answer = nativeProbe()
        return maliGpuOf(answer).also { gpu ->
            if (gpu == null) Log.i(TAG, "/dev/mali0: no kbase GPU the app's drivers know (errno ${answer[0]}, uAPI ${answer[2]}.${answer[3]}, GPU id 0x%x)".format(answer[1]))
        }
    }
}

/**
 * The GPU of the probe's [answer] ({errno, gpu_id, uAPI major, uAPI minor}): a CSF one with uAPI
 * major 1; a job-manager one ([MaliGpu.jobManager]) with major 11, only where mali-panvk knows it
 * ([maliPanvkKnows]); null on an error, another major, or another job-manager Mali, which then
 * stays what it was before the mali-panvk route (no kernel GPU: the phone's own driver only).
 * Pure, so the JVM tests check it.
 */
fun maliGpuOf(answer: LongArray): MaliGpu? {
    if (answer.size != 4 || answer[0] != 0L) return null
    return when (answer[2]) {
        CSF_UAPI_MAJOR -> MaliGpu(answer[1], answer[3].toInt())
        JM_UAPI_MAJOR -> MaliGpu(answer[1], answer[3].toInt(), jobManager = true).takeIf { maliPanvkKnows(it) }
        else -> null
    }
}

/** kbase's interface versions: CSF (csf/mali_kbase_csf_ioctl.h BASE_UK_VERSION_MAJOR) and the job manager's. */
private const val CSF_UAPI_MAJOR = 1L
private const val JM_UAPI_MAJOR = 11L

/** The CSF Malis the device check takes: v10 to v13, the architectures our kbase backend is for. */
val MALI_ARCHES = 10..13

/**
 * The oldest kbase CSF interface our backend runs on: 1.9, the oldest uAPI its calls are written
 * for (kbase_kmod.c). A kernel without one of the newer calls (MEM_ALLOC_EX, the newer group and
 * tiler heap layouts) is handled by the backend's fallbacks, not by this check.
 */
const val MALI_MIN_UAPI_MINOR = 9

/**
 * The CSF Malis (v10 to v13) the App userland's PanVK knows, by Mesa's product id, as the App
 * userland's Mesa (config.sh MESA_COMMIT) src/panfrost/model/pan_model.c (pan_model_list) lists
 * them, with the Mali-G710 that our patch mesa-panvk/0006 adds; PanVK refuses any other GPU
 * (pan_get_model). The G310's five variants share one product id, and so do the Immortalis
 * entries (G715, G720, G925) Mesa added in 7c648edb with their non-Immortalis models.
 */
private val panvkModels = mapOf(
    0x0A0802 to "Mali-G710",
    0x0A0807 to "Mali-G610",
    0x0A0C04 to "Mali-G310",
    0x0B0802 to "Mali-G715",
    0x0B0803 to "Mali-G615",
    0x0C0800 to "Mali-G720",
    0x0D0800 to "Mali-G725",
)

/** Whether the App userland's PanVK drives this Mali ([panvkModels]): a CSF one only. */
fun panvkKnows(gpu: MaliGpu): Boolean = !gpu.jobManager && gpu.productId in panvkModels

/**
 * The job-manager Malis that mali-panvk knows (the mali-panvk route, [MaliPanvk]), by Mesa's
 * product id, in the order of its src/panfrost/model/pan_model.c (pan_model_list, at config.sh's
 * MALI_PANVK_COMMIT): the Bifrost (v6, v7) and Valhall v9 entries of the architectures it builds
 * PanVK for (src/panfrost/vulkan/meson.build: v6, v7, v9 on the job manager); its Midgard entries
 * have no PanVK. The job manager has no GPU variant (kbase_kmod.c sets 0), as every one of these
 * entries.
 */
private val maliPanvkModels = linkedMapOf(
    0x060000 to "Mali-G71",
    0x060201 to "Mali-G72",
    0x070000 to "Mali-G51",
    0x070003 to "Mali-G31",
    0x070201 to "Mali-G76",
    0x070202 to "Mali-G52",
    0x070402 to "Mali-G52 r1",
    0x090000 to "Mali-G77",
    0x090001 to "Mali-G57",
    0x090003 to "Mali-G57",
    0x090202 to "Mali-G78",
    0x090204 to "Mali-G68",
)

/**
 * The name mali-panvk gives [gpu] (a job-manager Mali), or null where it refuses it: its entry by
 * product id, else on a v9 GPU the first v9 entry with the same product major, as its
 * pan_get_model does ("kbase names a v9 GPU by arch major and product major alone"), for a part
 * whose arch minor the table does not have.
 */
fun maliPanvkModel(gpu: MaliGpu): String? {
    if (!gpu.jobManager) return null
    maliPanvkModels[gpu.productId]?.let { return it }
    if (gpu.archMajor != 9) return null
    return maliPanvkModels.entries.firstOrNull { (id, _) -> id shr 16 == 9 && id and 0xFF == gpu.productId and 0xFF }?.value
}

/** Whether mali-panvk drives this Mali: a job-manager one that it knows ([maliPanvkModel]). */
fun maliPanvkKnows(gpu: MaliGpu): Boolean = maliPanvkModel(gpu) != null
