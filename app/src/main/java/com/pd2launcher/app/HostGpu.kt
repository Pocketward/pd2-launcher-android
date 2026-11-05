package com.pd2launcher.app

/**
 * The device's GPU as its kernel driver reports it: an Adreno through KGSL ([KgslGpu]) or a Mali
 * through Arm's kbase ([MaliGpu]); or, for a GPU that the bundled Turnip and PanVK do not drive,
 * the phone's own Vulkan driver ([SystemDriverGpu], the system-driver mode). The device check,
 * Automatic's graphics choices, the game's Vulkan values and which Vulkan driver the userland
 * unpack writes follow it.
 */
sealed interface HostGpu {
    /** A readable name, such as "Adreno 740" or "Mali-G715". */
    val name: String

    companion object {
        /**
         * KGSL's GPU, else kbase's, when the bundled Turnip or PanVK drives it
         * ([DeviceCheck.bundledDriverTakes]); else the phone's own Vulkan driver for the
         * system-driver mode ([PhoneDriver], which loads that driver into the app's process; its
         * report keeps the kernel driver's answer, [SystemDriverGpu.kernel], such as a Mali of
         * kbase's job manager for the mali-panvk route, [MaliPanvk.gpu]); else the kernel driver's
         * answer, or null when nothing answers.
         */
        fun probe(): HostGpu? {
            val kernel = KgslProbe.probe() ?: MaliProbe.probe()
            if (kernel != null && DeviceCheck.bundledDriverTakes(kernel)) return kernel
            return PhoneDriver.report(kernel) ?: kernel
        }

        /**
         * The phone's own Vulkan driver beside [gpu] ([probe]'s answer): [gpu] itself when that is
         * the phone's driver, else its report ([PhoneDriver]) where the built-in driver does not
         * know [gpu] ([DeviceCheck.bundledDriverKnows]). Where it does, the built-in route is
         * taken ([GraphicsRoute]) and the vendor driver is not loaded into the app's process,
         * unless [forced] ([DriverChoice.readsPhoneDriver]): for the player's pick of the Phone driver, or in a debug build
         * (its forced route, [SystemDriver.forced], needs the report for its Zink).
         */
        fun phoneDriver(gpu: HostGpu?, forced: Boolean = false): SystemDriverGpu? = when {
            gpu is SystemDriverGpu -> gpu
            gpu != null && DeviceCheck.bundledDriverKnows(gpu) && !forced -> null
            else -> PhoneDriver.report(gpu)
        }

        /**
         * The phone's own OpenGL ES driver for the VirGL route ([PhoneGles]): only where the
         * built-in driver does not know [gpu] (also where Venus runs: VirGL is the Phone driver's
         * other way), or where [forced], as [phoneDriver]; else null.
         */
        fun phoneGles(gpu: HostGpu?, forced: Boolean = false): PhoneGlesDriver? = when {
            gpu != null && DeviceCheck.bundledDriverKnows(gpu) && !forced -> null
            else -> PhoneGles.report()
        }
    }
}
