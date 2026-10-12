package com.pd2launcher.app

/**
 * The device's GPU as its kernel driver reports it: an Adreno through KGSL ([KgslGpu]) or a Mali
 * through Arm's kbase ([MaliGpu]). The device check, Automatic's graphics choices and the GL
 * thunk's host values follow it.
 */
sealed interface HostGpu {
    /** A readable name, such as "Adreno 740" or "Mali-G715". */
    val name: String

    companion object {
        /** KGSL's GPU, else kbase's; null when neither answers. */
        fun probe(): HostGpu? = KgslProbe.probe() ?: MaliProbe.probe()
    }
}
