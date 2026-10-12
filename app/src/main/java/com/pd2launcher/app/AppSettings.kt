package com.pd2launcher.app

import android.content.Context
import com.pd2launcher.app.control.GraphicsDriver
import com.pd2launcher.app.control.GraphicsProfile
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

/**
 * The Android app's own settings: kept in the app's preferences, not the Launcher's, and read
 * when the Game view opens, so a change takes effect on the next Play.
 */
data class AppSettings(
    /** The screen stays on while the Game view shows. */
    val keepScreenOn: Boolean = true,
    /** A small counter of the game's frames per second shows over the Game view. */
    val showFps: Boolean = false,
    /** The game resolution the player picked, in lines; null: Automatic ([GameResolution]). */
    val gameResolution: Int? = null,
    /** D2GL's graphics profile the player picked; null: Automatic ([GraphicsProfiles]). */
    val graphicsProfile: GraphicsProfile? = null,
    /**
     * On a screen faster than 60 Hz the game is limited to 60 frames a second; off: the screen's
     * full rate ([FrameRateVote]).
     */
    val limitFrameRate: Boolean = false,
    /**
     * The official PD2 launcher's screen the player picked, in lines; null: the display's full
     * size ([GameResolution.forLauncher]). Its own, not the game's: its window needs room.
     */
    val launcherResolution: Int? = null,
    /** The notice on Play about removing the installer downloads was answered ([InstallerDownloads.showNotice]). */
    val downloadsNoticeAnswered: Boolean = false,
    /**
     * A game session of at least [InstallerDownloads.PLAYED_MS] has ended since the game was
     * installed: that notice waits for it.
     */
    val gamePlayed: Boolean = false,
) {
    /**
     * Start over took the installation ([SetupRecord.NONE]): the downloads notice belongs to it,
     * so it waits for the next installation's first game session again.
     */
    fun afterStartOver(): AppSettings = copy(downloadsNoticeAnswered = false, gamePlayed = false)

    /**
     * The graphics driver every Play uses: the App userland's standard Turnip. The Compatibility
     * values stay in the engine but are not offered while there is only this one Turnip.
     */
    val graphicsDriver: GraphicsDriver get() = GraphicsDriver.STANDARD

    /** The preference values, by key (null: none); [fromPreferences] reads them back. */
    fun toPreferences(): Map<String, Any?> =
        mapOf(
            KEEP_SCREEN_ON to keepScreenOn,
            SHOW_FPS to showFps,
            GAME_RESOLUTION to gameResolution,
            GRAPHICS_PROFILE to graphicsProfile?.name,
            LIMIT_FRAME_RATE to limitFrameRate,
            LAUNCHER_RESOLUTION to launcherResolution,
            DOWNLOADS_NOTICE_ANSWERED to downloadsNoticeAnswered,
            GAME_PLAYED to gamePlayed,
        )

    companion object {
        const val KEEP_SCREEN_ON = "keep_screen_on"
        const val SHOW_FPS = "show_fps"
        const val GAME_RESOLUTION = "game_resolution"
        const val GRAPHICS_PROFILE = "graphics_profile"
        const val LIMIT_FRAME_RATE = "limit_frame_rate"
        const val LAUNCHER_RESOLUTION = "launcher_resolution"
        const val DOWNLOADS_NOTICE_ANSWERED = "downloads_notice_answered"
        const val GAME_PLAYED = "game_played"

        /** A missing or mistyped value is the default, one by one. */
        fun fromPreferences(values: Map<String, *>): AppSettings {
            val defaults = AppSettings()
            return AppSettings(
                keepScreenOn = values[KEEP_SCREEN_ON] as? Boolean ?: defaults.keepScreenOn,
                showFps = values[SHOW_FPS] as? Boolean ?: defaults.showFps,
                gameResolution = (values[GAME_RESOLUTION] as? Int)?.takeIf { it >= GameResolution.MIN_LINES },
                graphicsProfile = GraphicsProfile.entries.firstOrNull { it.name == values[GRAPHICS_PROFILE] },
                limitFrameRate = values[LIMIT_FRAME_RATE] as? Boolean ?: defaults.limitFrameRate,
                launcherResolution = (values[LAUNCHER_RESOLUTION] as? Int)?.takeIf { it >= GameResolution.MIN_LINES },
                downloadsNoticeAnswered = values[DOWNLOADS_NOTICE_ANSWERED] as? Boolean ?: defaults.downloadsNoticeAnswered,
                gamePlayed = values[GAME_PLAYED] as? Boolean ?: defaults.gamePlayed,
            )
        }
    }
}

/** [AppSettings] in the app's `app_settings` preferences, and as a flow the screens follow. */
object AppSettingsStore {
    private const val PREFERENCES = "app_settings"
    private val _current = MutableStateFlow(AppSettings())

    /** The settings [load] read last, or the defaults before. */
    val current: StateFlow<AppSettings> = _current.asStateFlow()

    fun load(context: Context): AppSettings =
        AppSettings.fromPreferences(context.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE).all)
            .also { _current.value = it }

    fun save(context: Context, settings: AppSettings) {
        val editor = context.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE).edit()
        for ((key, value) in settings.toPreferences()) {
            when (value) {
                is Boolean -> editor.putBoolean(key, value)
                is String -> editor.putString(key, value)
                is Int -> editor.putInt(key, value)
                null -> editor.remove(key)
            }
        }
        editor.apply()
        _current.value = settings
    }

    /** A game session ended ([AppSettings.gamePlayed]): saved the first time only. */
    fun gamePlayed(context: Context) {
        val settings = load(context)
        if (!settings.gamePlayed) save(context, settings.copy(gamePlayed = true))
    }

    /** Start over's reset succeeded ([AppSettings.afterStartOver]). */
    fun startedOver(context: Context) {
        val settings = load(context)
        val next = settings.afterStartOver()
        if (next != settings) save(context, next)
    }
}
