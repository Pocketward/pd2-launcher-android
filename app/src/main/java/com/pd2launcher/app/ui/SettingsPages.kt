package com.pd2launcher.app.ui

import android.view.KeyEvent
import androidx.annotation.DrawableRes
import androidx.annotation.StringRes
import androidx.compose.foundation.ScrollState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import com.pd2launcher.app.LauncherSettings
import com.pd2launcher.app.Line
import com.pd2launcher.app.R
import com.pd2launcher.app.Tone
import com.pd2launcher.app.control.Renderer
import com.pd2launcher.app.input.TouchMode

/**
 * Settings' pages, in the list's order: L1 and R1 step along it one page at a time, with no wrap
 * at the ends, as along the in-game menu's strip ([MenuStop]). Each page holds its [groups].
 */
enum class SettingsPage(@param:StringRes val title: Int, @param:DrawableRes val icon: Int) {
    DISPLAY(R.string.settings_page_display, R.drawable.ic_settings_display),
    GAME(R.string.settings_page_game, R.drawable.ic_settings_game),
    CONTROLS(R.string.settings_page_controls, R.drawable.ic_menu_controls),
    FILES(R.string.settings_page_files, R.drawable.ic_settings_files),
    APP(R.string.settings_page_app, R.drawable.ic_settings_updates),
    HELP(R.string.settings_page_help, R.drawable.ic_settings_help),
    ;

    /** The page's rows in their groups, in order; a row shows only where it applies ([SettingsRow.shown]). */
    val groups: List<RowGroup>
        get() = when (this) {
            DISPLAY -> listOf(
                // The GPU and the Renderer lead Display with no overline (decision 23).
                RowGroup(null, SettingsRow.GPU, SettingsRow.RENDERER),
                RowGroup(R.string.settings_section_picture, SettingsRow.GAME_RESOLUTION, SettingsRow.DISPLAY, SettingsRow.GRAPHICS_PROFILE),
                RowGroup(R.string.settings_frame_rate, SettingsRow.FRAME_RATE, SettingsRow.SLOWER_IN_BACKGROUND, SettingsRow.FPS_COUNTER),
                RowGroup(R.string.settings_section_screen, SettingsRow.KEEP_SCREEN_ON),
            )
            GAME -> listOf(
                RowGroup(R.string.settings_section_launcher, SettingsRow.GAME_UPDATE_CHECKS),
                // Each program with its start options (owner, 2026-11-08; mock docs/design/program-paths):
                // Skip to Battle.net and Sound in background inside the Start options drop-down.
                RowGroup(
                    R.string.settings_section_game_program, SettingsRow.GAME_PROGRAM, SettingsRow.START_OPTIONS,
                    SettingsRow.SKIP_TO_BNET, SettingsRow.SOUND_IN_BACKGROUND, SettingsRow.CUSTOM_ARGUMENTS,
                ),
                RowGroup(R.string.settings_section_official_launcher, SettingsRow.LAUNCHER_PROGRAM, SettingsRow.LAUNCHER_CUSTOM_ARGUMENTS),
                RowGroup(R.string.settings_loot_filters, SettingsRow.LOOT_FILTERS),
            )
            CONTROLS -> listOf(
                RowGroup(null, SettingsRow.ON_SCREEN_CONTROLS, SettingsRow.BUTTONS),
                RowGroup(R.string.controls_touch, SettingsRow.TOUCH, SettingsRow.POINTER_SPEED),
                RowGroup(R.string.controls_section_controller, SettingsRow.CHORD),
            )
            FILES -> listOf(
                RowGroup(R.string.settings_section_game_files, SettingsRow.GAME_FOLDER, SettingsRow.SAVE_SYNC),
                RowGroup(R.string.settings_section_storage, SettingsRow.STORAGE),
            )
            APP -> listOf(
                RowGroup(null, SettingsRow.APP_STATUS),
                RowGroup(R.string.app_update_channel_choice, SettingsRow.UPDATE_CHANNEL),
                RowGroup(null, SettingsRow.ABOUT, gap = true),
            )
            HELP -> listOf(
                // Detailed logging stands apart, so no rule goes between it and Save logs.
                RowGroup(
                    R.string.settings_section_problem_reports, SettingsRow.DETAILED_LOGGING, SettingsRow.SAVE_LOGS,
                    hint = R.string.settings_logs_note, ruled = false,
                ),
                RowGroup(R.string.settings_section_android, SettingsRow.NOTIFICATIONS, SettingsRow.BATTERY),
            )
        }

    /**
     * The row that takes the focus when L1 or R1 opens the page, or when the rows go from under the
     * focus, the same each time: the first that shows ([shown]) and can take it
     * ([SettingsRow.takesFirstFocus]; [editable]: the Launcher's settings take a change). Where
     * none can, the first grey switch, which the controller can reach and read. Game's Loot filters
     * row always can, so Game's first focus is its first switch, else Loot filters.
     */
    fun firstRow(shown: (SettingsRow) -> Boolean, editable: Boolean): SettingsRow {
        val rows = groups.flatMap { it.rows }.filter(shown)
        return rows.firstOrNull { it.takesFirstFocus(editable) } ?: rows.first { it.takesFirstFocus(editable = true) }
    }

    /**
     * The page's line in the list ([facts]: the settings now): Display "1068 × 600 · 60 fps", Game
     * "Game updates: On", Controls "Trackpad · 1.00×", Files and saves "Save sync: Off", App the
     * channel and its version ([com.pd2launcher.app.AppUpdate.pageLine]; "Version x.y.z" in a build
     * that gets no updates), Help "Logs · Android". A page that needs the player says why instead,
     * with its [PageLine.alert]: the Launcher could not save its settings (Game), Save sync cannot
     * reach its folder or failed (Files and saves), a download stopped or failed (App), battery optimisation
     * is on or notifications are off (Help).
     */
    fun line(facts: SettingsFacts): PageLine = when (this) {
        DISPLAY -> PageLine(
            listOf(
                Line(R.string.settings_size, facts.size.first, facts.size.second),
                Line(R.string.game_fps, facts.frameRate),
            ),
        )
        GAME -> {
            val launcher = facts.launcher
            val checks = launcher?.gameUpdateChecks
            when {
                launcher?.failure != null -> PageLine(listOf(Line(R.string.settings_not_saved)), Tone.ERROR)
                checks != null -> PageLine(listOf(Line(if (checks) R.string.settings_list_game_updates_on else R.string.settings_list_game_updates_off)))
                launcher == null -> PageLine(listOf(Line(R.string.settings_waiting)))
                else -> PageLine(listOf(rendererValue(launcher.renderer)))
            }
        }
        CONTROLS -> PageLine(
            listOf(
                Line(if (facts.touch == TouchMode.DIRECT) R.string.controls_touch_direct else R.string.controls_touch_trackpad),
                Line(R.string.controls_factor, speedText(facts.pointerSpeed)),
            ),
        )
        FILES -> when {
            facts.syncOn && facts.syncBroken -> PageLine(listOf(Line(R.string.settings_list_sync_problem)), Tone.WARNING)
            else -> PageLine(listOf(Line(if (facts.syncOn) R.string.settings_list_sync_on else R.string.settings_list_sync_off)))
        }
        APP -> facts.appLine ?: PageLine(listOf(Line(R.string.about_version, facts.version)))
        // Notifications first (the owner, 2026-11-03: "notification setting is more important than battery permissions").
        HELP -> when {
            facts.system.notificationsOff -> PageLine(listOf(Line(R.string.settings_list_notifications)), Tone.WARNING)
            facts.system.batteryRestricted -> PageLine(listOf(Line(R.string.settings_list_battery)), Tone.WARNING)
            else -> PageLine(listOf(Line(R.string.settings_section_logs), Line(R.string.settings_section_android)))
        }
    }

    companion object {
        /** The page the shoulder button [keyCode] goes to from [from] (L1 back, R1 on); null at an end or for another key. */
        fun step(from: SettingsPage, keyCode: Int): SettingsPage? = when (keyCode) {
            KeyEvent.KEYCODE_BUTTON_L1 -> entries.getOrNull(from.ordinal - 1)
            KeyEvent.KEYCODE_BUTTON_R1 -> entries.getOrNull(from.ordinal + 1)
            else -> null
        }

        /** The page a debug intent names (`--es page game`), in any case; null: none. */
        fun named(name: String?): SettingsPage? = entries.firstOrNull { it.name.equals(name, ignoreCase = true) }
    }
}

/**
 * The Start options row's value: null while Skip to Battle.net is on and Sound in background off
 * (the defaults; the row says "Default"), else the start options that are on ("-skiptobnet",
 * "-sndbkg"), an empty list when none is.
 */
fun startOptionsOn(skipToBnet: Boolean, soundInBackground: Boolean): List<String>? =
    if (skipToBnet && !soundInBackground) null else listOfNotNull("-skiptobnet".takeIf { skipToBnet }, "-sndbkg".takeIf { soundInBackground })

/** The renderer as the Game page's Renderer row and the list name it: "Glide (recommended)" or "DirectDraw". */
fun rendererValue(renderer: Renderer): Line =
    Line(if (renderer == Renderer.DIRECT_DRAW) R.string.settings_renderer_direct_draw else R.string.settings_renderer_glide_recommended)

/** Every row of Settings; each page holds its own ([SettingsPage.groups]). */
enum class SettingsRow {
    GAME_RESOLUTION,
    DISPLAY,
    GRAPHICS_PROFILE,
    FRAME_RATE,
    FPS_COUNTER,
    KEEP_SCREEN_ON,
    SKIP_TO_BNET,
    GAME_UPDATE_CHECKS,
    SLOWER_IN_BACKGROUND,
    SOUND_IN_BACKGROUND,
    /** Game's Custom start options: the player's own options after the Launcher's, which open their input ([CustomArgumentsInput]). */
    CUSTOM_ARGUMENTS,
    /** Game's Game program: the program Play starts, which opens its input ([ProgramPathInput]). */
    GAME_PROGRAM,
    /** Game's Start options: the drop-down that holds [SKIP_TO_BNET] and [SOUND_IN_BACKGROUND] ([SettingsNav.startOptionsOpen]). */
    START_OPTIONS,
    /** Game's Launcher program: the program the official PD2 launcher button opens ([ProgramPathInput]). */
    LAUNCHER_PROGRAM,
    /** The official PD2 launcher's Custom start options ([CustomArgumentsInput]). */
    LAUNCHER_CUSTOM_ARGUMENTS,
    RENDERER,
    LOOT_FILTERS,
    ON_SCREEN_CONTROLS,
    BUTTONS,
    CHORD,
    POINTER_SPEED,
    TOUCH,
    GAME_FOLDER,
    SAVE_SYNC,
    STORAGE,
    APP_STATUS,
    UPDATE_CHANNEL,
    DETAILED_LOGGING,
    SAVE_LOGS,
    BATTERY,
    NOTIFICATIONS,
    GPU,
    ABOUT,
    ;

    /**
     * Whether the row shows now: Check for game updates only with an engine that has the setting
     * ([gameUpdateChecks]), the app-update rows only in a build that gets app updates
     * ([updates]), and Skip to Battle.net and Sound in background only while the Start options
     * drop-down is open ([startOptionsOpen]); every other row always.
     */
    fun shown(gameUpdateChecks: Boolean, updates: Boolean, startOptionsOpen: Boolean = false): Boolean = when (this) {
        GAME_UPDATE_CHECKS -> gameUpdateChecks
        APP_STATUS, UPDATE_CHANNEL -> updates
        SKIP_TO_BNET, SOUND_IN_BACKGROUND -> startOptionsOpen
        else -> true
    }

    /** Skip to Battle.net and Sound in background: the rows inside the Start options drop-down, set in under it. */
    val inStartOptions: Boolean get() = this == SKIP_TO_BNET || this == SOUND_IN_BACKGROUND

    /**
     * Whether the row can hold its page's first focus: a row that the Settings screen gives the
     * page's requester, and that is not grey. The Launcher's switches are grey unless [editable];
     * the Renderer row is always grey, and Check now is grey during a check.
     */
    fun takesFirstFocus(editable: Boolean): Boolean = when (this) {
        GAME_RESOLUTION, ON_SCREEN_CONTROLS, GAME_FOLDER, UPDATE_CHANNEL, ABOUT, DETAILED_LOGGING -> true
        // Display's GPU row opens its sub page in every build (the rows there take a pick, DriverPage.SELECTABLE).
        GPU -> true
        // Game's Loot filters row opens its page (owner, 2026-11-05).
        LOOT_FILTERS -> true
        GAME_UPDATE_CHECKS, SKIP_TO_BNET, SOUND_IN_BACKGROUND, CUSTOM_ARGUMENTS,
        GAME_PROGRAM, START_OPTIONS, LAUNCHER_PROGRAM, LAUNCHER_CUSTOM_ARGUMENTS,
        -> editable
        else -> false
    }
}

/**
 * A group of rows under its overline ([label]; null: none, as the first rows of Controls), with
 * the line that says what the group is ([hint]). [ruled]: a rule between its rows. [gap]: the group
 * closes the page, after a wider gap and a rule.
 */
class RowGroup(
    @param:StringRes val label: Int?,
    vararg rows: SettingsRow,
    @param:StringRes val hint: Int? = null,
    val ruled: Boolean = true,
    val gap: Boolean = false,
) {
    val rows: List<SettingsRow> = rows.toList()
}

/** What the list's lines read: the settings now, and the state of the device that they depend on. */
data class SettingsFacts(
    /** The X screen the next Play uses ([com.pd2launcher.app.GameResolution.forPlay]). */
    val size: Pair<Int, Int>,
    /** In use on this display ([com.pd2launcher.app.FramePick.onPanel]), not as stored. */
    val frameRate: Int,
    val launcher: LauncherSettings.View?,
    val touch: TouchMode,
    val pointerSpeed: Float,
    /** Save sync is on; [syncBroken]: its last sync could not reach the folder, or failed. */
    val syncOn: Boolean,
    val syncBroken: Boolean,
    /** This app's version, and the App page's line where this build gets updates (null: none). */
    val version: String,
    val appLine: PageLine?,
    val system: SystemState,
)

/**
 * The rows to give the focus so that the page does not scroll, as indexes of [spans] (each row's
 * top to bottom, in a view [height] high): the rows wholly in view from the top, then those only
 * partly in view. A row with no height is none.
 */
fun rowsInView(spans: List<ClosedFloatingPointRange<Float>>, height: Float): List<Int> =
    spans.indices.filter { spans[it].start < spans[it].endInclusive && spans[it].start < height && spans[it].endInclusive > 0f }
        .sortedBy { spans[it].start }
        .sortedBy { spans[it].start < 0f || spans[it].endInclusive > height }

/**
 * A page's line in the list: its values now ([lines], joined with " · "); [alert]: the page needs
 * the player (a warning, amber, or an error, red), with a mark. [news]: a new version of the
 * selected channel, shown as the amber dot beside the words.
 */
data class PageLine(val lines: List<Line>, val alert: Tone? = null, val news: Boolean = false)

/**
 * Where Settings stands: the open page, on a phone whether it shows or the list does, how far each
 * page and the list are scrolled, and the sheet or input open over the page. Kept while Settings
 * is away (the on-screen controls editor, the Controls screen, About, the game).
 */
class SettingsNav {
    var page by mutableStateOf(SettingsPage.DISPLAY)
    /** Phones: the open page shows (else the list). Handhelds show both. */
    var pageShown by mutableStateOf(false)
    /** Play's Version opened the App page: on a phone its back label and Back go to Play, not to the list. */
    var fromPlay by mutableStateOf(false)
    /** Counts the shoulder buttons' steps: each moves the focus to the first row of the page that it opens. */
    var focusPage by mutableIntStateOf(0)
    /** The row that opened the sheet, the input or the screen over the page: the focus goes back to it. */
    var opener: SettingsRow? = null
    /** How far the page was scrolled when [opener] opened a screen; the way back puts it there again. */
    var openerScroll = 0
    var resolutions by mutableStateOf(false)
    var customSize by mutableStateOf(false)
    var displays by mutableStateOf(false)
    var profiles by mutableStateOf(false)
    var frameRates by mutableStateOf(false)
    var pointerSpeed by mutableStateOf(false)
    var customArguments by mutableStateOf(false)
    var gameProgram by mutableStateOf(false)
    var launcherProgram by mutableStateOf(false)
    var launcherCustomArguments by mutableStateOf(false)
    /** Game's Start options drop-down is open: Skip to Battle.net and Sound in background show under it. Closed at first. */
    var startOptionsOpen by mutableStateOf(false)
    /** Display's GPU sub page shows over Settings ([com.pd2launcher.app.ui.GpuPage]); Back returns to its row. */
    var gpu by mutableStateOf(false)
    /** Game's Loot filters page shows over Settings ([com.pd2launcher.app.ui.LootFiltersPage]); Back returns to its row. */
    var lootFilters by mutableStateOf(false)
    val scroll: Map<SettingsPage, ScrollState> = SettingsPage.entries.associateWith { ScrollState(0) }
    val listScroll = ScrollState(0)

    /** A sheet or an input shows over the page: it takes Back, and L1 and R1 do not step. */
    val popup: Boolean get() = resolutions || customSize || displays || profiles || frameRates || pointerSpeed || customArguments || gameProgram ||
            launcherProgram || launcherCustomArguments || gpu || lootFilters

    /**
     * Play's Settings button and Repair's: the last page that was open (Display at first); a phone
     * shows the list first.
     */
    fun openSettings() {
        closePopups()
        pageShown = false
        fromPlay = false
    }

    /** Display's GPU sub page in view ([gpu]): Play's button after a driver failure and the card's Quit. */
    fun openGpu() {
        open(SettingsPage.DISPLAY)
        // Back from the sub page returns to the GPU row.
        opener = SettingsRow.GPU
        gpu = true
    }

    /** [page] in view: Play's Controls button, a row of the list, L1 and R1. */
    fun open(page: SettingsPage) {
        closePopups()
        this.page = page
        pageShown = true
        fromPlay = false
    }

    /**
     * Play's Version: the App page itself. [fromPlay] false: the page comes back after Android made
     * the activity anew, and the player had come from the list.
     */
    fun openVersion(fromPlay: Boolean = true) {
        open(SettingsPage.APP)
        this.fromPlay = fromPlay
    }

    /**
     * Back: on a [phone] an open page goes back to the list (true), unless Play's Version opened
     * it; else Settings closes (false).
     */
    fun back(phone: Boolean): Boolean {
        if (!phone || !pageShown || fromPlay) return false
        pageShown = false
        return true
    }

    /**
     * A shoulder button ([keyCode]) wherever the focus is: L1 opens the page before, R1 the page
     * after ([SettingsPage.step]; a [press], not its release or a repeat), and the focus goes to
     * its first row ([SettingsPage.firstRow]). True: the key was theirs. Not while a sheet or an
     * input shows ([popup]).
     */
    fun shoulder(keyCode: Int, press: Boolean): Boolean {
        if (!MenuStop.isStep(keyCode) || popup) return false
        if (press) {
            SettingsPage.step(page, keyCode)?.let {
                open(it)
                focusPage++
            }
        }
        return true
    }

    private fun closePopups() {
        opener = null
        resolutions = false
        customSize = false
        displays = false
        profiles = false
        frameRates = false
        pointerSpeed = false
        customArguments = false
        gameProgram = false
        launcherProgram = false
        launcherCustomArguments = false
        gpu = false
        lootFilters = false
    }
}
