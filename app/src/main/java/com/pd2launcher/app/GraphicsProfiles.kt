package com.pd2launcher.app

import com.pd2launcher.app.control.GraphicsProfile

/**
 * D2GL's graphics profile (Settings → Game → Graphics profile): the D2GL values the engine writes
 * into the game's `d2gl.json` when the profile a Play sends is not the one it applied last, so the player's own changes in the game (Ctrl+O) stay until the
 * profile changes. The player's pick is [AppSettings.graphicsProfile]; none is Automatic
 * ([automatic]), resolved here like the game resolution's ([GameResolution.automaticLines]).
 */
object GraphicsProfiles {
    /**
     * Automatic's profile: Performance on the older and entry-level Adrenos ([lowTierAdreno]),
     * where Project Diablo 2's own D2GL settings are slow, and on a Mali, whose PanVK path is
     * tuned for it; else (a newer Adreno, or no GPU known) Default.
     */
    fun automatic(gpu: HostGpu?): GraphicsProfile = when {
        gpu is MaliGpu -> GraphicsProfile.PERFORMANCE
        gpu is KgslGpu && lowTierAdreno(gpu.chipId) -> GraphicsProfile.PERFORMANCE
        else -> GraphicsProfile.DEFAULT
    }

    /** The profile a Play sends: the [picked] one, else Automatic's. */
    fun forPlay(picked: GraphicsProfile?, gpu: HostGpu?): GraphicsProfile = picked ?: automatic(gpu)

    /** One of Settings' choices: [profile] (null: Automatic), its [title] and what it means. */
    data class Choice(val profile: GraphicsProfile?, val title: Line, val body: Line)

    /** What Settings' row shows: the [picked] profile, or Automatic with what it picks on this GPU. */
    fun shown(picked: GraphicsProfile?, gpu: HostGpu?): Line = when (picked) {
        null -> automaticTitle(gpu)
        GraphicsProfile.DEFAULT -> Line(R.string.settings_graphics_default)
        GraphicsProfile.PERFORMANCE -> Line(R.string.settings_graphics_performance)
    }

    /** Settings' choices, in order: Automatic (what it picks on this GPU), Default, Performance. */
    fun choices(gpu: HostGpu?): List<Choice> = listOf(
        Choice(null, automaticTitle(gpu), Line(R.string.settings_graphics_automatic_body)),
        Choice(GraphicsProfile.DEFAULT, Line(R.string.settings_graphics_default), Line(R.string.settings_graphics_default_body)),
        Choice(GraphicsProfile.PERFORMANCE, Line(R.string.settings_graphics_performance), Line(R.string.settings_graphics_performance_body)),
    )

    private fun automaticTitle(gpu: HostGpu?): Line = when (automatic(gpu)) {
        GraphicsProfile.DEFAULT -> Line(R.string.settings_graphics_automatic_default)
        GraphicsProfile.PERFORMANCE -> Line(R.string.settings_graphics_automatic_performance)
    }
}
