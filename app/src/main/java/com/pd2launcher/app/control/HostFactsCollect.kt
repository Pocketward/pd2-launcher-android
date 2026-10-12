package com.pd2launcher.app.control

import android.content.Context
import android.os.Build
import android.system.Os
import android.system.OsConstants
import com.pd2launcher.app.DeviceCheck
import com.pd2launcher.app.HostGpu
import com.pd2launcher.app.KgslGpu
import com.pd2launcher.app.MaliGpu

/** What [collect] reports when neither GPU probe can read the GPU. */
const val GPU_NAME_UNKNOWN: String = "unknown"

/** The `chip_id` [collect] reports when neither GPU probe can read the GPU. */
const val CHIP_ID_UNKNOWN: Long = 0

/**
 * The host facts the app's `hello` carries, measured now.
 *
 * `gpu_name` and `chip_id` come from [HostGpu.probe]: an Adreno's KGSL chip id, or a Mali's kbase
 * GPU id (its 32-bit GPU_ID register), else [GPU_NAME_UNKNOWN] and [CHIP_ID_UNKNOWN]; the engine
 * only logs them.
 */
fun HostFacts.Companion.collect(context: Context): HostFacts {
    val gpu = HostGpu.probe()
    return HostFacts(
        androidRelease = Build.VERSION.RELEASE,
        sdk = Build.VERSION.SDK_INT.toLong(),
        deviceModel = Build.MODEL,
        gpuName = gpu?.name ?: GPU_NAME_UNKNOWN,
        chipId = when (gpu) {
            is KgslGpu -> gpu.chipId
            is MaliGpu -> gpu.gpuId and 0xFFFFFFFFL
            null -> CHIP_ID_UNKNOWN
        },
        kernelRelease = Os.uname().release,
        pageSize = Os.sysconf(OsConstants._SC_PAGESIZE),
        // /data/data/<package> when that is this app's folder.
        dataDir = DeviceCheck.dataDir(context),
    )
}
