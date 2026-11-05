package com.pd2launcher.app

/**
 * A frozen picture ([feed]): the game keeps sending frames (the present counter goes up) but its
 * picture has not changed for [frozenMs]. On the VirGL route a phone driver that draws wrong can
 * do this (Qualcomm's OpenGL ES: the picture stops after the first frames while the frames go on),
 * and the no-frame rule ([PhoneDriverFailure.failed]) does not see it. D2's screens move while it
 * draws (the menu's fire, the town's torches, the pause menu's pentagrams), so a still picture for
 * that long with frames is a driver failure. The VirGL, libhybris, mali-panvk and built-in routes
 * (a fault of a job-manager Mali can show as a still picture too). The picture is the game's
 * buffer on the screen while the app shows it (the direct present, which leaves the X screen
 * still), else the X screen ([GameEnvironment.pictureHash]). Pure, so the JVM tests check it.
 */
class FrozenPicture(
    private val frozenMs: Long = FROZEN_MS,
    private val minPresents: Long = MIN_PRESENTS,
) {
    private var picture: Long? = null
    private var since = 0L
    private var presentsSince: Long? = null
    private var reported = false
    private var frozen: Long? = null

    /**
     * One look at [nowMs]: the present counter ([presents], a 32-bit count; null without it) and
     * a hash of the game's picture ([picture]; null when it cannot be read). True once, at the
     * first look that finds the picture frozen; not again until [moved] or [reset]. A look
     * without a counter or a picture starts the time again: the picture was not seen meanwhile.
     */
    fun feed(nowMs: Long, presents: Long?, picture: Long?): Boolean {
        if (picture == null || presents == null) {
            this.picture = null
            presentsSince = null
            return false
        }
        if (picture != this.picture || presentsSince == null) {
            this.picture = picture
            since = nowMs
            presentsSince = presents
            return false
        }
        val frames = (presents - presentsSince!!) and 0xFFFFFFFFL
        if (reported || nowMs - since < frozenMs || frames < minPresents) return false
        reported = true
        frozen = picture
        return true
    }

    /**
     * Whether [picture] differs from the one [feed] found frozen: the game went on after a still
     * picture, which is no failure. True once; [feed] can then find a frozen picture again. Only
     * the built-in route looks after [feed] found one
     * ([SessionService]).
     */
    fun moved(picture: Long?): Boolean {
        if (!reported || picture == null || picture == frozen) return false
        reported = false
        frozen = null
        this.picture = null
        presentsSince = null
        return true
    }

    /** A new game: no picture seen yet. */
    fun reset() {
        picture = null
        presentsSince = null
        reported = false
        frozen = null
    }

    companion object {
        /** How long the picture stays the same, with frames, before it counts as frozen. */
        const val FROZEN_MS = 30_000L

        /** The frames in that time: fewer, and the game is not drawing (a loading screen, a hang the no-frame rule sees). */
        const val MIN_PRESENTS = 60L

        /** How often the service looks ([SessionService]). */
        const val LOOK_MS = 2_000L

        /** The routes whose games the service follows with this rule. */
        val ROUTES = setOf(GraphicsRoute.VIRGL, GraphicsRoute.LIBHYBRIS, GraphicsRoute.MALI_PANVK, GraphicsRoute.BUILT_IN)

        /**
         * A hash of [pixels] at [GRID_X] x [GRID_Y] points spread over the screen: enough to see
         * any change of D2's moving parts, a few thousand reads every [LOOK_MS].
         */
        fun hash(pixels: ScreenPixels): Long = hash(pixels.header.width, pixels.header.height, pixels::pixel)

        /** The same hash of a [width] x [height] picture whose pixels [pixel] reads. */
        fun hash(width: Int, height: Int, pixel: (Int, Int) -> Int): Long {
            var hash = -0x340d631b7bdddcdbL // FNV-1a offset basis
            for (gy in 0 until GRID_Y) {
                val y = (gy * height + height / 2) / GRID_Y
                for (gx in 0 until GRID_X) {
                    val x = (gx * width + width / 2) / GRID_X
                    hash = (hash xor (pixel(x, y).toLong() and 0xFFFFFFFFL)) * 0x100000001b3L
                }
            }
            return hash
        }

        private const val GRID_X = 96
        private const val GRID_Y = 54
    }
}
