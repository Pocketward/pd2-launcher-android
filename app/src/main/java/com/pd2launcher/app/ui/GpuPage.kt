package com.pd2launcher.app.ui

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.LocalIndication
import androidx.compose.foundation.background
import androidx.compose.foundation.focusable
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.interaction.collectIsFocusedAsState
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawing
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.selection.selectableGroup
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusProperties
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.disabled
import androidx.compose.ui.semantics.role
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import com.pd2launcher.app.AppSettings
import com.pd2launcher.app.DriverPage
import com.pd2launcher.app.HostGpu
import com.pd2launcher.app.PlayerDriver
import com.pd2launcher.app.R

/**
 * The page model of [app]'s pick for [gpu], with a debug build's chip ([DriverPage.debugChip]), or
 * else the route in use ([inUse], [DriverPage.onRoute]); and the chip's name.
 */
@Composable
internal fun driverPage(app: AppSettings, gpu: HostGpu?, inUse: DriverPage.InUse? = null): Pair<DriverPage.Page, String?> {
    val context = LocalContext.current
    val debugChip = remember { DriverPage.debugChip(context.filesDir) }
    val chip = DriverPage.chip(gpu, debugChip?.first)
    val orderGpu = DriverPage.orderGpu(gpu, debugChip?.first)
    val grey = remember { DriverPage.debugGrey(context.filesDir) }
    val onRoute = inUse?.takeIf { debugChip == null }?.greyed(grey)
    return DriverPage.page(chip, app.driverPick, app.driverWay, orderGpu, onRoute = onRoute) to (debugChip?.second ?: gpu?.name)
}

/** Display's GPU row value: "App driver", "Phone driver · VirGL", "Software driver" ([DriverPage.value]); null: none in use. */
@Composable
internal fun gpuValue(page: DriverPage.Page): String? = DriverPage.value(page)?.let { (driver, way) ->
    val name = stringResource(driverTitle(driver))
    if (way == null) name else stringResource(R.string.settings_gpu_value_way, name, stringResource(way.label))
}

private fun driverTitle(driver: PlayerDriver): Int = when (driver) {
    PlayerDriver.APP -> R.string.driver_app
    PlayerDriver.PHONE -> R.string.driver_phone
    PlayerDriver.SOFTWARE -> R.string.driver_software
}

/**
 * Settings → Display → GPU (decision 23, the Buttons and About sub page pattern; decision 33, the
 * mock's version 3): "‹ Display" and the title GPU, then the DRIVER group: the chip's line and one
 * list of the App driver, the Phone driver (with its VirGL and Venus rows under it while it is in
 * use and the chip has both; a way that cannot give the game OpenGL 3.3, or a Venus without
 * Zink's base requirements, is a grey row, [DriverPage.Page.greyWays]) and the Software driver ([DriverPage]). "In use" sits beside the
 * driver in use, or beside the picked way; the chip's better way says so. A pick
 * ([AppSettings.driverPick], [AppSettings.driverWay]) is the route of the next Play
 * ([DriverPage.route]); the rows show the route that it takes ([inUse]). The focus starts on
 * the first row that takes a pick, else on "‹ Display"; [onBack] returns to Display's GPU row.
 */
@Composable
fun GpuPage(
    app: AppSettings,
    gpu: HostGpu?,
    onAppChange: ((AppSettings) -> AppSettings) -> Unit,
    inUse: DriverPage.InUse? = null,
    onBack: () -> Unit,
) {
    BackHandler(onBack = onBack)
    val (page, name) = driverPage(app, gpu, inUse)
    val back = remember { FocusRequester() }
    val first = remember { FocusRequester() }
    val firstRow = page.rows.firstOrNull { it.enabled }?.driver
    // Down from the Phone driver goes to the way in use, not to the first way row.
    val way = remember { FocusRequester() }
    InitialFocus(if (firstRow != null) first else back)
    Column(Modifier.fillMaxSize().background(Palette.Background).windowInsetsPadding(WindowInsets.safeDrawing)) {
        Row(Modifier.heightIn(min = 56.dp).padding(horizontal = 8.dp), verticalAlignment = Alignment.CenterVertically) {
            LauncherButton(stringResource(R.string.back_to_display), onBack, kind = Kind.QUIET, height = 48.dp, focusRequester = back, padding = 14.dp)
            ScreenTitle(stringResource(R.string.settings_gpu), Modifier.padding(start = 4.dp))
        }
        Rule()
        Column(Modifier.weight(1f).fillMaxWidth().verticalScroll(rememberScrollState()), horizontalAlignment = Alignment.CenterHorizontally) {
            Column(Modifier.widthIn(max = 560.dp).fillMaxWidth().padding(horizontal = 8.dp).padding(bottom = 24.dp)) {
                GroupLabel(stringResource(R.string.settings_section_gpu_driver), Modifier.padding(start = 14.dp), top = 8.dp)
                Text(
                    name?.let { stringResource(R.string.settings_driver_chip, it) } ?: stringResource(R.string.settings_driver_chip_unknown),
                    style = Type.Hint, color = Palette.Muted, modifier = Modifier.padding(start = 14.dp, end = 14.dp, bottom = 4.dp),
                )
                page.note?.let {
                    Text(stringResource(it), style = Type.Hint, color = Palette.Amber, modifier = Modifier.padding(start = 14.dp, end = 14.dp, bottom = 4.dp))
                }
                Column(Modifier.selectableGroup()) {
                    page.rows.forEachIndexed { index, row ->
                        if (index > 0) Rule(Modifier.padding(horizontal = 14.dp))
                        val parent = row.driver == PlayerDriver.PHONE && page.ways.isNotEmpty()
                        DriverRadio(
                            stringResource(driverTitle(row.driver)), stringResource(row.body), row.selected, row.tag, row.enabled, row.focusable,
                            {
                                // Review LOW-1: a row also stores its driver where the stored pick gave no route (that row shows in use then).
                                if (!row.selected || app.driverPick != row.driver) onAppChange { it.withPick(row.driver) }
                            },
                            focusRequester = first.takeIf { row.driver == firstRow },
                            modifier = if (parent) Modifier.focusProperties { down = way } else Modifier,
                            minHeight = 60.dp, padding = PaddingValues(start = 14.dp, end = 14.dp, top = 10.dp, bottom = if (parent) 4.dp else 10.dp),
                        )
                        // The Phone driver's ways: full radios on its text column (14 + 20 + 14 = 48 dp), 48 dp rows, 8 dp apart.
                        if (parent) {
                            Column(Modifier.selectableGroup().padding(start = 34.dp, bottom = 12.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                                page.ways.forEach { w ->
                                    DriverRadio(
                                        stringResource(w.label), stringResource(R.string.driver_way_better).takeIf { w == page.better },
                                        w == page.way, w == page.way, enabled = true, focusable = true,
                                        { if (w != page.way) onAppChange { it.withPick(PlayerDriver.PHONE, w) } },
                                        focusRequester = way.takeIf { w == page.way },
                                        minHeight = 48.dp, padding = PaddingValues(horizontal = 14.dp, vertical = 4.dp),
                                    )
                                }
                                // A way that cannot run here by its own rule (VirGL: OpenGL 3.3; Venus: Zink's base requirements): grey, with the reason (DriverPage.greyWays).
                                page.greyWays.forEach { w ->
                                    DriverRadio(
                                        stringResource(w.label), stringResource(R.string.driver_phone_none),
                                        selected = false, tag = false, enabled = false, focusable = true, onSelect = {},
                                        focusRequester = null,
                                        minHeight = 48.dp, padding = PaddingValues(horizontal = 14.dp, vertical = 4.dp),
                                    )
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/**
 * A driver or a way: its radio (an unpicked ring in [Palette.Muted] on this page, which is 3:1
 * and more; the kit's [Palette.Rule] ring is not), its name with "In use" beside it when [tag], and
 * its line. A grey row stays focusable while rows take a pick ([focusable]), as [OptionRow]. TalkBack
 * reads the row as one radio button, with its tag.
 */
@Composable
private fun DriverRadio(
    title: String,
    body: String?,
    selected: Boolean,
    tag: Boolean,
    enabled: Boolean,
    focusable: Boolean,
    onSelect: () -> Unit,
    focusRequester: FocusRequester?,
    minHeight: Dp,
    padding: PaddingValues,
    modifier: Modifier = Modifier,
) {
    val interaction = remember { MutableInteractionSource() }
    val focused by interaction.collectIsFocusedAsState()
    Row(
        modifier
            .focusOn(focusRequester)
            .fillMaxWidth().heightIn(min = minHeight).focusMark(focused)
            .then(
                when {
                    enabled -> Modifier.selectable(selected, interaction, LocalIndication.current, role = Role.RadioButton, onClick = onSelect)
                    else -> Modifier.semantics(mergeDescendants = true) {
                        disabled()
                        role = Role.RadioButton
                        this.selected = selected
                    }.then(if (focusable) Modifier.focusable(interactionSource = interaction) else Modifier)
                },
            )
            .padding(padding),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(14.dp),
    ) {
        RadioMark(selected, enabled = enabled, ring = if (enabled) Palette.Muted else Palette.Faint)
        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Text(title, style = Type.Body, color = if (enabled) Palette.Text else Palette.Faint)
                if (tag) InUseTag(enabled)
            }
            if (body != null) Text(body, style = Type.Hint, color = if (enabled) Palette.Muted else Palette.Faint)
        }
    }
}

/** "In use": a small teal tag (Label type, the teal tint, a pill); grey on a grey row. */
@Composable
private fun InUseTag(enabled: Boolean) {
    val color = if (enabled) Palette.Teal else Palette.Faint
    Text(
        stringResource(R.string.driver_in_use), style = Type.Label, color = color,
        modifier = Modifier.clip(Pill).background(color.copy(alpha = 0.16f)).padding(horizontal = 8.dp, vertical = 1.dp),
    )
}
