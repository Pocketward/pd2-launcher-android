package com.pd2launcher.app

import android.util.Log

/** The GPU as KGSL reports it: [chipId] and its Adreno [generation] (6, 7, 8; other numbers are unsupported). */
data class KgslGpu(val chipId: Long, val generation: Int) : HostGpu {
    override val name: String get() = adrenoName(chipId, generation)
}

/**
 * Reads the Adreno chip id from `/dev/kgsl-3d0` (libkgslprobe.so, in the APK's native-library
 * folder, so it runs before the App userland is unpacked).
 */
object KgslProbe {
    private const val TAG = "pd2-kgsl"

    init {
        System.loadLibrary("kgslprobe")
    }

    /** {errno (0 on success), chip_id, generation}. */
    @JvmStatic
    private external fun nativeProbe(): LongArray

    /** The GPU, or null when `/dev/kgsl-3d0` is absent or does not answer. */
    fun probe(): KgslGpu? {
        val (error, chipId, generation) = nativeProbe()
        if (error != 0L) {
            Log.i(TAG, "/dev/kgsl-3d0 unavailable: errno $error")
            return null
        }
        return KgslGpu(chipId, generation.toInt())
    }
}

// KGSL chip ids whose marketing name is not the core/major/minor digits, from the App userland's
// Mesa (config.sh MESA_COMMIT) src/freedreno/common/freedreno_devices.py. 0x07002000 is the
// Adreno 702's id as Mesa listed it before 3e15a53a ("Fix a702 chip-id": 0x07000200, which reads
// 702 by its digits); kept so that the name and the tier stay for a device that reports it.
private val namedChips = mapOf(
    0x06060201L to "Adreno 644",
    0x07002000L to "Adreno 702",
    0x07030002L to "Adreno 725",
    0x43020000L to "Adreno 720",
    0x43020100L to "Adreno 722",
    0x43030B00L to "Adreno 735",
    0x43030C00L to "Adreno X1-45",
    0x43030E01L to "Adreno 735",
    0x43050A00L to "Adreno A32",
    0x43050A01L to "Adreno 740",
    0x43050B00L to "Adreno 740v3",
    0x43050C01L to "Adreno X1-85",
    0x43051401L to "Adreno 750",
    0x43051701L to "Adreno 753",
    0x44010000L to "Adreno 810",
    0x44030A20L to "Adreno 829",
    0x44050000L to "Adreno 830",
    0x44050001L to "Adreno 830",
    0x44050A21L to "Adreno 840",
    0x44050A31L to "Adreno 840",
    0x44060030L to "Adreno X2-85",
    0x44070031L to "Adreno X2-85",
    0x44070041L to "Adreno X2-90",
)

/**
 * The A6xx and later GPUs the App userland's Turnip (Mesa main at config.sh's MESA_COMMIT) knows
 * on KGSL, as its src/freedreno/common/freedreno_devices.py lists them: Turnip matches a KGSL GPU
 * by its "gpu id" (core * 100 + major * 10 + minor, src/freedreno/vulkan/tu_knl_kgsl.cc) against
 * the entries given as a number, else by chip id (freedreno_dev_info.c, dev_id_compare; the
 * entries' fuse id is a wildcard, KGSL reports none).
 */
private val turnipGpuIds = setOf(605, 608, 610, 612, 613, 615, 616, 618, 619, 620, 630, 640, 650, 660, 680, 690, 702, 740)
private val turnipChipIds = setOf(
    0x06020100L, 0x06020300L, 0x06030500L, 0x06060201L, 0x06060300L, 0x06090000L,
    0x07000200L, 0x07000400L, 0x07010000L, 0x07030001L, 0x07030002L,
    0x43020000L, 0x43020100L, 0x43030B00L, 0x43030C00L, 0x43030E01L, 0x43050A00L, 0x43050A01L,
    0x43050B00L, 0x43050C01L, 0x43051401L, 0x43051701L,
    0x44010000L, 0x44030A20L, 0x44050000L, 0x44050001L, 0x44050A21L, 0x44050A31L, 0x44060030L,
    0x44070031L, 0x44070041L,
)

/** Whether the App userland's Turnip drives this KGSL GPU ([turnipGpuIds], [turnipChipIds]). */
fun turnipKnows(chipId: Long): Boolean {
    val gpuId = ((chipId shr 24) and 0xFF) * 100 + ((chipId shr 16) and 0xFF) * 10 + ((chipId shr 8) and 0xFF)
    return gpuId.toInt() in turnipGpuIds || chipId in turnipChipIds
}

// Automatic's rule ([lowTierModel]): the first tier (the model number's last two digits) of the
// 6xx and the 7xx that gets Default, and the first generation that gets it throughout.

/**
 * The 6xx from the 660 (the Snapdragon 888's) up get Default; up to the 650 (the Snapdragon 865's)
 * Performance.
 */
const val ADRENO_6XX_DEFAULT_TIER = 60

/** The 7xx from the 710 up get Default; the 702 and 704 are the generation's entry level. */
const val ADRENO_7XX_DEFAULT_TIER = 10

/** This generation and every later one get Default throughout (the 8xx from the 810). */
const val ADRENO_DEFAULT_GENERATION = 8

/**
 * Whether Automatic gives the Adreno [model] (610, 740) the lighter choices: the Performance
 * graphics profile ([GraphicsProfiles.automatic]).
 * The model's first digit is its generation, its last two the tier within it. The generations
 * before the 6xx get them (the bundled Turnip drives none of them, so the device check refuses
 * them before any Play); in the 6xx the tiers below [ADRENO_6XX_DEFAULT_TIER], the entry, mid
 * and high range up to the 650, and in the 7xx those below [ADRENO_7XX_DEFAULT_TIER], the entry
 * level; [ADRENO_DEFAULT_GENERATION] and later never.
 *
 * | Model | Generation | Tier | Automatic |
 * |-------|------------|------|-----------|
 * | 605   | 6          | 05   | Performance |
 * | 610   | 6          | 10   | Performance |
 * | 612   | 6          | 12   | Performance |
 * | 618   | 6          | 18   | Performance |
 * | 619   | 6          | 19   | Performance |
 * | 620   | 6          | 20   | Performance |
 * | 630   | 6          | 30   | Performance |
 * | 640   | 6          | 40   | Performance |
 * | 642L  | 6          | 35   | Performance (KGSL chip id 0x06030500, whose digits read 635) |
 * | 644   | 6          | 44   | Performance (KGSL chip id 0x06060201, whose digits read 662) |
 * | 650   | 6          | 50   | Performance |
 * | 660   | 6          | 60   | Default |
 * | 702   | 7          | 02   | Performance |
 * | 710   | 7          | 10   | Default |
 * | 720   | 7          | 20   | Default |
 * | 730   | 7          | 30   | Default |
 * | 740   | 7          | 40   | Default |
 * | 750   | 7          | 50   | Default |
 * | 830   | 8          | 30   | Default |
 */
fun lowTierModel(model: Int): Boolean {
    val generation = model / 100
    val tier = model % 100
    return when {
        generation >= ADRENO_DEFAULT_GENERATION -> false
        generation == 7 -> tier < ADRENO_7XX_DEFAULT_TIER
        generation == 6 -> tier < ADRENO_6XX_DEFAULT_TIER
        else -> true
    }
}

/**
 * [lowTierModel] for a KGSL chip id. An unknown or unreadable GPU (no chip id, or one with no
 * model number, [adrenoModel]) gets Default: the full choices, which the player can lower.
 */
fun lowTierAdreno(chipId: Long?): Boolean = chipId?.let(::adrenoModel)?.let(::lowTierModel) ?: false

/**
 * The Adreno model number for a KGSL chip id (core:8 major:8 minor:8 patch:8): the number in
 * [namedChips]' name (0x43050A01: 740), else the core, major and minor digits (0x06010000: 610);
 * null when it is neither, or named without one (the X1-45, the A32).
 */
fun adrenoModel(chipId: Long): Int? {
    namedChips[chipId]?.let { return it.removePrefix("Adreno ").take(3).toIntOrNull() }
    val core = (chipId shr 24) and 0xFF
    val major = (chipId shr 16) and 0xFF
    val minor = (chipId shr 8) and 0xFF
    // Up to the A730 the digits are the name (0x06030001 is the A630).
    return if (core in 1..9 && major <= 9 && minor <= 9) (core * 100 + major * 10 + minor).toInt() else null
}

/**
 * The Adrenos that Turnip drives without hardware multiview (Mesa's freedreno_devices.py: the 6xx
 * "gen1 low" parts and the 702). Mesa 26.2's Turnip reported Vulkan 1.0 for them, and Zink (the
 * game's OpenGL) refuses a 1.0 device, so OpenGL fell back to software on the Adreno 610 (a black
 * picture); TU_DEBUG's `noconform` made Turnip report its Vulkan 1.3 there too. Mesa main (the
 * userland's pin, config.sh MESA_COMMIT) has software multiview on these GPUs and reports Vulkan 1.3
 * on every Adreno 6xx; it no longer has the `noconform` option and ignores it. So with main this
 * work-around does nothing; it stays until the pin is known not to go back to a 26.2 release.
 */
private val adrenosWithoutMultiview = setOf(605, 608, 610, 612, 613, 702)

/** Whether the game's Turnip gets `noconform` on this KGSL GPU ([adrenosWithoutMultiview]; needed by Mesa 26.2, main ignores it). */
fun adrenoWithoutMultiview(chipId: Long?): Boolean = chipId?.let(::adrenoModel) in adrenosWithoutMultiview

/** A readable GPU name for a KGSL chip id ([adrenoModel]). */
fun adrenoName(chipId: Long, generation: Int): String =
    namedChips[chipId] ?: adrenoModel(chipId)?.let { "Adreno $it" } ?: "Adreno ${generation}xx (chip id 0x%08x)".format(chipId)
