package com.pd2launcher.app

/**
 * The test build for issue 23 (branch test/23-csf-approaches; never merged): on a Mali of Arm's
 * CSF kbase (G610, G615, G710, G715, G720) the player picks between two App drivers and nothing
 * else, and sends logs from each. Approach 1 is our PanVK on kbase ([GraphicsRoute.BUILT_IN],
 * with the fix that each logical device opens its own kbase context); Approach 2 is mali-panvk
 * ([GraphicsRoute.MALI_PANVK]), whose tree also builds the CSF architectures. The Phone and
 * Software drivers are off on such a chip in this build. The GPU page shows Approach 2 in the
 * Phone driver's row and pick, so the stored settings stay those of 0.4.0-beta1.
 */
object Test23 {
    /** Whether [gpu] is a Mali of the CSF kbase: the chips that this test build changes. */
    fun on(gpu: HostGpu?): Boolean = gpu is MaliGpu && !gpu.jobManager

    /** The routes on such a chip: Approach 2 where the APK has mali-panvk ([maliPanvkFiles]), the default as in main (ADR 0031), then Approach 1. */
    fun options(options: DriverOptions, maliPanvkFiles: Boolean): DriverOptions = options.copy(
        venus = false, virgl = false, virglFirst = false, qualcomm = false, mali = false,
        libhybris = false, radv = false, maliPanvk = maliPanvkFiles, maliPanvkFirst = true,
    )

    /** Whether [options] are this test's: both approaches can run. */
    fun approaches(options: DriverOptions): Boolean =
        GraphicsRoute.BUILT_IN in options.routes && GraphicsRoute.MALI_PANVK in options.routes
}
