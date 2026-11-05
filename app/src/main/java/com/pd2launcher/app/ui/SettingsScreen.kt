package com.pd2launcher.app.ui

import androidx.activity.compose.BackHandler
import androidx.annotation.StringRes
import androidx.compose.foundation.LocalIndication
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.focusGroup
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
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.runtime.snapshotFlow
import androidx.compose.runtime.withFrameNanos
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.focus.FocusDirection
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusProperties
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.focus.onFocusChanged
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.input.InputMode
import androidx.compose.ui.input.key.Key
import androidx.compose.ui.input.key.KeyEventType
import androidx.compose.ui.input.key.key
import androidx.compose.ui.input.key.onKeyEvent
import androidx.compose.ui.input.key.type
import androidx.compose.ui.layout.LayoutCoordinates
import androidx.compose.ui.layout.onPlaced
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalFocusManager
import androidx.compose.ui.platform.LocalInputModeManager
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import com.pd2launcher.app.AppSettings
import com.pd2launcher.app.BuildConfig
import com.pd2launcher.app.DriverPage
import com.pd2launcher.app.FrameRateVote
import com.pd2launcher.app.GameDisplay
import com.pd2launcher.app.GameResolution
import com.pd2launcher.app.GraphicsProfiles
import com.pd2launcher.app.HostGpu
import com.pd2launcher.app.LauncherSettings
import com.pd2launcher.app.PlayerDriver
import com.pd2launcher.app.Line
import com.pd2launcher.app.R
import com.pd2launcher.app.SaveSyncFolder
import com.pd2launcher.app.SaveSyncState
import com.pd2launcher.app.Tone
import com.pd2launcher.app.control.Command
import com.pd2launcher.app.input.Chord
import com.pd2launcher.app.input.Controls
import com.pd2launcher.app.input.ControlsStore
import com.pd2launcher.app.input.TouchMode
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch
import kotlinx.coroutines.withTimeoutOrNull

/** What the Android group says about the system settings the app depends on. */
data class SystemState(val batteryRestricted: Boolean, val notificationsOff: Boolean)

/** The list's width on a handheld, beside the open page. */
private val LIST_WIDTH = 240.dp

/** The longest wait on the way back from a screen for the page to reach its old scroll: the system bars' return. */
private const val SCROLL_BACK_MS = 500L

/**
 * Settings, as a list of pages ([SettingsPage]: Display, Game, Controls, Files and saves, App,
 * Help), each in the list with its values now ([SettingsPage.line]) and a mark where it needs the
 * player. A handheld shows two panes, the list at the left and the open page at the
 * right; the d-pad moves between them. A phone shows the list, then the page with "‹ Settings";
 * Back goes from the page to the list. L1 and R1 step through the pages ([SettingsNav.shoulder],
 * from MainActivity). Rows are short: a row that opens a sheet or an input shows its title and
 * its value, and the sheet or the input says the rest. Each row says when it applies only where
 * that is not at once.
 *
 * Display: the game resolution, also the official launcher's, whose choices follow the landscape
 * [display] (while the game or the official launcher runs at another size, [running]: its X
 * screen, the row says that the new size applies at the next start); how the Game view shows
 * them (Display, at once, also over a running game); D2GL's graphics profile, whose Automatic
 * follows the [gpu]; the frame rate, whose choices follow the screen's full rate
 * ([fullFrameRate], [com.pd2launcher.app.ScreenRate]); Slower game in the background, the FPS
 * counter and Keep the screen on. Game: the Launcher's settings ([launcher], changed with
 * [onCommand]; its save state beside the page's title): the check for game updates, then the
 * start options that Play gives Diablo II; Loot filters keeps its place, greyed out. Controls:
 * On-screen controls ([onEditOnScreen]: their editor holds all their options), Buttons
 * ([onButtons]: the Controls screen of the game, which holds every binding), Touch, Pointer speed
 * (its input, [PointerSpeedInput]) and the chord. Files and saves: the game folder, Save sync and
 * Storage. App ([appPage]; null: this build gets no updates): the version, Check now and the
 * Update channel, and About ([onAbout]). Help: the problem reports (Detailed logging and Save
 * logs) and Android's battery and notification settings ([system]). Display's GPU row and sub
 * page show the route that the next Play takes ([gpuInUse], [DriverPage.onRoute]), and the
 * Renderer row the driver's renderer. [back]: the
 * back label, to where Settings returns ([com.pd2launcher.app.Onboarding.settingsBack]).
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
    onButtons: () -> Unit,
    onAbout: () -> Unit,
    onBack: () -> Unit,
    appPage: AppPageUi? = null,
    running: Pair<Int, Int>? = null,
    gpuInUse: DriverPage.InUse? = null,
) {
    // Display's GPU sub page over Settings, as About and Buttons: on the way back Settings is new,
    // and the focus returns to the GPU row (nav.opener).
    if (nav.gpu) {
        GpuPage(app, gpu, onAppChange, gpuInUse) { nav.gpu = false }
        return
    }
    val context = LocalContext.current
    remember { ControlsStore.load(context) }
    val controls by ControlsStore.current.collectAsState()
    val save: (Controls) -> Unit = { ControlsStore.save(context, it) }
    remember { SaveSyncFolder.load(context) }
    val sync by SaveSyncFolder.state.collectAsState()
    val phone = LocalFormFactor.current == FormFactor.PHONE
    val (width, height) = display
    val size = GameResolution.forPlay(width, height, app.customSize)
    val facts = SettingsFacts(
        size = size,
        frameRate = FrameRateVote.shown(app.frameRate, fullFrameRate),
        launcher = launcher,
        touch = app.touchMode,
        pointerSpeed = controls.pointerSpeed,
        syncOn = app.syncFolder != null,
        syncBroken = sync.problem == SaveSyncState.Problem.FOLDER_GONE || sync.problem == SaveSyncState.Problem.FAILED,
        version = BuildConfig.SHOWN_VERSION,
        appLine = appPage?.line,
        system = system,
    )
    val list = remember { SettingsPage.entries.associateWith { FocusRequester() } }
    val page = remember { FocusRequester() }
    val rows = remember { PageRows() }
    val input = LocalInputModeManager.current
    // As Settings opens: the list's row of the open page; a phone's list or page as each shows. As
    // InitialFocus, except that the first key after a touch on a scrolled page goes to a row in view,
    // so the page stays where the player put it: the first row or the list would lose that.
    var fromTouch by remember { mutableStateOf(false) }
    val start = if (phone && nav.pageShown) page else list.getValue(nav.page)
    LaunchedEffect(input.inputMode, phone && nav.pageShown) {
        val stays = fromTouch && input.inputMode == InputMode.Keyboard && (!phone || nav.pageShown) && !nav.popup &&
            nav.scroll.getValue(nav.page).value > 0 && rows.focusInView()
        fromTouch = input.inputMode == InputMode.Touch
        if (!stays) runCatching { start.requestFocus() }
    }
    // A closed sheet, and Buttons, About or the on-screen controls on the way back: the row that
    // opened it, which is in view, so the page does not scroll. On the way back Settings is new:
    // the frames that its first layout needs, as a row with no place yet can scroll the page. The
    // scroll comes back too: the Controls screen and the editor hide the system bars, and until they
    // show again a higher page can scroll less, which cuts the kept value. The page gets its height
    // back in a few frames, or not at all when the page changed.
    LaunchedEffect(nav.popup) {
        val row = nav.opener.takeUnless { nav.popup } ?: return@LaunchedEffect
        nav.opener = null
        if (rows.view == null) repeat(2) { withFrameNanos {} }
        val scroll = nav.scroll.getValue(nav.page)
        if (scroll.value != nav.openerScroll) {
            withTimeoutOrNull(SCROLL_BACK_MS) { snapshotFlow { scroll.maxValue }.first { it >= nav.openerScroll } }
            scroll.scrollTo(nav.openerScroll)
        }
        runCatching { rows.focus.getValue(row).requestFocus() }
    }
    // L1 and R1: the focus goes to the first row of the page they open, by its own requester. The
    // keys do not end Android's touch mode, in which no row takes the focus. After the frames that
    // the new mode and the new page need: the first effect above also answers a new mode.
    val seenSteps = remember { nav.focusPage }
    LaunchedEffect(nav.focusPage) {
        if (nav.focusPage != seenSteps) {
            input.requestInputMode(InputMode.Keyboard)
            repeat(2) { withFrameNanos {} }
            runCatching { page.requestFocus() }
        }
    }
    val updates = appPage != null
    // The rows go from under the focus: another page by any way but the list, or the App page's
    // updates. The focus then falls out of the page, so it goes to the first row, as for L1 and R1.
    var inPage by remember { mutableStateOf(false) }
    val rowsNow = nav.page to (nav.page == SettingsPage.APP && updates)
    val lost = remember(rowsNow) { inPage }
    LaunchedEffect(rowsNow) {
        if (lost) {
            repeat(2) { withFrameNanos {} }
            runCatching { page.requestFocus() }
        }
    }
    BackHandler(enabled = !nav.popup) { if (!nav.back(phone)) onBack() }
    val enabled = launcher?.editable == true
    val shown = { r: SettingsRow -> r.shown(gameUpdateChecks = launcher?.gameUpdateChecks != null, updates = updates) }
    val firstRow = nav.page.firstRow(shown, enabled)
    val focusOf = { r: SettingsRow -> page.takeIf { r == firstRow } }
    val opens = { r: SettingsRow, open: () -> Unit ->
        nav.opener = r
        nav.openerScroll = nav.scroll.getValue(nav.page).value
        open()
    }
    val rowView: @Composable ColumnScope.(SettingsRow) -> Unit = { row ->
        when (row) {
            SettingsRow.GAME_RESOLUTION -> OpenRow(
                stringResource(R.string.settings_section_resolution), text(GameResolution.label(width, height, app.customSize)),
                notes = listOfNotNull(
                    // The official launcher's window is cut off at a size with fewer columns or lines than it.
                    stringResource(R.string.settings_resolution_launcher_cut).takeIf { GameResolution.cutsLauncher(size) },
                    stringResource(R.string.settings_resolution_running).takeIf { running != null && running != size },
                ),
                focusRequester = focusOf(row),
            ) { opens(row) { nav.resolutions = true } }
            SettingsRow.DISPLAY -> OpenRow(stringResource(R.string.settings_section_display), stringResource(app.gameDisplay.title)) { opens(row) { nav.displays = true } }
            SettingsRow.GRAPHICS_PROFILE -> OpenRow(stringResource(R.string.settings_graphics_profile), text(GraphicsProfiles.shown(app.graphicsProfile, gpu))) {
                opens(row) { nav.profiles = true }
            }
            SettingsRow.FRAME_RATE -> OpenRow(stringResource(R.string.settings_frame_rate), frameRateLabel(app.frameRate, fullFrameRate)) { opens(row) { nav.frameRates = true } }
            SettingsRow.FPS_COUNTER -> ToggleRow(
                stringResource(R.string.game_menu_fps_counter), stringResource(R.string.settings_fps_counter_body),
                app.showFps, { value -> onAppChange { it.copy(showFps = value) } },
            )
            SettingsRow.KEEP_SCREEN_ON -> ToggleRow(
                stringResource(R.string.settings_keep_screen_on), stringResource(R.string.settings_keep_screen_on_body),
                app.keepScreenOn, { value -> onAppChange { it.copy(keepScreenOn = value) } },
            )
            SettingsRow.SKIP_TO_BNET -> ToggleRow(
                stringResource(R.string.settings_skip_to_bnet), stringResource(R.string.settings_skip_to_bnet_body),
                launcher?.skipToBnet ?: false, { onCommand(LauncherSettings.skipToBnet(it)) }, enabled, focusOf(row),
            )
            // The Launcher's checks for Project Diablo 2's updates (the app's own are on the App page).
            SettingsRow.GAME_UPDATE_CHECKS -> ToggleRow(
                stringResource(R.string.settings_game_update_checks), stringResource(R.string.settings_game_update_checks_body),
                launcher?.gameUpdateChecks == true, { onCommand(LauncherSettings.gameUpdateChecks(it)) }, enabled, focusOf(row),
            )
            SettingsRow.SLOWER_IN_BACKGROUND -> ToggleRow(
                stringResource(R.string.settings_background_frame_rate), stringResource(R.string.settings_background_frame_rate_body),
                app.lowFrameRateInBackground, { value -> onAppChange { it.copy(lowFrameRateInBackground = value) } },
            )
            SettingsRow.SOUND_IN_BACKGROUND -> ToggleRow(
                stringResource(R.string.settings_sound_in_background), stringResource(R.string.settings_sound_in_background_body),
                launcher?.soundInBackground ?: false, { onCommand(LauncherSettings.soundInBackground(it)) }, enabled, focusOf(row),
            )
            SettingsRow.RENDERER -> RendererInfoRow(software = gpuInUse?.driver == PlayerDriver.SOFTWARE)
            SettingsRow.LOOT_FILTERS -> LootFiltersRow()
            SettingsRow.ON_SCREEN_CONTROLS -> OpenRow(
                stringResource(R.string.controls_edit_on_screen), body = stringResource(R.string.controls_edit_on_screen_body),
                minHeight = 56.dp, focusRequester = focusOf(row),
            ) { opens(row, onEditOnScreen) }
            SettingsRow.BUTTONS -> OpenRow(
                stringResource(R.string.controls_section_buttons), body = stringResource(R.string.settings_buttons_body), minHeight = 56.dp,
            ) { opens(row, onButtons) }
            SettingsRow.CHORD -> ChoiceRow(
                stringResource(R.string.controls_chord),
                listOf(stringResource(R.string.controls_chord_select_start), stringResource(R.string.controls_chord_thumbs)),
                controls.chord.ordinal, { save(controls.copy(chord = Chord.entries[it])) }, stringResource(R.string.controls_chord_body),
            )
            SettingsRow.POINTER_SPEED -> ValueRow(
                stringResource(R.string.controls_pointer_speed_title), stringResource(R.string.controls_factor, speedText(controls.pointerSpeed)),
            ) { opens(row) { nav.pointerSpeed = true } }
            SettingsRow.TOUCH -> ChoiceRow(
                stringResource(R.string.controls_touch),
                listOf(stringResource(R.string.controls_touch_trackpad), stringResource(R.string.controls_touch_direct)),
                TouchMode.entries.indexOf(app.touchMode), { index -> onAppChange { it.copy(touchMode = TouchMode.entries[index]) } },
            )
            SettingsRow.GAME_FOLDER -> GameFilesRows(focusOf(row))
            SettingsRow.SAVE_SYNC -> SaveSyncRows(app)
            SettingsRow.STORAGE -> StorageRows()
            SettingsRow.APP_STATUS -> appPage?.let { AppStatusBlock(it, list.getValue(nav.page).takeUnless { phone }, page) }
            SettingsRow.UPDATE_CHANNEL -> appPage?.let { ChannelRows(it, focusOf(row)) }
            SettingsRow.DETAILED_LOGGING -> ToggleRow(
                stringResource(R.string.settings_detailed_logging), stringResource(R.string.settings_detailed_logging_body),
                app.detailedLogging, { value -> onAppChange { it.copy(detailedLogging = value) } }, focusRequester = focusOf(row), prominent = true,
            )
            SettingsRow.SAVE_LOGS -> SaveLogsRow()
            SettingsRow.BATTERY -> SystemRow(
                stringResource(R.string.settings_battery), !system.batteryRestricted,
                stringResource(if (system.batteryRestricted) R.string.settings_battery_restricted else R.string.settings_battery_unrestricted),
                stringResource(R.string.settings_battery_open), onBatterySettings,
            )
            SettingsRow.NOTIFICATIONS -> SystemRow(
                stringResource(R.string.settings_notifications), !system.notificationsOff,
                stringResource(if (system.notificationsOff) R.string.settings_notifications_off else R.string.settings_notifications_on),
                stringResource(R.string.settings_notifications_open), onNotificationSettings,
            )
            // Display's GPU: the driver in use, and its sub page (decision 23).
            SettingsRow.GPU -> OpenRow(
                stringResource(R.string.settings_gpu), gpuValue(driverPage(app, gpu, gpuInUse).first), focusRequester = focusOf(row),
            ) { opens(row) { nav.gpu = true } }
            SettingsRow.ABOUT -> OpenRow(
                stringResource(R.string.settings_about), stringResource(R.string.about_version, BuildConfig.SHOWN_VERSION), minHeight = 56.dp,
                focusRequester = focusOf(row),
            ) { opens(row, onAbout) }
        }
    }
    val saveState = LauncherSettings.saveState(launcher).takeIf { nav.page == SettingsPage.GAME }
    Box(Modifier.fillMaxSize().background(Palette.Background)) {
        if (phone) {
            Column(Modifier.fillMaxSize().windowInsetsPadding(WindowInsets.safeDrawing)) {
                if (nav.pageShown) {
                    if (nav.fromPlay) {
                        Header(stringResource(back), onBack, stringResource(nav.page.title), saveState)
                    } else {
                        Header(stringResource(R.string.back_to_settings), { nav.back(phone = true) }, stringResource(nav.page.title), saveState)
                    }
                    Rule()
                    PageBody(nav, launcher, updates, null, rows, { inPage = it }, shown, rowView, Modifier.weight(1f))
                } else {
                    Header(stringResource(back), onBack, stringResource(R.string.settings_title), null)
                    Rule()
                    PageList(nav, facts, phone = true, list, null, Modifier.weight(1f).fillMaxWidth())
                }
            }
        } else {
            Row(Modifier.fillMaxSize().windowInsetsPadding(WindowInsets.safeDrawing)) {
                Column(Modifier.width(LIST_WIDTH).fillMaxHeight()) {
                    Header(stringResource(back), onBack, stringResource(R.string.settings_title), null)
                    PageList(nav, facts, phone = false, list, page, Modifier.weight(1f).fillMaxWidth())
                }
                Rule(Modifier.fillMaxHeight().padding(vertical = 12.dp), vertical = true)
                Column(Modifier.weight(1f).fillMaxHeight()) {
                    Header(null, {}, stringResource(nav.page.title), saveState, Type.PageTitle)
                    PageBody(nav, launcher, updates, list.getValue(nav.page), rows, { inPage = it }, shown, rowView, Modifier.weight(1f))
                }
            }
        }
        if (nav.resolutions) ResolutionSheet(app, display, phone, onAppChange, { nav.customSize = true }) { nav.resolutions = false }
        if (nav.customSize) CustomSizeInput(app.customSize, display, { value -> onAppChange { it.copy(customSize = value) } }) { nav.customSize = false }
        if (nav.displays) DisplaySheet(app, phone, onAppChange) { nav.displays = false }
        if (nav.profiles) GraphicsProfileSheet(app, gpu, phone, onAppChange) { nav.profiles = false }
        if (nav.frameRates) {
            FrameRateInput(
                app.frameRate, fullFrameRate, { value -> onAppChange { it.copy(frameRate = value) } },
                intro = stringResource(R.string.settings_frame_rate_body),
            ) { nav.frameRates = false }
        }
        if (nav.pointerSpeed) PointerSpeedInput(controls.pointerSpeed, { save(controls.copy(pointerSpeed = it)) }) { nav.pointerSpeed = false }
    }
}

/**
 * A header: the back button ([back]; null: none, the page's header beside the list), the [title],
 * and the Launcher's [save] state where the page holds its settings (Game).
 */
@Composable
private fun Header(back: String?, onBack: () -> Unit, title: String, save: LauncherSettings.SaveState?, style: TextStyle = Type.Title) {
    Row(Modifier.fillMaxWidth().heightIn(min = 56.dp).padding(start = 8.dp, end = 20.dp), verticalAlignment = Alignment.CenterVertically) {
        if (back != null) LauncherButton(back, onBack, kind = Kind.QUIET, height = 48.dp, padding = 14.dp)
        ScreenTitle(title, Modifier.weight(1f).padding(start = if (back != null) 4.dp else 14.dp), style)
        if (save != null) SaveStatus(save)
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

/**
 * The list of pages: each with its icon, its name and its line ([SettingsPage.line]), the open
 * one tinted on a handheld; on a [phone] a chevron, and a press opens the page. A handheld's d-pad
 * goes right into the open page ([page]).
 */
@Composable
private fun PageList(
    nav: SettingsNav,
    facts: SettingsFacts,
    phone: Boolean,
    focus: Map<SettingsPage, FocusRequester>,
    page: FocusRequester?,
    modifier: Modifier,
) {
    Column(modifier.verticalScroll(nav.listScroll).padding(horizontal = 8.dp, vertical = 4.dp)) {
        SettingsPage.entries.forEach { entry ->
            PageRow(entry, entry.line(facts), phone, selected = !phone && nav.page == entry, focus.getValue(entry), page) { nav.open(entry) }
        }
    }
}

/**
 * One page in the list, at least 56 dp high: its icon, its name, and its line joined with " · ",
 * in the alert's colour with its mark where the page needs the player, or the amber dot of a new
 * version ([PageLine.news]).
 */
@Composable
private fun PageRow(page: SettingsPage, line: PageLine, phone: Boolean, selected: Boolean, focus: FocusRequester, right: FocusRequester?, onOpen: () -> Unit) {
    val interaction = remember { MutableInteractionSource() }
    val focused by interaction.collectIsFocusedAsState()
    val press = if (phone) {
        Modifier.clickable(interaction, LocalIndication.current, role = Role.Button, onClick = onOpen)
    } else {
        Modifier.selectable(selected, interaction, LocalIndication.current, role = Role.Tab, onClick = onOpen)
    }
    Row(
        Modifier.focusRequester(focus)
            .then(if (right != null) Modifier.focusProperties { this.right = right } else Modifier)
            .fillMaxWidth().heightIn(min = 56.dp).focusMark(focused, selected)
            .then(press)
            .padding(start = 14.dp, end = 12.dp, top = 6.dp, bottom = 6.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(14.dp),
    ) {
        Icon(painterResource(page.icon), null, Modifier.size(22.dp), tint = if (selected) Palette.Teal else Palette.Muted)
        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(1.dp)) {
            Text(
                stringResource(page.title), style = Type.Body.copy(fontWeight = if (selected) FontWeight.SemiBold else null),
                color = if (selected) Palette.Teal else Palette.Text,
            )
            // One line, so the six pages fit a short screen also at a large font. An alert's small mark, or a new
            // version's amber dot, sits beside the words.
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                line.alert?.let { StatusMark(it) } ?: if (line.news) Dot(Palette.Amber) else Unit
                Text(
                    joined(line.lines), style = Type.Hint, color = line.alert?.let(::toneColor) ?: Palette.Muted,
                    maxLines = 1, overflow = TextOverflow.Ellipsis,
                )
            }
        }
        if (phone) Text("›", style = Type.Title, color = Palette.Teal)
    }
}

/** [lines] as one text: "1068 × 600 · 60 fps". */
@Composable
private fun joined(lines: List<Line>): String {
    val parts = lines.map { text(it) }
    return parts.drop(1).fold(parts.first()) { start, next -> stringResource(R.string.settings_list_join, start, next) }
}

/**
 * The open page: on Game the Launcher's save problem or wait first, on the App page in a build
 * that gets none a line that says so, then each group with its overline and its rows that show
 * ([shown]). Its rows are one focus group, which says whether it holds the focus ([onFocus]); a
 * handheld's d-pad left out of it goes to the list's row of the page ([back]). Down never leaves
 * the group: with no control below, it scrolls to the page's end, which can hold what takes no
 * focus (Loot filters, the chord's hint). [rows] holds each row's focus and place.
 */
@Composable
private fun PageBody(
    nav: SettingsNav,
    launcher: LauncherSettings.View?,
    updates: Boolean,
    back: FocusRequester?,
    rows: PageRows,
    onFocus: (Boolean) -> Unit,
    shown: (SettingsRow) -> Boolean,
    row: @Composable ColumnScope.(SettingsRow) -> Unit,
    modifier: Modifier,
) {
    val page = nav.page
    val phone = LocalFormFactor.current == FormFactor.PHONE
    val focus = LocalFocusManager.current
    val scope = rememberCoroutineScope()
    val scroll = nav.scroll.getValue(page)
    Column(modifier.fillMaxWidth().onPlaced { rows.view = it }.verticalScroll(scroll), horizontalAlignment = if (phone) Alignment.CenterHorizontally else Alignment.Start) {
        Column(
            Modifier.widthIn(max = if (phone) 560.dp else 520.dp).fillMaxWidth().padding(horizontal = 8.dp).padding(top = if (phone) 12.dp else 0.dp, bottom = 24.dp)
                .focusProperties {
                    onExit = {
                        when (requestedFocusDirection) {
                            FocusDirection.Down -> cancelFocusChange()
                            FocusDirection.Left -> back?.requestFocus()
                            else -> Unit
                        }
                    }
                }
                .onKeyEvent { event ->
                    if (event.type != KeyEventType.KeyDown || event.key != Key.DirectionDown) return@onKeyEvent false
                    // False: no control below, or only one outside the page.
                    if (!focus.moveFocus(FocusDirection.Down)) scope.launch { scroll.animateScrollTo(scroll.maxValue) }
                    true
                }
                .onFocusChanged { onFocus(it.hasFocus) }
                .focusGroup(),
        ) {
            if (page == SettingsPage.GAME) {
                val failure = launcher?.failure
                when {
                    failure != null -> Banner(stringResource(R.string.settings_hint_failed, failure), Palette.Red, Modifier.padding(horizontal = 6.dp, vertical = 4.dp))
                    launcher?.editable != true -> Banner(stringResource(R.string.settings_hint_waiting), Palette.Amber, Modifier.padding(horizontal = 6.dp, vertical = 4.dp))
                }
            }
            if (page == SettingsPage.APP && !updates) {
                Text(stringResource(R.string.settings_updates_none), style = Type.Hint, color = Palette.Muted, modifier = Modifier.padding(horizontal = 14.dp, vertical = 12.dp))
            }
            var first = true
            page.groups.forEach { group ->
                val inGroup = group.rows.filter(shown)
                if (inGroup.isNotEmpty()) {
                    Section(group, first) {
                        inGroup.forEachIndexed { index, each ->
                            if (index > 0 && group.ruled) RowRule()
                            Column(Modifier.focusRequester(rows.focus.getValue(each)).onPlaced { rows.places[each] = it }) { row(each) }
                        }
                    }
                    first = false
                }
            }
        }
    }
}

/**
 * The open page's rows: each row's [focus], which goes to the first control in it that takes the
 * focus, its place in the page ([places]), and the page's part of the screen ([view]).
 */
private class PageRows {
    val focus = SettingsRow.entries.associateWith { FocusRequester() }
    val places = HashMap<SettingsRow, LayoutCoordinates>()
    var view: LayoutCoordinates? = null

    /** The focus goes to a row in view ([rowsInView]). False: none takes it. */
    fun focusInView(): Boolean {
        val page = view?.takeIf { it.isAttached } ?: return false
        val placed = places.entries.filter { it.value.isAttached }
        val spans = placed.map { (_, place) -> page.localBoundingBoxOf(place, clipBounds = false).let { it.top..it.bottom } }
        return rowsInView(spans, page.size.height.toFloat()).any { index ->
            runCatching { focus.getValue(placed[index].key).requestFocus() }.getOrDefault(false)
        }
    }
}

/**
 * A group: a teal overline ([RowGroup.label]; none for null) with more room above than below,
 * less at the top of the page ([first]), the line that says what the group is, and its rows, no
 * card. A closing group ([RowGroup.gap]) starts with a wider gap and a rule.
 */
@Composable
private fun Section(group: RowGroup, first: Boolean, content: @Composable ColumnScope.() -> Unit) {
    if (group.gap) {
        Spacer(Modifier.height(20.dp))
        RowRule()
    }
    group.label?.let { GroupLabel(stringResource(it), Modifier.padding(start = 14.dp), top = if (first) 8.dp else 24.dp) }
    group.hint?.let {
        Text(stringResource(it), style = Type.Hint, color = Palette.Muted, modifier = Modifier.padding(start = 14.dp, end = 14.dp, bottom = 4.dp))
    }
    Column(content = content)
}

@Composable
private fun RowRule() = Rule(Modifier.padding(horizontal = 14.dp))

/**
 * A row that opens a sheet, an input or a screen: its title, then its value in teal ([value]) or a
 * quiet line ([body]), the amber [notes] under them, and a chevron. What the setting does is on
 * the sheet or the input.
 */
@Composable
private fun OpenRow(
    title: String,
    value: String? = null,
    body: String? = null,
    notes: List<String> = emptyList(),
    minHeight: Dp = 60.dp,
    focusRequester: FocusRequester? = null,
    onOpen: () -> Unit,
) {
    FocusRow(onOpen, focusRequester = focusRequester, minHeight = minHeight) {
        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
            Text(title, style = Type.Body, color = Palette.Text)
            if (value != null) Text(value, style = Type.Hint, color = Palette.Teal)
            if (body != null) Text(body, style = Type.Hint, color = Palette.Muted)
            notes.forEach { Text(it, style = Type.Hint, color = Palette.Amber) }
        }
        Text("›", style = Type.Title, color = Palette.Teal)
    }
}

/** A row of one line that opens an input: its title, its value in teal at the right, and a chevron. */
@Composable
private fun ValueRow(title: String, value: String, onOpen: () -> Unit) {
    FocusRow(onOpen) {
        Text(title, style = Type.Body, color = Palette.Text, modifier = Modifier.weight(1f))
        Text(value, style = Type.Hint, color = Palette.Teal, maxLines = 1)
        Text("›", style = Type.Title, color = Palette.Teal)
    }
}

/** One line per setting: its title and its slim switch at the right, and the [hint] under them. */
@Composable
private fun ChoiceRow(title: String, options: List<String>, selected: Int, onSelect: (Int) -> Unit, hint: String? = null) {
    Column(Modifier.fillMaxWidth().heightIn(min = 56.dp).padding(horizontal = 14.dp, vertical = 4.dp), verticalArrangement = Arrangement.Center) {
        Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(12.dp)) {
            Text(title, style = Type.Body, color = Palette.Text, modifier = Modifier.weight(1f))
            SlimSwitch(options, selected, onSelect)
        }
        if (hint != null) Text(hint, style = Type.Hint, color = Palette.Muted)
    }
}

/**
 * Display → Renderer, information only (decision 24, the owner, 2026-11-03: "renderers are now tied
 * to the GPU options"): information only, the first line white as other titles, the second greyed;
 * no focus, no sheet. Every Play from the app runs the driver's renderer
 * ([com.pd2launcher.app.GlideFirst]): Glide, and DirectDraw on the Software driver ([software]).
 */
@Composable
private fun RendererInfoRow(software: Boolean) {
    Column(
        Modifier.fillMaxWidth().heightIn(min = 56.dp).padding(horizontal = 14.dp, vertical = 10.dp),
        verticalArrangement = Arrangement.spacedBy(2.dp),
    ) {
        // Two lines (the owner, 2026-11-03): "Renderer: Glide", white as the other rows' titles ("white like the rest
        // menu items for consistency"), then who sets it, greyed.
        val renderer = if (software) R.string.settings_renderer_direct_draw else R.string.settings_renderer_glide
        Text(stringResource(R.string.settings_renderer_value, stringResource(renderer)), style = Type.Body, color = Palette.Text)
        Text(stringResource(R.string.settings_renderer_set_by_gpu), style = Type.Hint, color = Palette.Faint)
    }
}

/** Loot filters: greyed out, as the place of a later screen. */
@Composable
private fun LootFiltersRow() {
    Row(
        Modifier.fillMaxWidth().heightIn(min = 56.dp).padding(horizontal = 14.dp, vertical = 10.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(14.dp),
    ) {
        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
            Text(stringResource(R.string.settings_loot_filters), style = Type.Body, color = Palette.Faint)
            Text(stringResource(R.string.settings_loot_filters_body), style = Type.Hint, color = Palette.Faint)
        }
        SlimButton(stringResource(R.string.settings_loot_filters_manage), {}, Modifier.focusProperties { canFocus = false }, enabled = false)
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

/** D2GL's graphics profiles ([GraphicsProfiles.choices]) under what a profile changes. A pick closes it. */
@Composable
private fun BoxScope.GraphicsProfileSheet(
    app: AppSettings,
    gpu: HostGpu?,
    phone: Boolean,
    onAppChange: ((AppSettings) -> AppSettings) -> Unit,
    onClose: () -> Unit,
) {
    ChoiceSheet(stringResource(R.string.settings_graphics_profile), phone, onClose, stringResource(R.string.settings_graphics_profile_body)) { first ->
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

/** The Game view's ways to show the game ([GameDisplay]) under what Display does. A pick closes it. */
@Composable
private fun BoxScope.DisplaySheet(
    app: AppSettings,
    phone: Boolean,
    onAppChange: ((AppSettings) -> AppSettings) -> Unit,
    onClose: () -> Unit,
) {
    ChoiceSheet(stringResource(R.string.settings_section_display), phone, onClose, stringResource(R.string.settings_display_body)) { first ->
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

/**
 * A sheet of choices: [title], Done, then what the setting does ([intro]) and [content], which
 * scroll; [content] gets the first row's focus. From the right on a handheld, from the bottom on
 * a [phone], the rest dimmed.
 */
@Composable
private fun BoxScope.ChoiceSheet(title: String, phone: Boolean, onClose: () -> Unit, intro: String, content: @Composable ColumnScope.(FocusRequester) -> Unit) {
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
        Column(Modifier.weight(1f).verticalScroll(rememberScrollState()).padding(6.dp)) {
            Text(intro, style = Type.Hint, color = Palette.Muted, modifier = Modifier.padding(start = 14.dp, end = 14.dp, top = 8.dp, bottom = 6.dp))
            content(first)
        }
    }
}

/**
 * The game resolution's choices on the landscape [display], under what the size does: the
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
    ChoiceSheet(stringResource(R.string.settings_section_resolution), phone, onClose, stringResource(R.string.settings_resolution_body)) { first ->
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

/**
 * An Android permission state with its way out: the system screen that changes it (below the words
 * on phones). Its state is a small dot in the status line (the size of Settings' save-state dot,
 * [Dot]), amber with amber words when Android stops something, not a large mark (the owner,
 * 2026-11-03: "we don't have to use such huge yellow warning icons").
 */
@Composable
private fun SystemRow(title: String, ok: Boolean, status: String, action: String, onOpen: () -> Unit) {
    val phone = LocalFormFactor.current == FormFactor.PHONE
    val button = @Composable { modifier: Modifier -> SlimButton(action, onOpen, modifier) }
    Row(
        Modifier.fillMaxWidth().heightIn(min = 56.dp).padding(horizontal = 14.dp, vertical = 10.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(14.dp),
    ) {
        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
            Text(title, style = Type.Body, color = Palette.Text)
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                StatusMark(if (ok) Tone.OK else Tone.WARNING)
                Text(status, style = Type.Hint, color = if (ok) Palette.Muted else Palette.Amber)
            }
            if (phone) button(Modifier.padding(top = 1.dp))
        }
        if (!phone) button(Modifier)
    }
}
