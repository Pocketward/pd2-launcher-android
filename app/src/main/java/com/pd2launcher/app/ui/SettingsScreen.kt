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
    var launcherResolutions by mutableStateOf(false)
    var profiles by mutableStateOf(false)
    val scroll = List(3) { ScrollState(0) }

    fun open(tab: Int) {
        this.tab = tab
        picker = null
        resolutions = false
        launcherResolutions = false
        profiles = false
    }

    companion object {
        const val GAME = 0
        const val APP = 1
        const val CONTROLLER = 2
    }
}

/**
 * Settings, in three text tabs: Game (the Launcher's settings that apply on Android, [launcher],
 * changed with [onCommand], and the game resolution and D2GL's graphics profile, whose Automatic
 * follows the [gpu], the resolution's choices following the landscape [display], and the limit to
 * 60 frames a second on a screen whose [highestFrameRate] is higher; next Play), App (the app's
 * own, [app], and the system settings it depends on; at once) and Controller (the physical
 * controller, [ControlsStore]; at once, in the game too). Each tab opens with when its settings
 * apply. Controller leads with Edit on-screen controls, the one way to the on-screen controls
 * ([onEditOnScreen]: their editor holds all their options); a binding opens the picker as a
 * sheet over the column, as the game resolution opens its choices. [back]: the back label, to where
 * Settings returns ([com.pd2launcher.app.Onboarding.settingsBack]). [updates]: this build gets app
 * updates (the App tab's App updates: the daily check, and [newVersion] when one is out, which
 * About installs).
 */
@Composable
fun SettingsScreen(
    launcher: LauncherSettings.View?,
    app: AppSettings,
    display: Pair<Int, Int>,
    highestFrameRate: Float,
    gpu: HostGpu?,
    system: SystemState,
    nav: SettingsNav,
    @StringRes back: Int,
    onCommand: (Command) -> Unit,
    onAppChange: (AppSettings) -> Unit,
    onBatterySettings: () -> Unit,
    onNotificationSettings: () -> Unit,
    onEditOnScreen: () -> Unit,
    onAbout: () -> Unit,
    onBack: () -> Unit,
    updates: Boolean = false,
    newVersion: String? = null,
) {
    val context = LocalContext.current
    remember { ControlsStore.load(context) }
    val controls by ControlsStore.current.collectAsState()
    val save: (Controls) -> Unit = { ControlsStore.save(context, it) }
    val phone = LocalFormFactor.current == FormFactor.PHONE
    val tabFocus = remember { FocusRequester() }
    InitialFocus(tabFocus, nav.tab)
    BackHandler(enabled = nav.picker == null && !nav.resolutions && !nav.launcherResolutions && !nav.profiles, onBack = onBack)
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
                        SettingsNav.GAME -> GameTab(launcher, onCommand, app, display, highestFrameRate, gpu, nav, onAppChange)
                        SettingsNav.APP -> AppTab(app, system, onAppChange, onBatterySettings, onNotificationSettings, onAbout, updates, newVersion)
                        else -> ControllerTab(controls, nav, phone, save, onEditOnScreen)
                    }
                }
            }
        }
        nav.picker?.let { button -> PickerSheet(controls, button, nav, phone, save) }
        if (nav.resolutions) ResolutionSheet(app, display, gpu, phone, onAppChange) { nav.resolutions = false }
        if (nav.launcherResolutions) LauncherResolutionSheet(app, display, phone, onAppChange) { nav.launcherResolutions = false }
        if (nav.profiles) GraphicsProfileSheet(app, gpu, phone, onAppChange) { nav.profiles = false }
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
    highestFrameRate: Float,
    gpu: HostGpu?,
    nav: SettingsNav,
    onAppChange: (AppSettings) -> Unit,
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
    ResolutionSection(app, display, gpu) { nav.resolutions = true }
    LauncherResolutionSection(app, display) { nav.launcherResolutions = true }
    // The settings that control the game itself: D2GL's graphics, how it starts, its sound.
    Section(stringResource(R.string.settings_section_start)) {
        GraphicsProfileRow(app, gpu) { nav.profiles = true }
        // Only on a screen faster than 60 Hz, where the game otherwise runs at the screen's rate.
        if (FrameRateVote.limitApplies(highestFrameRate)) {
            RowRule()
            ToggleRow(
                stringResource(R.string.settings_limit_frame_rate),
                stringResource(R.string.settings_limit_frame_rate_body, Math.round(highestFrameRate)),
                app.limitFrameRate, { onAppChange(app.copy(limitFrameRate = it)) },
            )
        }
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
    onAppChange: (AppSettings) -> Unit,
    onClose: () -> Unit,
) {
    ChoiceSheet(stringResource(R.string.settings_graphics_profile), phone, onClose) { first ->
        GraphicsProfiles.choices(gpu).forEachIndexed { index, choice ->
            if (index > 0) RowRule()
            OptionRow(
                text(choice.title), text(choice.body), app.graphicsProfile == choice.profile,
                {
                    onAppChange(app.copy(graphicsProfile = choice.profile))
                    onClose()
                },
                focusRequester = first.takeIf { index == 0 },
            )
        }
    }
}

/** A size as the player reads it, "1600 × 720", with "(full)" for the display's own. */
@Composable
private fun resolutionLabel(size: Pair<Int, Int>, display: Pair<Int, Int>): String =
    stringResource(if (size == display) R.string.settings_resolution_full else R.string.settings_resolution_size, size.first, size.second)

/** The app's own setting: the size the next Play uses, its choices in a sheet. */
@Composable
private fun ResolutionSection(app: AppSettings, display: Pair<Int, Int>, gpu: HostGpu?, onOpen: () -> Unit) {
    Section(stringResource(R.string.settings_section_resolution)) {
        val size = GameResolution.forPlay(display.first, display.second, app.gameResolution, gpu)
        val label = resolutionLabel(size, display)
        FocusRow(onOpen) {
            Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
                Text(
                    if (app.gameResolution == null) stringResource(R.string.settings_resolution_automatic, label) else label,
                    style = Type.Body, color = Palette.Text,
                )
                Text(stringResource(R.string.settings_resolution_body), style = Type.Hint, color = Palette.Muted)
            }
            Text("›", style = Type.Title, color = Palette.Teal)
        }
    }
}

/** The app's own setting: the size the official PD2 launcher opens at, its choices in a sheet. */
@Composable
private fun LauncherResolutionSection(app: AppSettings, display: Pair<Int, Int>, onOpen: () -> Unit) {
    Section(stringResource(R.string.settings_section_launcher_resolution)) {
        FocusRow(onOpen) {
            Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
                Text(
                    resolutionLabel(GameResolution.forLauncher(display.first, display.second, app.launcherResolution), display),
                    style = Type.Body, color = Palette.Text,
                )
                Text(stringResource(R.string.settings_launcher_resolution_body), style = Type.Hint, color = Palette.Muted)
            }
            Text("›", style = Type.Title, color = Palette.Teal)
        }
    }
}

/** The official PD2 launcher's sizes, in the game resolution's sheet: the display's full size (the default) down to 480 lines. */
@Composable
private fun BoxScope.LauncherResolutionSheet(
    app: AppSettings,
    display: Pair<Int, Int>,
    phone: Boolean,
    onAppChange: (AppSettings) -> Unit,
    onClose: () -> Unit,
) {
    val (width, height) = display
    val current = GameResolution.forLauncher(width, height, app.launcherResolution)
    ChoiceSheet(stringResource(R.string.settings_section_launcher_resolution), phone, onClose) { first ->
        GameResolution.choices(width, height).forEachIndexed { index, size ->
            if (index > 0) RowRule()
            OptionRow(
                resolutionLabel(size, display), null, current == size,
                {
                    // The full size is the default: kept as none, so it follows the display.
                    onAppChange(app.copy(launcherResolution = size.second.takeIf { it < height }))
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
 * The game resolution's choices, as the binding picker's sheet: Automatic (what it picks on this
 * GPU), then the display's full size down to 480 lines ([GameResolution.choices]). A pick closes it.
 */
@Composable
private fun BoxScope.ResolutionSheet(
    app: AppSettings,
    display: Pair<Int, Int>,
    gpu: HostGpu?,
    phone: Boolean,
    onAppChange: (AppSettings) -> Unit,
    onClose: () -> Unit,
) {
    val (width, height) = display
    ChoiceSheet(stringResource(R.string.settings_section_resolution), phone, onClose) { first ->
            val pick: (Int?) -> Unit = {
                onAppChange(app.copy(gameResolution = it))
                onClose()
            }
            val automatic = GameResolution.forPlay(width, height, null, gpu)
            OptionRow(
                stringResource(R.string.settings_resolution_automatic, resolutionLabel(automatic, display)),
                stringResource(R.string.settings_resolution_automatic_body),
                app.gameResolution == null, { pick(null) },
                focusRequester = first,
            )
            GameResolution.choices(width, height).forEach { size ->
                RowRule()
                OptionRow(resolutionLabel(size, display), null, app.gameResolution == size.second, { pick(size.second) })
            }
    }
}

@Composable
private fun AppTab(
    app: AppSettings,
    system: SystemState,
    onAppChange: (AppSettings) -> Unit,
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
            app.keepScreenOn, { onAppChange(app.copy(keepScreenOn = it)) },
        )
        RowRule()
        ToggleRow(
            stringResource(R.string.settings_show_fps), stringResource(R.string.settings_show_fps_body),
            app.showFps, { onAppChange(app.copy(showFps = it)) },
        )
    }
    Section(stringResource(R.string.settings_section_game_files)) { GameFilesRows() }
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
    }
    // The app's own updates: the daily check, and a new version's way to About, which installs it.
    if (updates) Section(stringResource(R.string.app_update_title)) {
        ToggleRow(
            stringResource(R.string.app_update_auto), stringResource(R.string.app_update_auto_body),
            app.updateChecks, { onAppChange(app.copy(updateChecks = it)) },
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
private fun ControllerTab(controls: Controls, nav: SettingsNav, phone: Boolean, save: (Controls) -> Unit, onEditOnScreen: () -> Unit) {
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
        PointerSpeed(controls, save)
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

/** The one way to the on-screen controls: their editor, which holds every option they have. */
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

/** Mouse mode's pointer speed; also in the game's Controls panel. */
@Composable
internal fun PointerSpeed(controls: Controls, save: (Controls) -> Unit) {
    StepperRow(
        stringResource(R.string.controls_pointer_speed), stringResource(R.string.controls_factor, controls.pointerSpeed),
        Controls.fraction(controls.pointerSpeed, Controls.POINTER_SPEEDS),
        { save(controls.copy(pointerSpeed = Controls.stepped(controls.pointerSpeed, Controls.POINTER_SPEED_STEP, -1, Controls.POINTER_SPEEDS))) },
        { save(controls.copy(pointerSpeed = Controls.stepped(controls.pointerSpeed, Controls.POINTER_SPEED_STEP, 1, Controls.POINTER_SPEEDS))) },
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

/** The binding picker: a sheet from the right (handhelds) or the bottom (phones), the column dimmed. */
@Composable
private fun BoxScope.PickerSheet(controls: Controls, button: PadButton, nav: SettingsNav, phone: Boolean, save: (Controls) -> Unit) {
    BackHandler { nav.picker = null }
    Box(Modifier.matchParentSize().background(Color.Black.copy(alpha = 0.5f)).clickable(remember { MutableInteractionSource() }, null) { nav.picker = null })
    val sheet = if (phone) {
        Modifier.align(Alignment.BottomCenter).fillMaxWidth().fillMaxHeight(0.72f)
    } else {
        Modifier.align(Alignment.CenterEnd).width(340.dp).fillMaxHeight()
    }
    val current = controls.action(button, nav.mouse)
    val first = remember { FocusRequester() }
    LaunchedEffect(button, nav.mouse) { runCatching { first.requestFocus() } }
    Column(sheet.windowInsetsPadding(WindowInsets.safeDrawing).padding(8.dp).clip(RowShape).background(Palette.Surface).border(1.dp, Palette.Rule, RowShape)) {
        Row(Modifier.fillMaxWidth().heightIn(min = 56.dp).padding(start = 16.dp, end = 4.dp), verticalAlignment = Alignment.CenterVertically) {
            KeyCap(buttonCap(button))
            Column(Modifier.weight(1f).padding(start = 12.dp)) {
                Text(button.label, style = Type.Body, color = Palette.Text)
                Text(stringResource(if (nav.mouse) R.string.controls_mouse_mode else R.string.controls_pad_mode), style = Type.Hint, color = Palette.Muted)
            }
            LauncherButton(stringResource(R.string.controls_picker_done), { nav.picker = null }, kind = Kind.QUIET, height = 48.dp, padding = 14.dp)
        }
        Rule()
        Column(Modifier.weight(1f).verticalScroll(rememberScrollState()).padding(6.dp)) {
            val general = generalChoices(nav.mouse)
            val pick: (PadAction) -> Unit = {
                save(controls.bind(button, nav.mouse, it))
                nav.picker = null
            }
            val focusOn = current.takeIf { it in general || it in keyChoices } ?: general.first()
            ChoiceGroup(stringResource(R.string.controls_picker_general), general, current, focusOn, first, pick)
            ChoiceGroup(stringResource(R.string.controls_picker_keys), keyChoices, current, focusOn, first, pick)
        }
    }
}

@Composable
internal fun ChoiceGroup(title: String, choices: List<PadAction>, current: PadAction, focusOn: PadAction, first: FocusRequester, onPick: (PadAction) -> Unit) {
    GroupLabel(title, Modifier.padding(start = 10.dp))
    choices.chunked(2).forEach { row ->
        Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(4.dp)) {
            row.forEach { action -> ChoiceCell(action, action == current, { onPick(action) }, Modifier.weight(1f), if (action == focusOn) first else null) }
            if (row.size == 1) Spacer(Modifier.weight(1f))
        }
    }
}

@Composable
private fun ChoiceCell(action: PadAction, selected: Boolean, onClick: () -> Unit, modifier: Modifier, focus: FocusRequester?) {
    val interaction = remember { MutableInteractionSource() }
    val focused by interaction.collectIsFocusedAsState()
    Row(
        modifier.focusOn(focus)
            .heightIn(min = 48.dp).focusMark(focused, selected)
            .selectable(selected, interaction, LocalIndication.current, role = Role.RadioButton, onClick = onClick)
            .padding(horizontal = 10.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        // Every row keeps the same cap column, so the names line up whether or not it has a cap.
        Box(Modifier.width(CHOICE_CAP), contentAlignment = Alignment.Center) {
            choiceCap(action)?.let { KeyCap(it, Modifier.fillMaxWidth(), selected = selected) }
        }
        Text(actionName(action), style = Type.Hint, color = if (selected) Palette.Teal else Palette.Text, maxLines = 2)
    }
}

/** The width of a choice row's key cap, enough for "Space" or "Enter". */
private val CHOICE_CAP = 56.dp
