package com.pd2launcher.app

/**
 * The test build for issue 23 (branch test/23-loading; never merged): on a Mali of Arm's CSF kbase
 * (G610, G615, G710, G715, G720) the game runs on mali-panvk ([GraphicsRoute.MALI_PANVK]) and on
 * nothing else, as main makes it the App driver there (ADR 0031). Our PanVK on kbase
 * ([GraphicsRoute.BUILT_IN]) drives such a chip only where the APK has no mali-panvk. The Phone and
 * Software drivers are off on such a chip in this build. The GPU page is that of 0.4.0-beta1.
 */
object Test23 {
    /** Whether [gpu] is a Mali of the CSF kbase: the chips that this test build changes. */
    fun on(gpu: HostGpu?): Boolean = gpu is MaliGpu && !gpu.jobManager

    /** The routes on such a chip: mali-panvk alone where the APK has it ([maliPanvkFiles]), else our PanVK. */
    fun options(options: DriverOptions, maliPanvkFiles: Boolean): DriverOptions = options.copy(
        builtIn = options.builtIn && !maliPanvkFiles,
        venus = false, virgl = false, virglFirst = false, qualcomm = false, mali = false,
        libhybris = false, radv = false, maliPanvk = maliPanvkFiles,
    )
}
