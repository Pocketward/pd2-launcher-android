package com.pd2launcher.app

import android.content.Context
import com.pd2launcher.app.control.GraphicsDriver
import com.pd2launcher.app.control.GraphicsProfile
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

/**
 * The Android app's own settings: kept in the app's preferences, not the Launcher's. Most are read
 * when the Game view opens or at Play, so a change takes effect on the next Play; the Game view
 * follows some live (Display, the frame rate, Show fps), as each one's KDoc says.
 */
data class AppSettings(
    /** The screen stays on while the Game view shows. */
    val keepScreenOn: Boolean = true,
    /** A small counter of the game's frames per second shows over the Game view. */
    val showFps: Boolean = false,
    /**
     * The game resolution's Custom size the player typed ([GameResolution.typedCustom]), from the
     * next Play or official PD2 launcher; null: the game's own size ([GameResolution.ownSize]).
     */
    val customSize: Pair<Int, Int>? = null,
    /**
     * How the Game view shows the game's picture or the official launcher's window ([GameDisplay]);
     * the Game view reads it live, with no restart of the game.
     */
    val gameDisplay: GameDisplay = GameDisplay.FIT,
    /** D2GL's graphics profile the player picked; null: Automatic ([GraphicsProfiles]). */
    val graphicsProfile: GraphicsProfile? = null,
    /**
     * The game's frame rate on the screen, in frames a second: a whole number from
     * [FrameRateVote.MIN] that the player typed, or [FULL_FRAME_RATE]: the screen's full rate. 60
     * by default. A screen not faster than the value shows its full rate ([FrameRateVote.cap]).
     */
    val frameRate: Int = 60,
    /**
     * While the game is not on the screen (the app in the background, the screen off, the app's own
     * screens over the game), the game is told so (its window loses the focus), and it draws at its
     * own background frame rate (D2GL's Max Background FPS, 25 by default).
     */
    val lowFrameRateInBackground: Boolean = true,
    /** The notice on Play about removing the installer downloads was answered ([InstallerDownloads.showNotice]). */
    val downloadsNoticeAnswered: Boolean = false,
    /**
     * A game session of at least [InstallerDownloads.PLAYED_MS] has ended since the game was
     * installed: that notice waits for it.
     */
    val gamePlayed: Boolean = false,
    /** The app looks for a newer release of itself once a day ([AppUpdater]); off: only About's Check now. */
    val updateChecks: Boolean = true,
    /**
     * Detailed logging (Settings → App → Help): the game's and the official launcher's Wine
     * processes log more from their next start (their network connects and closes, every
     * exception, Box64's own log), for a problem report; off by default, and it stays as the
     * player set it. Sent with each Play and each press of the official launcher.
     */
    val detailedLogging: Boolean = false,
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
            CUSTOM_SIZE to customSize?.let { (width, height) -> "${width}x$height" },
            VIEW_DISPLAY to gameDisplay.takeIf { it != GameDisplay.FIT }?.name,
            // The older game resolution and display keys: not read, so every earlier pick is the game's own
            // size and Fit to screen once; removed at the first save.
            GAME_RESOLUTION to null,
            GAME_SCALE to null,
            GAME_SIZE to null,
            GAME_DISPLAY to null,
            GRAPHICS_PROFILE to graphicsProfile?.name,
            // The older Strict CPU emulation pick (a Boolean): not read, removed at the first save.
            STRICT_CPU_EMULATION to null,
            FRAME_RATE to frameRate,
            // The older "Limit to 60 fps" switch, now FRAME_RATE's 60: removed at the first save.
            LIMIT_FRAME_RATE to null,
            LOW_FRAME_RATE_IN_BACKGROUND to lowFrameRateInBackground,
            // The official launcher's own resolution of earlier versions (lines): not read, as the launcher
            // now opens at the game resolution; removed at the first save.
            LAUNCHER_RESOLUTION to null,
            DOWNLOADS_NOTICE_ANSWERED to downloadsNoticeAnswered,
            GAME_PLAYED to gamePlayed,
            UPDATE_CHECKS to updateChecks,
            DETAILED_LOGGING to detailedLogging,
        )

    companion object {
        const val KEEP_SCREEN_ON = "keep_screen_on"
        const val SHOW_FPS = "show_fps"
        /** [customSize] as "<width>x<height>"; none: the game's own size. */
        const val CUSTOM_SIZE = "custom_size"
        /** [gameDisplay]'s name; none: [GameDisplay.FIT]. */
        const val VIEW_DISPLAY = "view_display"
        /** Older game resolution and display keys (lines, a scale, a size's name, a display's name): not read, removed at the first save. */
        const val GAME_RESOLUTION = "game_resolution"
        const val GAME_SCALE = "game_scale"
        const val GAME_SIZE = "game_size"
        const val GAME_DISPLAY = "game_display"
        const val GRAPHICS_PROFILE = "graphics_profile"
        /** The older Strict CPU emulation pick (a Boolean): not read, removed at the first save. */
        const val STRICT_CPU_EMULATION = "strict_cpu_emulation"
        const val FRAME_RATE = "frame_rate"
        /** [frameRate]'s value for Full, the screen's full rate: stored, so a player's Full is not the default. */
        const val FULL_FRAME_RATE = 0
        /** The older "Limit to 60 fps" switch (a Boolean): on, it was 60, now the default. */
        const val LIMIT_FRAME_RATE = "limit_frame_rate"
        const val LOW_FRAME_RATE_IN_BACKGROUND = "low_frame_rate_in_background"
        /** The older official launcher resolution (an Int, lines): not read, removed at the first save. */
        const val LAUNCHER_RESOLUTION = "launcher_resolution"
        const val DOWNLOADS_NOTICE_ANSWERED = "downloads_notice_answered"
        const val GAME_PLAYED = "game_played"
        const val UPDATE_CHECKS = "update_checks"
        const val DETAILED_LOGGING = "detailed_logging"

        /** "<width>x<height>" of positive whole numbers, else null. */
        private fun parseSize(text: String): Pair<Int, Int>? {
            val (width, height) = Regex("([0-9]{1,5})x([0-9]{1,5})").matchEntire(text)?.destructured ?: return null
            return (width.toInt() to height.toInt()).takeIf { (w, h) -> w > 0 && h > 0 }
        }

        /** A missing or mistyped value is the default, one by one. */
        fun fromPreferences(values: Map<String, *>): AppSettings {
            val defaults = AppSettings()
            return AppSettings(
                keepScreenOn = values[KEEP_SCREEN_ON] as? Boolean ?: defaults.keepScreenOn,
                showFps = values[SHOW_FPS] as? Boolean ?: defaults.showFps,
                customSize = (values[CUSTOM_SIZE] as? String)?.let(::parseSize),
                gameDisplay = GameDisplay.entries.firstOrNull { it.name == values[VIEW_DISPLAY] } ?: defaults.gameDisplay,
                graphicsProfile = GraphicsProfile.entries.firstOrNull { it.name == values[GRAPHICS_PROFILE] },
                frameRate = (values[FRAME_RATE] as? Int)?.takeIf { it == FULL_FRAME_RATE || it >= FrameRateVote.MIN }
                    ?: defaults.frameRate,
                lowFrameRateInBackground = values[LOW_FRAME_RATE_IN_BACKGROUND] as? Boolean ?: defaults.lowFrameRateInBackground,
                downloadsNoticeAnswered = values[DOWNLOADS_NOTICE_ANSWERED] as? Boolean ?: defaults.downloadsNoticeAnswered,
                gamePlayed = values[GAME_PLAYED] as? Boolean ?: defaults.gamePlayed,
                updateChecks = values[UPDATE_CHECKS] as? Boolean ?: defaults.updateChecks,
                detailedLogging = values[DETAILED_LOGGING] as? Boolean ?: defaults.detailedLogging,
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
