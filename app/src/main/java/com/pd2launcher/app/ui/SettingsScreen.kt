package com.pd2launcher.app.ui

import androidx.activity.compose.BackHandler
import androidx.annotation.StringRes
import androidx.compose.foundation.LocalIndication
import androidx.compose.foundation.ScrollState
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.interaction.collectIsFocusedAsState
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxScope
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawing
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.pd2launcher.app.AppSettings
import com.pd2launcher.app.FrameRateVote
import com.pd2launcher.app.GameDisplay
import com.pd2launcher.app.GameResolution
import com.pd2launcher.app.GraphicsProfiles
import com.pd2launcher.app.HostGpu
import com.pd2launcher.app.LauncherSettings
import com.pd2launcher.app.R
import com.pd2launcher.app.Tone
import com.pd2launcher.app.control.Command
import com.pd2launcher.app.control.Renderer
import com.pd2launcher.app.input.Chord
import com.pd2launcher.app.input.Controls
import com.pd2launcher.app.input.ControlsStore
import com.pd2launcher.app.input.PadAction
import com.pd2launcher.app.input.PadButton
import com.pd2launcher.app.input.PointerSpeed

/** What the Android section says about the system settings the app depends on. */
data class SystemState(val batteryRestricted: Boolean, val notificationsOff: Boolean)

/**
 * Where Settings stands: the open tab, how far each tab is scrolled, the bindings' mode, the
 * binding being picked. Kept while Settings is away (the on-screen controls editor, the game).
 */
class SettingsNav {
    var tab by mutableIntStateOf(GAME)
    var mouse by mutableStateOf(false)
    var picker by mutableStateOf<PadButton?>(null)
    var resolutions by mutableStateOf(false)
    var customSize by mutableStateOf(false)
    var displays by mutableStateOf(false)
    var profiles by mutableStateOf(false)
    var frameRates by mutableStateOf(false)
    val scroll = List(3) { ScrollState(0) }

    fun open(tab: Int) {
        this.tab = tab
        picker = null
        resolutions = false
        customSize = false
        displays = false
        profiles = false
        frameRates = false
    }

    companion object {
        const val GAME = 0
        const val APP = 1
        const val CONTROLLER = 2
    }
}

/**
 * Settings, in three text tabs: Game (the Launcher's settings that apply on Android, [launcher],
 * changed with [onCommand]; the game resolution, also the official launcher's, whose choices
 * follow the landscape [display]; how the Game view shows the game and the official launcher's
 * window (Display, at once, also over a running game, with no restart); D2GL's
 * graphics profile, whose Automatic follows the [gpu]; the frame rate, whose choices follow the
 * screen's full rate ([fullFrameRate], [com.pd2launcher.app.ScreenRate]); and the lower frame rate in the background; next Play), App (the app's
 * own, [app], and the system settings it depends on; at once) and Controller (the physical
 * controller, [ControlsStore], and Touch, [app]; at once, in the game too). Each tab opens with when its settings
 * apply; while the game or the official launcher runs on another X screen than the game
 * resolution makes ([running]: its size), the game resolution also says that the new size applies
 * at the next start. Controller leads with On-screen controls, the way to the on-screen controls
 * ([onEditOnScreen]: their editor holds all their options); a binding opens the picker as a
 * sheet over the column, as the game resolution opens its choices. [back]: the back label, to where
 * Settings returns ([com.pd2launcher.app.Onboarding.settingsBack]). [updates]: this build gets app
 * updates (the App tab's App updates: the hourly check, and [newVersion] when one is out, which
 * About installs).
 */
@Composable
fun SettingsScreen(
    launcher: LauncherSettings.View?,
    app: AppSettings,
    display: Pair<Int, Int>,
    fullFrameRate: Float,
    gpu: HostGpu?,
    system: SystemState,
    nav: SettingsNav,
    @StringRes back: Int,
    onCommand: (Command) -> Unit,
    onAppChange: ((AppSettings) -> AppSettings) -> Unit,
    onBatterySettings: () -> Unit,
    onNotificationSettings: () -> Unit,
    onEditOnScreen: () -> Unit,
    onAbout: () -> Unit,
    onBack: () -> Unit,
    updates: Boolean = false,
    newVersion: String? = null,
    running: Pair<Int, Int>? = null,
) {
    val context = LocalContext.current
    remember { ControlsStore.load(context) }
    val controls by ControlsStore.current.collectAsState()
    val save: (Controls) -> Unit = { ControlsStore.save(context, it) }
    val phone = LocalFormFactor.current == FormFactor.PHONE
    val tabFocus = remember { FocusRequester() }
    InitialFocus(tabFocus, nav.tab)
    BackHandler(
        enabled = nav.picker == null && !nav.resolutions && !nav.customSize && !nav.displays && !nav.profiles &&
            !nav.frameRates,
        onBack = onBack,
    )
    Box(Modifier.fillMaxSize().background(Palette.Background)) {
        Column(Modifier.fillMaxSize().windowInsetsPadding(WindowInsets.safeDrawing)) {
            Row(Modifier.fillMaxWidth().heightIn(min = 56.dp).padding(start = 8.dp, end = 20.dp), verticalAlignment = Alignment.CenterVertically) {
                LauncherButton(
                    stringResource(back), onBack,
                    kind = Kind.QUIET, height = 48.dp, padding = 14.dp,
                )
                ScreenTitle(stringResource(R.string.settings_title), Modifier.weight(1f).padding(start = 4.dp))
                SaveStatus(LauncherSettings.saveState(launcher))
            }
            Row(Modifier.fillMaxWidth().padding(horizontal = 12.dp), horizontalArrangement = Arrangement.Center) {
                listOf(R.string.settings_tab_game, R.string.settings_tab_app, R.string.settings_tab_controller).forEachIndexed { i, label ->
                    TextTab(
                        stringResource(label), nav.tab == i, { nav.open(i) },
                        if (phone) Modifier.weight(1f) else Modifier, if (nav.tab == i) tabFocus else null,
                    )
                }
            }
            Rule()
            Column(Modifier.weight(1f).fillMaxWidth().verticalScroll(nav.scroll[nav.tab]), horizontalAlignment = Alignment.CenterHorizontally) {
                Column(Modifier.widthIn(max = 560.dp).fillMaxWidth().padding(horizontal = 8.dp).padding(top = 12.dp, bottom = 24.dp)) {
                    when (nav.tab) {
                        SettingsNav.GAME -> GameTab(launcher, onCommand, app, display, fullFrameRate, gpu, nav, onAppChange, running)
                        SettingsNav.APP -> AppTab(app, system, onAppChange, onBatterySettings, onNotificationSettings, onAbout, updates, newVersion)
                        else -> ControllerTab(controls, app, nav, phone, save, onAppChange, onEditOnScreen)
                    }
                }
            }
        }
        nav.picker?.let { button -> BindingPicker(controls, button, nav.mouse, side = !phone, save) { nav.picker = null } }
        if (nav.resolutions) ResolutionSheet(app, display, phone, onAppChange, { nav.customSize = true }) { nav.resolutions = false }
        if (nav.customSize) CustomSizeInput(app.customSize, display, { value -> onAppChange { it.copy(customSize = value) } }) { nav.customSize = false }
        if (nav.displays) DisplaySheet(app, phone, onAppChange) { nav.displays = false }
        if (nav.profiles) GraphicsProfileSheet(app, gpu, phone, onAppChange) { nav.profiles = false }
        if (nav.frameRates) FrameRateInput(app.frameRate, fullFrameRate, { value -> onAppChange { it.copy(frameRate = value) } }) { nav.frameRates = false }
    }
}

@Composable
private fun SaveStatus(state: LauncherSettings.SaveState) {
    val (color, label) = when (state) {
        LauncherSettings.SaveState.SAVED -> Palette.Faint to R.string.settings_saved
        LauncherSettings.SaveState.WAITING -> Palette.Amber to R.string.settings_waiting
        LauncherSettings.SaveState.FAILED -> Palette.Red to R.string.settings_not_saved
    }
    Row(verticalAlignment = Alignment.CenterVertically) {
        Dot(color)
        Spacer(Modifier.width(8.dp))
        Text(stringResource(label), style = Type.Hint, color = if (state == LauncherSettings.SaveState.FAILED) Palette.Red else Palette.Muted, maxLines = 1)
    }
}

/** A text tab with a teal underline under the open one. */
@Composable
private fun TextTab(label: String, selected: Boolean, onClick: () -> Unit, modifier: Modifier = Modifier, focus: FocusRequester? = null) {
    val interaction = remember { MutableInteractionSource() }
    val focused by interaction.collectIsFocusedAsState()
    Box(
        modifier.focusOn(focus)
            .heightIn(min = 52.dp).focusMark(focused, shape = Small)
            .selectable(selected, interaction, LocalIndication.current, role = Role.Tab, onClick = onClick)
            .padding(horizontal = 18.dp),
        contentAlignment = Alignment.Center,
    ) {
        Text(label, style = Type.Button, color = if (selected) Palette.Text else Palette.Muted, maxLines = 1)
        if (selected) Box(Modifier.align(Alignment.BottomCenter).padding(bottom = 6.dp).width(24.dp).height(2.dp).background(Palette.Teal))
    }
}

/** When this tab's settings take effect: the one truth players need before changing anything. */
@Composable
private fun Applies(text: String) {
    Text(text, style = Type.Hint, color = Palette.Muted, modifier = Modifier.padding(horizontal = 14.dp, vertical = 4.dp))
}

/** A section: a teal overline with more room above than below; its rows ruled, no card. */
@Composable
private fun Section(title: String, content: @Composable ColumnScope.() -> Unit) {
    GroupLabel(title, Modifier.padding(start = 14.dp, top = 8.dp))
    Column(content = content)
}

@Composable
private fun RowRule() = Rule(Modifier.padding(horizontal = 14.dp))

// ---------------------------------------------------------------- Game and App

@Composable
private fun GameTab(
    launcher: LauncherSettings.View?,
    onCommand: (Command) -> Unit,
    app: AppSettings,
    display: Pair<Int, Int>,
    fullFrameRate: Float,
    gpu: HostGpu?,
    nav: SettingsNav,
    onAppChange: ((AppSettings) -> AppSettings) -> Unit,
    running: Pair<Int, Int>?,
) {
    val enabled = launcher?.editable == true
    val failure = launcher?.failure
    when {
        failure != null -> Banner(stringResource(R.string.settings_hint_failed, failure), Palette.Red, Modifier.padding(horizontal = 6.dp, vertical = 4.dp))
        !enabled -> Banner(stringResource(R.string.settings_hint_waiting), Palette.Amber, Modifier.padding(horizontal = 6.dp, vertical = 4.dp))
        else -> Applies(stringResource(R.string.settings_applies_game))
    }
    val renderer = launcher?.renderer ?: Renderer.GLIDE
    Section(stringResource(R.string.settings_section_renderer)) {
        OptionRow(
            stringResource(R.string.settings_renderer_glide), stringResource(R.string.settings_renderer_glide_body),
            renderer == Renderer.GLIDE, { if (renderer != Renderer.GLIDE) onCommand(LauncherSettings.renderer(Renderer.GLIDE)) }, enabled,
        )
        RowRule()
        OptionRow(
            stringResource(R.string.settings_renderer_direct_draw), stringResource(R.string.settings_renderer_direct_draw_body),
            renderer == Renderer.DIRECT_DRAW, { if (renderer != Renderer.DIRECT_DRAW) onCommand(LauncherSettings.renderer(Renderer.DIRECT_DRAW)) }, enabled,
        )
    }
    ResolutionSection(app, display, running) { nav.resolutions = true }
    DisplaySection(app) { nav.displays = true }
    // The settings that control the game itself: D2GL's graphics, its frame rate, how it starts, its sound.
    Section(stringResource(R.string.settings_section_start)) {
        GraphicsProfileRow(app, gpu) { nav.profiles = true }
        RowRule()
        FrameRateRow(app, fullFrameRate) { nav.frameRates = true }
        RowRule()
        ToggleRow(
            stringResource(R.string.settings_background_frame_rate),
            stringResource(R.string.settings_background_frame_rate_body),
            app.lowFrameRateInBackground, { value -> onAppChange { it.copy(lowFrameRateInBackground = value) } },
        )
        RowRule()
        ToggleRow(
            stringResource(R.string.settings_skip_to_bnet), stringResource(R.string.settings_skip_to_bnet_body),
            launcher?.skipToBnet ?: false, { onCommand(LauncherSettings.skipToBnet(it)) }, enabled,
        )
        RowRule()
        ToggleRow(
            stringResource(R.string.settings_sound_in_background), stringResource(R.string.settings_sound_in_background_body),
            launcher?.soundInBackground ?: false, { onCommand(LauncherSettings.soundInBackground(it)) }, enabled,
        )
    }
    // The Launcher's checks for Project Diablo 2's updates (the app's own updates are under App);
    // only from an engine that has the setting.
    launcher?.gameUpdateChecks?.let { checks ->
        Section(stringResource(R.string.settings_section_game_updates)) {
            ToggleRow(
                stringResource(R.string.settings_game_update_checks), stringResource(R.string.settings_game_update_checks_body),
                checks, { onCommand(LauncherSettings.gameUpdateChecks(it)) }, enabled,
            )
        }
    }
}

/**
 * The app's own setting beside the Launcher's game settings: D2GL's graphics profile the next Play
 * sends ([GraphicsProfiles.shown]: Automatic says what it picks on this GPU), its choices in a sheet.
 */
@Composable
private fun GraphicsProfileRow(app: AppSettings, gpu: HostGpu?, onOpen: () -> Unit) {
    FocusRow(onOpen, minHeight = 60.dp) {
        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
            Text(stringResource(R.string.settings_graphics_profile), style = Type.Body, color = Palette.Text)
            Text(text(GraphicsProfiles.shown(app.graphicsProfile, gpu)), style = Type.Hint, color = Palette.Teal)
            Text(stringResource(R.string.settings_graphics_profile_body), style = Type.Hint, color = Palette.Muted)
        }
        Text("›", style = Type.Title, color = Palette.Teal)
    }
}

/** D2GL's graphics profiles, in the game resolution's sheet ([GraphicsProfiles.choices]). A pick closes it. */
@Composable
private fun BoxScope.GraphicsProfileSheet(
    app: AppSettings,
    gpu: HostGpu?,
    phone: Boolean,
    onAppChange: ((AppSettings) -> AppSettings) -> Unit,
    onClose: () -> Unit,
) {
    ChoiceSheet(stringResource(R.string.settings_graphics_profile), phone, onClose) { first ->
        GraphicsProfiles.choices(gpu).forEachIndexed { index, choice ->
            if (index > 0) RowRule()
            OptionRow(
                text(choice.title), text(choice.body), app.graphicsProfile == choice.profile,
                {
                    onAppChange { it.copy(graphicsProfile = choice.profile) }
                    onClose()
                },
                focusRequester = first.takeIf { index == 0 },
            )
        }
    }
}

/**
 * The game's frame rate on the screen, as the player reads it, in Settings and the in-game menu:
 * always a number, the player's rate where it applies, else the screen's [full] rate ("120 fps"
 * for the stored Full, [FrameRateVote.shown], [com.pd2launcher.app.ScreenRate]); "– fps" when no
 * display mode could be read.
 */
@Composable
internal fun frameRateLabel(pick: Int, full: Float): String {
    val shown = FrameRateVote.shown(pick, full)
    return if (shown > 0) stringResource(R.string.game_fps, shown) else stringResource(R.string.game_fps_unknown)
}

/** The app's own setting beside the graphics profile: the game's frame rate on the screen; a tap opens its input ([FrameRateInput]). */
@Composable
private fun FrameRateRow(app: AppSettings, full: Float, onOpen: () -> Unit) {
    FocusRow(onOpen, minHeight = 60.dp) {
        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
            Text(stringResource(R.string.settings_frame_rate), style = Type.Body, color = Palette.Text)
            Text(frameRateLabel(app.frameRate, full), style = Type.Hint, color = Palette.Teal)
            Text(stringResource(R.string.settings_frame_rate_body), style = Type.Hint, color = Palette.Muted)
        }
        Text("›", style = Type.Title, color = Palette.Teal)
    }
}

/**
 * The app's own setting: the size the next Play and the next official launcher use on the
 * landscape [display], its choices in a sheet; that the official launcher's window is cut off at
 * a size with fewer columns or lines than it ([GameResolution.LAUNCHER_WINDOW],
 * [GameResolution.cutsLauncher]); while a game or the launcher runs at another size ([running]: its
 * X screen), that the new size applies at the next start.
 */
@Composable
private fun ResolutionSection(app: AppSettings, display: Pair<Int, Int>, running: Pair<Int, Int>?, onOpen: () -> Unit) {
    val (width, height) = display
    val next = GameResolution.forPlay(width, height, app.customSize)
    Section(stringResource(R.string.settings_section_resolution)) {
        FocusRow(onOpen) {
            Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
                Text(text(GameResolution.label(width, height, app.customSize)), style = Type.Body, color = Palette.Text)
                Text(stringResource(R.string.settings_resolution_body), style = Type.Hint, color = Palette.Muted)
                if (GameResolution.cutsLauncher(next)) {
                    Text(stringResource(R.string.settings_resolution_launcher_cut), style = Type.Hint, color = Palette.Amber)
                }
                if (running != null && running != next) {
                    Text(stringResource(R.string.settings_resolution_running), style = Type.Hint, color = Palette.Amber)
                }
            }
            Text("›", style = Type.Title, color = Palette.Teal)
        }
    }
}

/**
 * The app's own setting: how the Game view shows the game and the official launcher's window
 * ([GameDisplay]), its choices in a sheet; a change applies at once, also to a running game (it
 * shows when the game shows again), with no restart. The in-game menu offers it too.
 */
@Composable
private fun DisplaySection(app: AppSettings, onOpen: () -> Unit) {
    Section(stringResource(R.string.settings_section_display)) {
        FocusRow(onOpen) {
            Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
                Text(stringResource(app.gameDisplay.title), style = Type.Body, color = Palette.Text)
                Text(stringResource(R.string.settings_display_body), style = Type.Hint, color = Palette.Muted)
            }
            Text("›", style = Type.Title, color = Palette.Teal)
        }
    }
}

/** The Game view's ways to show the game ([GameDisplay]), as the binding picker's sheet. A pick closes it. */
@Composable
private fun BoxScope.DisplaySheet(
    app: AppSettings,
    phone: Boolean,
    onAppChange: ((AppSettings) -> AppSettings) -> Unit,
    onClose: () -> Unit,
) {
    ChoiceSheet(stringResource(R.string.settings_section_display), phone, onClose) { first ->
        GameDisplay.entries.forEachIndexed { index, choice ->
            if (index > 0) RowRule()
            OptionRow(
                stringResource(choice.title), stringResource(choice.body), app.gameDisplay == choice,
                {
                    onAppChange { it.copy(gameDisplay = choice) }
                    onClose()
                },
                focusRequester = first.takeIf { index == 0 },
            )
        }
    }
}

/** A sheet of choices like the binding picker's: [title], Done, and [content] scrolling; [content] gets the first row's focus. */
@Composable
private fun BoxScope.ChoiceSheet(title: String, phone: Boolean, onClose: () -> Unit, content: @Composable ColumnScope.(FocusRequester) -> Unit) {
    BackHandler(onBack = onClose)
    Box(Modifier.matchParentSize().background(Color.Black.copy(alpha = 0.5f)).clickable(remember { MutableInteractionSource() }, null, onClick = onClose))
    val sheet = if (phone) {
        Modifier.align(Alignment.BottomCenter).fillMaxWidth().fillMaxHeight(0.72f)
    } else {
        Modifier.align(Alignment.CenterEnd).width(340.dp).fillMaxHeight()
    }
    val first = remember { FocusRequester() }
    LaunchedEffect(Unit) { runCatching { first.requestFocus() } }
    Column(sheet.windowInsetsPadding(WindowInsets.safeDrawing).padding(8.dp).clip(RowShape).background(Palette.Surface).border(1.dp, Palette.Rule, RowShape)) {
        Row(Modifier.fillMaxWidth().heightIn(min = 56.dp).padding(start = 16.dp, end = 4.dp), verticalAlignment = Alignment.CenterVertically) {
            Text(title, style = Type.Body, color = Palette.Text, modifier = Modifier.weight(1f))
            LauncherButton(stringResource(R.string.controls_picker_done), onClose, kind = Kind.QUIET, height = 48.dp, padding = 14.dp)
        }
        Rule()
        Column(Modifier.weight(1f).verticalScroll(rememberScrollState()).padding(6.dp)) { content(first) }
    }
}

/**
 * The game resolution's choices on the landscape [display], as the binding picker's sheet: the
 * game's own size (the default; the display's full size where it is smaller,
 * [GameResolution.ownSize]), then Custom… where the display offers it
 * ([GameResolution.customOffered]), which opens its input ([onCustom], [CustomSizeInput]). A pick
 * closes it.
 */
@Composable
private fun BoxScope.ResolutionSheet(
    app: AppSettings,
    display: Pair<Int, Int>,
    phone: Boolean,
    onAppChange: ((AppSettings) -> AppSettings) -> Unit,
    onCustom: () -> Unit,
    onClose: () -> Unit,
) {
    val (width, height) = display
    val custom = GameResolution.allowedCustom(width, height, app.customSize)
    ChoiceSheet(stringResource(R.string.settings_section_resolution), phone, onClose) { first ->
        OptionRow(
            text(GameResolution.label(width, height, null)),
            stringResource(
                if (GameResolution.holdsOwnSize(width, height)) R.string.settings_resolution_game_size_body else R.string.settings_resolution_full_body,
            ),
            custom == null,
            {
                onAppChange { it.copy(customSize = null) }
                onClose()
            },
            focusRequester = first,
        )
        if (GameResolution.customOffered(width, height)) {
            RowRule()
            OptionRow(
                if (custom != null) text(GameResolution.label(width, height, custom)) else stringResource(R.string.settings_resolution_custom_choice),
                stringResource(R.string.settings_resolution_custom_body),
                custom != null,
                {
                    onCustom()
                    onClose()
                },
            )
        }
    }
}

@Composable
private fun AppTab(
    app: AppSettings,
    system: SystemState,
    onAppChange: ((AppSettings) -> AppSettings) -> Unit,
    onBatterySettings: () -> Unit,
    onNotificationSettings: () -> Unit,
    onAbout: () -> Unit,
    updates: Boolean,
    newVersion: String?,
) {
    Applies(stringResource(R.string.settings_applies_app))
    Section(stringResource(R.string.settings_section_screen)) {
        ToggleRow(
            stringResource(R.string.settings_keep_screen_on), stringResource(R.string.settings_keep_screen_on_body),
            app.keepScreenOn, { value -> onAppChange { it.copy(keepScreenOn = value) } },
        )
        RowRule()
        ToggleRow(
            stringResource(R.string.settings_show_fps), stringResource(R.string.settings_show_fps_body),
            app.showFps, { value -> onAppChange { it.copy(showFps = value) } },
        )
    }
    // Where the game folder is (mods, loot filters), then Save sync for the characters.
    Section(stringResource(R.string.settings_section_game_files)) {
        GameFilesRows()
        RowRule()
        SaveSyncRows(app)
    }
    Section(stringResource(R.string.settings_section_storage)) { StorageRows() }
    Section(stringResource(R.string.settings_section_android)) {
        SystemRow(
            stringResource(R.string.settings_battery), !system.batteryRestricted,
            stringResource(if (system.batteryRestricted) R.string.settings_battery_restricted else R.string.settings_battery_unrestricted),
            stringResource(R.string.settings_battery_open), onBatterySettings,
        )
        RowRule()
        SystemRow(
            stringResource(R.string.settings_notifications), !system.notificationsOff,
            stringResource(if (system.notificationsOff) R.string.settings_notifications_off else R.string.settings_notifications_on),
            stringResource(R.string.settings_notifications_open), onNotificationSettings,
        )
    }
    Section(stringResource(R.string.settings_section_help)) {
        ShareLogsRow(LogShareUi.Action.SHARE)
        RowRule()
        ShareLogsRow(LogShareUi.Action.SAVE)
        RowRule()
        ToggleRow(
            stringResource(R.string.settings_detailed_logging), stringResource(R.string.settings_detailed_logging_body),
            app.detailedLogging, { value -> onAppChange { it.copy(detailedLogging = value) } },
        )
    }
    // The app's own updates: the hourly check, and a new version's way to About, which installs it.
    if (updates) Section(stringResource(R.string.app_update_title)) {
        ToggleRow(
            stringResource(R.string.app_update_auto), stringResource(R.string.app_update_auto_body),
            app.updateChecks, { value -> onAppChange { it.copy(updateChecks = value) } },
        )
        if (newVersion != null) {
            RowRule()
            FocusRow(onAbout) {
                Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
                    Text(stringResource(R.string.app_update_new_version, newVersion), style = Type.Body, color = Palette.Teal)
                    Text(stringResource(R.string.app_update_new_version_body), style = Type.Hint, color = Palette.Muted)
                }
                Text("›", style = Type.Title, color = Palette.Teal)
            }
        }
    }
    Section(stringResource(R.string.settings_section_about)) {
        FocusRow(onAbout) {
            Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
                Text(stringResource(R.string.settings_about), style = Type.Body, color = Palette.Text)
                Text(stringResource(R.string.settings_about_body), style = Type.Hint, color = Palette.Muted)
            }
            Text("›", style = Type.Title, color = Palette.Teal)
        }
    }
}

/** An Android permission state with its way out: the system screen that changes it (below the words on phones). */
@Composable
private fun SystemRow(title: String, ok: Boolean, status: String, action: String, onOpen: () -> Unit) {
    val phone = LocalFormFactor.current == FormFactor.PHONE
    val button = @Composable { modifier: Modifier ->
        LauncherButton(action, onOpen, modifier, height = 48.dp, padding = 16.dp, style = Type.Hint.copy(fontWeight = FontWeight.SemiBold))
    }
    Row(
        Modifier.fillMaxWidth().heightIn(min = 64.dp).padding(horizontal = 14.dp, vertical = 10.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(14.dp),
    ) {
        StatusMark(if (ok) Tone.OK else Tone.WARNING)
        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
            Text(title, style = Type.Body, color = Palette.Text)
            Text(status, style = Type.Hint, color = Palette.Muted)
            if (phone) button(Modifier.padding(top = 8.dp))
        }
        if (!phone) button(Modifier)
    }
}

// ---------------------------------------------------------------- Controller

@Composable
private fun ControllerTab(
    controls: Controls,
    app: AppSettings,
    nav: SettingsNav,
    phone: Boolean,
    save: (Controls) -> Unit,
    onAppChange: ((AppSettings) -> AppSettings) -> Unit,
    onEditOnScreen: () -> Unit,
) {
    EditOnScreenRow(onEditOnScreen)
    Applies(stringResource(R.string.settings_applies_controller))
    Section(stringResource(R.string.controls_section_controller)) {
        Column(Modifier.fillMaxWidth().padding(horizontal = 14.dp, vertical = 8.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
            Text(stringResource(R.string.controls_chord), style = Type.Body, color = Palette.Text)
            Text(stringResource(R.string.controls_chord_body), style = Type.Hint, color = Palette.Muted)
            Segmented(
                listOf(stringResource(R.string.controls_chord_select_start), stringResource(R.string.controls_chord_thumbs)),
                controls.chord.ordinal, { save(controls.copy(chord = Chord.entries[it])) }, Modifier.fillMaxWidth(),
            )
        }
        RowRule()
        PointerSpeedRow(controls, save)
        RowRule()
        // As the in-game Controls screen's, at this tab's row height and room.
        TouchRow(app.touchMode, { mode -> onAppChange { it.copy(touchMode = mode) } }, Modifier.heightIn(min = 64.dp).padding(horizontal = 14.dp, vertical = 6.dp))
    }
    Section(stringResource(R.string.controls_section_buttons)) {
        ModeSwitch(nav)
        val chord = setOf(controls.chord.first, controls.chord.second)
        PadButton.entries.chunked(if (phone) 1 else 2).forEach { row ->
            Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(4.dp)) {
                row.forEach { button ->
                    BindingCell(button, controls.action(button, nav.mouse), controls.isDefault(button, nav.mouse), button in chord, { nav.picker = button }, Modifier.weight(1f))
                }
            }
        }
        ResetRow(nav.mouse) { save(controls.resetMode(nav.mouse)) }
    }
}

/** On-screen controls: their editor, which holds every option they have (the in-game Controls screen opens it too). */
@Composable
private fun EditOnScreenRow(onClick: () -> Unit) {
    FocusRow(
        onClick,
        Modifier.padding(start = 6.dp, end = 6.dp, top = 4.dp, bottom = 8.dp).clip(RowShape).background(Palette.Surface).border(1.dp, Palette.Rule, RowShape),
        minHeight = 72.dp,
    ) {
        Column(Modifier.weight(1f).padding(vertical = 6.dp), verticalArrangement = Arrangement.spacedBy(2.dp)) {
            Text(stringResource(R.string.controls_edit_on_screen), style = Type.Body.copy(fontWeight = FontWeight.SemiBold), color = Palette.Text)
            Text(stringResource(R.string.controls_edit_on_screen_body), style = Type.Hint, color = Palette.Muted)
        }
        Text("›", style = Type.Title, color = Palette.Teal)
    }
}

/** The pointer speed of mouse mode and Trackpad; the in-game Controls screen changes it too ([PointerSpeedInput]). */
@Composable
private fun PointerSpeedRow(controls: Controls, save: (Controls) -> Unit) {
    StepperRow(
        stringResource(R.string.controls_pointer_speed_title), stringResource(R.string.controls_factor, speedText(controls.pointerSpeed)),
        Controls.fraction(controls.pointerSpeed, PointerSpeed.RANGE),
        { save(controls.copy(pointerSpeed = Controls.stepped(controls.pointerSpeed, PointerSpeed.STEP, -1, PointerSpeed.RANGE))) },
        { save(controls.copy(pointerSpeed = Controls.stepped(controls.pointerSpeed, PointerSpeed.STEP, 1, PointerSpeed.RANGE))) },
        supporting = stringResource(R.string.controls_pointer_speed_body),
    )
}

/** The mode switch over the bindings, with what the mode means. */
@Composable
private fun ModeSwitch(nav: SettingsNav) {
    Column(Modifier.fillMaxWidth().padding(horizontal = 14.dp, vertical = 8.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
        Segmented(
            listOf(stringResource(R.string.controls_pad_mode), stringResource(R.string.controls_mouse_mode)),
            if (nav.mouse) 1 else 0, { nav.mouse = it == 1 }, Modifier.fillMaxWidth(),
        )
        Text(
            stringResource(if (nav.mouse) R.string.controls_mouse_mode_body else R.string.controls_pad_mode_body),
            style = Type.Hint, color = Palette.Muted,
        )
    }
}

/** One binding: the button's cap and what it does; defaults muted so changed ones stand out. A chord button says so. */
@Composable
private fun BindingCell(button: PadButton, action: PadAction, isDefault: Boolean, chord: Boolean, onClick: () -> Unit, modifier: Modifier) {
    FocusRow(onClick, modifier, minHeight = 52.dp) {
        KeyCap(buttonCap(button), Modifier.width(60.dp))
        Column(Modifier.weight(1f)) {
            Text(actionLine(action), style = Type.Body, color = if (isDefault) Palette.Muted else Palette.Text, maxLines = 1, overflow = TextOverflow.Ellipsis)
            if (chord) Text(stringResource(R.string.controls_chord_button), style = Type.Hint, color = Palette.Muted, maxLines = 2)
        }
    }
}

/** Reset with an in-place confirmation that names what it resets. */
@Composable
private fun ResetRow(mouse: Boolean, onReset: () -> Unit) {
    var confirm by remember { mutableStateOf(false) }
    val mode = stringResource(if (mouse) R.string.controls_mouse_mode else R.string.controls_pad_mode)
    Row(
        Modifier.fillMaxWidth().padding(horizontal = 6.dp, vertical = 6.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        if (!confirm) {
            Spacer(Modifier.weight(1f))
            LauncherButton(stringResource(R.string.controls_reset, mode), { confirm = true }, kind = Kind.QUIET, height = 48.dp, padding = 14.dp, style = Type.Hint)
        } else {
            Text(stringResource(R.string.controls_reset_confirm, PadButton.entries.size, mode), style = Type.Hint, color = Palette.Text, modifier = Modifier.weight(1f).padding(start = 8.dp))
            LauncherButton(stringResource(R.string.controls_reset_cancel), { confirm = false }, height = 48.dp, padding = 16.dp)
            LauncherButton(stringResource(R.string.controls_reset_do), { onReset(); confirm = false }, kind = Kind.DANGER, height = 48.dp, padding = 16.dp)
        }
    }
}
