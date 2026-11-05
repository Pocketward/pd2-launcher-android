package com.pd2launcher.app.ui

import androidx.annotation.DrawableRes
import androidx.compose.foundation.LocalIndication
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.indication
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.interaction.collectIsFocusedAsState
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusProperties
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.focus.onFocusChanged
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.disabled
import androidx.compose.ui.semantics.role
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import com.pd2launcher.app.AppUpdate
import com.pd2launcher.app.BuildConfig
import com.pd2launcher.app.Line
import com.pd2launcher.app.R
import com.pd2launcher.app.RowPart
import com.pd2launcher.app.Tone
import com.pd2launcher.app.UpdateChannel

/** Settings → App's updates: what [status], [rows] and the list's [line] say, and what the page's presses do. */
class AppPageUi(
    val status: AppUpdate.Status,
    val rows: List<AppUpdate.ChannelRow>,
    val line: PageLine,
    /** The one line above the rows while they are locked ([AppUpdate.lock]); null: not locked. */
    val lock: Line?,
    val onCheck: () -> Unit,
    val onPick: (UpdateChannel) -> Unit,
    val onSign: (AppUpdate.ChannelRow) -> Unit,
    val onAction: (AppUpdate.Action) -> Unit,
    val onPage: (String) -> Unit,
)

/**
 * The version the player has, when the app checked, and Check now: one group at the top of the
 * page. The d-pad goes from Check now only to [left] (the list; null: none on a phone) and Down to
 * [down], the first channel row: the search by place would find a chip or a sign on a lower row.
 * Grey during a check, it takes no focus: the focus it held goes to [down], and Up on that row stays
 * there ([ChannelRows]).
 */
@Composable
fun AppStatusBlock(ui: AppPageUi, left: FocusRequester?, down: FocusRequester) {
    var held by remember { mutableStateOf(false) }
    val lost = remember(ui.status.canCheck) { held }
    LaunchedEffect(ui.status.canCheck) { if (lost) runCatching { down.requestFocus() } }
    Row(
        Modifier.fillMaxWidth().padding(start = 14.dp, end = 14.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
            Text(
                stringResource(R.string.app_update_you_have, AppUpdate.shown(BuildConfig.SHOWN_VERSION)),
                style = Type.PageTitle.copy(fontWeight = FontWeight.SemiBold), color = Palette.Text,
            )
            Text(text(ui.status.line), style = Type.Hint, color = if (ui.status.tone == Tone.WARNING) Palette.Amber else Palette.Muted)
            if (com.pd2launcher.app.AppUpdater.TEST_BUILD) {
                Text(stringResource(R.string.app_update_test_build), style = Type.Hint, color = Palette.Muted)
            }
        }
        SlimButton(
            stringResource(R.string.app_update_check_now), ui.onCheck,
            Modifier.focusProperties {
                canFocus = ui.status.canCheck
                this.left = left ?: FocusRequester.Cancel
                right = FocusRequester.Cancel
                this.down = down
            }.onFocusChanged { held = it.isFocused },
            enabled = ui.status.canCheck,
        )
    }
}

/**
 * Update channel: a row for each channel. A press on a row selects it; a press on a control of a
 * row that is not selected selects that row first, then does the control's action
 * ([AppUpdate.presses]). An open row shows its channel's version as a chip next to its name (a
 * closed Beta has none); the chip opens the release page in the browser only where the release has
 * its own tag page. A row has a sign at its end for what it can do now. A download stays on the row
 * of its own channel. [first] goes on the first row's own target ([SettingsPage.firstRow]). While
 * Check now takes no focus, Up on the first row has no stop in the page, so it stays: the search by
 * place would go to "‹ Play". When the Beta row closes under the focus, the focus goes to the Stable row.
 * While the game or the official PD2 launcher runs, or the game starts, the rows are locked
 * ([AppPageUi.lock], [AppUpdate.ChannelRow.locked]): one amber line above them says why.
 */
@Composable
fun ChannelRows(ui: AppPageUi, first: FocusRequester?) {
    val stableRow = remember { FocusRequester() }
    var betaFocused by remember { mutableStateOf(false) }
    val beta = ui.rows.firstOrNull { it.channel == UpdateChannel.BETA }
    // Read in the composition that closes the row, before its focus goes.
    val lost = remember(beta?.closed) { betaFocused && beta?.closed == true }
    LaunchedEffect(beta?.closed) { if (lost) runCatching { stableRow.requestFocus() } }
    ui.lock?.let { Text(text(it), Modifier.padding(start = 14.dp, end = 14.dp, bottom = 4.dp), style = Type.Hint, color = Palette.Amber) }
    ui.rows.forEachIndexed { index, row ->
        if (index > 0) Rule(Modifier.padding(horizontal = 14.dp))
        if (row.closed) {
            ClosedChannelRow(row)
        } else {
            Box(if (row.channel == UpdateChannel.BETA) Modifier.onFocusChanged { betaFocused = it.hasFocus } else Modifier) {
                ChannelRowView(
                    row, ui, first.takeIf { index == 0 }, staysUp = index == 0 && !ui.status.canCheck,
                    rowFocus = stableRow.takeIf { row.channel == UpdateChannel.STABLE },
                )
            }
        }
    }
}

/**
 * A channel closed now ([AppUpdate.betaOpen]: no beta out that this build can take): greyed like Loot filters, no focus and no
 * press, no chip, dot or sign (the owner, 2026-11-03: "The beta channel can be grayed out for the
 * time being"). While a beta is out that this build cannot take, a line under it says why
 * ([AppUpdate.ChannelRow.info]; the owner, 2026-11-04, "Grey, with a reason"). TalkBack reads one
 * disabled radio button.
 */
@Composable
private fun ClosedChannelRow(row: AppUpdate.ChannelRow) {
    val name = stringResource(if (row.channel == UpdateChannel.STABLE) R.string.app_update_channel_stable else R.string.app_update_channel_beta)
    Row(
        Modifier.fillMaxWidth().heightIn(min = 56.dp).padding(start = 14.dp, end = 10.dp, top = 4.dp, bottom = 4.dp)
            .semantics(mergeDescendants = true) {
                disabled()
                role = Role.RadioButton
                selected = false
            },
        verticalAlignment = if (row.info != null) Alignment.Top else Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(14.dp),
    ) {
        RadioMark(false, if (row.info != null) Modifier.padding(top = 14.dp) else Modifier, enabled = false)
        Column(Modifier.weight(1f)) {
            Row(Modifier.heightIn(min = 48.dp), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Text(name, style = Type.Body.copy(fontWeight = FontWeight.SemiBold), color = Palette.Faint)
                Text(stringResource(R.string.app_update_channel_closed), style = Type.Body, color = Palette.Faint, maxLines = 1)
            }
            row.info?.let { Text(text(it), Modifier.offset(y = (-10).dp), style = Type.Hint, color = Palette.Muted) }
        }
    }
}

@Composable
private fun ChannelRowView(row: AppUpdate.ChannelRow, ui: AppPageUi, first: FocusRequester?, staysUp: Boolean, rowFocus: FocusRequester?) {
    val focus = remember { RowPart.entries.associateWith { FocusRequester() } }
    val parts = AppUpdate.parts(row)
    val interaction = remember { MutableInteractionSource() }
    val focused by interaction.collectIsFocusedAsState()
    // A press can take the sign or Stop away from under the focus: a download starts, stops or
    // ends; a check can take the chip. The focus then goes to what the row has now ([AppUpdate.focusAfter]), not out of the
    // page. Where that is the row, it waits there ([AppUpdate.focusWaits]) until the player moves
    // it: the download's checks have nothing to press, and Allow can follow them.
    var held by remember { mutableStateOf<RowPart?>(null) }
    var waiting by remember { mutableStateOf(false) }
    // The selection too: a press on a chip with no page on a row that is not selected takes the chip's stop away.
    val moving = listOf(row.offer != null, row.selected, row.sign, row.work?.action, row.work?.enabled)
    // The chip stays under the focus while other parts change.
    val lost = remember(moving) { held != null && (held != RowPart.CHIP || RowPart.CHIP !in parts) }
    LaunchedEffect(moving) {
        if (lost || (waiting && focused)) {
            held = null
            waiting = AppUpdate.focusWaits(row)
            runCatching { focus.getValue(AppUpdate.focusAfter(row)).requestFocus() }
        }
    }
    // The chip, Stop and the sign sit inside the row's target, so the d-pad's search finds none of
    // them from it: each stop names its neighbours. Left from the row's target leaves the page.
    fun stop(part: RowPart): Modifier {
        fun walk(by: Int) = parts.getOrNull(parts.indexOf(part) + by)?.let(focus::getValue) ?: FocusRequester.Cancel
        val named = Modifier.focusRequester(focus.getValue(part)).focusProperties {
            if (part != RowPart.ROW) left = walk(-1)
            right = walk(1)
            if (staysUp) up = FocusRequester.Cancel
        }
        return when (part) {
            RowPart.ROW -> named.onFocusChanged { if (!it.isFocused) waiting = false }
            RowPart.CHIP, RowPart.ACTION, RowPart.SIGN -> named.onFocusChanged { held = if (it.isFocused) part else null }
        }
    }
    val multi = row.work != null || row.info != null
    val name = stringResource(if (row.channel == UpdateChannel.STABLE) R.string.app_update_channel_stable else R.string.app_update_channel_beta)
    Box(Modifier.fillMaxWidth()) {
        // The row's own target, behind the chip and the sign, which take their own presses: a press
        // on the rest of the row only selects.
        Box(
            Modifier.matchParentSize().focusOn(first).focusOn(rowFocus).then(stop(RowPart.ROW)).focusMark(focused)
                .selectable(row.selected, interaction, LocalIndication.current, role = Role.RadioButton) { if (!row.locked) ui.onPick(row.channel) }
                .semantics {
                    contentDescription = name
                    if (row.locked) disabled()
                },
        )
        Row(
            Modifier.fillMaxWidth().heightIn(min = 56.dp).padding(start = 14.dp, end = 10.dp, top = 4.dp, bottom = if (multi) 2.dp else 4.dp),
            verticalAlignment = if (multi) Alignment.Top else Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(14.dp),
        ) {
            // The radio sits beside the name also over a longer row.
            RadioMark(row.selected, if (multi) Modifier.padding(top = 14.dp) else Modifier, enabled = !row.locked)
            Column(Modifier.weight(1f)) {
                Row(Modifier.heightIn(min = 48.dp), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    Text(name, Modifier.clearAndSetSemantics {}, style = Type.Body.copy(fontWeight = FontWeight.SemiBold), color = Palette.Text)
                    val offer = row.offer
                    if (offer != null) {
                        val page = AppUpdate.releasePage(offer)
                        VersionChip(
                            AppUpdate.shown(offer.versionName), row.fresh, row.selected, page != null, stop(RowPart.CHIP),
                            { press(row, RowPart.CHIP, ui) }.takeIf { RowPart.CHIP in parts },
                        )
                    } else {
                        Text(stringResource(R.string.app_update_none_out), style = Type.Body, color = Palette.Muted, maxLines = 1)
                    }
                }
                // Up to the name's line: the chip's 48 dp target leaves room under the name.
                val under = Modifier.offset(y = (-10).dp)
                row.work?.let { WorkPart(it, ui.onAction, under, stop(RowPart.ACTION)) }
                row.info?.let { Text(text(it), under, style = Type.Hint, color = Palette.Muted) }
            }
            RowSign(row, stop(RowPart.SIGN)) { press(row, RowPart.SIGN, ui) }
            row.ring?.let { ProgressRing(it, text(AppUpdate.ringLine(row))) }
        }
    }
}

/**
 * The version as a chip next to the name: 28 dp to see and 48 dp to touch, in teal on the selected
 * row; [dot]: the amber dot of a new version. A press ([onPress]) opens the release page where it
 * has one ([link]), after it selects a row that is not selected; null: no control (the selected
 * row's chip with no page), so it takes no focus and no press.
 */
@Composable
private fun VersionChip(version: String, dot: Boolean, selected: Boolean, link: Boolean, modifier: Modifier, onPress: (() -> Unit)?) {
    val interaction = remember { MutableInteractionSource() }
    val focused by interaction.collectIsFocusedAsState()
    // With no page to open, the words name only the version (TalkBack must not promise the browser).
    val words = if (link) stringResource(R.string.app_update_chip, version) else version
    val news = stringResource(R.string.app_update_dot)
    Box(
        modifier.heightIn(min = 48.dp)
            .then(if (onPress != null) Modifier.clickable(interactionSource = interaction, indication = null, role = Role.Button, onClick = onPress) else Modifier)
            .semantics(mergeDescendants = true) { contentDescription = if (dot) "$words, $news" else words },
        contentAlignment = Alignment.Center,
    ) {
        Row(
            Modifier.height(28.dp).clip(Pill).background(if (selected) Palette.Teal.copy(alpha = 0.16f) else Palette.Raised)
                .border(if (focused) 2.dp else 1.dp, if (focused) Palette.Teal else if (selected) Color.Transparent else Palette.Rule, Pill)
                .indication(interaction, LocalIndication.current).padding(horizontal = 10.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            // A new version: the app's small amber dot (the owner, 2026-11-03: "reuse this yellow dot for the App
            // update section as well for updates").
            if (dot) {
                Dot(Palette.Amber)
                Spacer(Modifier.width(6.dp))
            }
            Text(
                version, style = Type.Hint.copy(fontWeight = if (selected) FontWeight.SemiBold else FontWeight.Medium),
                color = if (selected) Palette.Teal else Palette.Muted, maxLines = 1, softWrap = false,
            )
            // The link glyph only where the chip opens a page ([link]: the release's own tag page).
            if (link) Spacer(Modifier.width(4.dp))
            if (link) Icon(painterResource(R.drawable.ic_open_in_new), null, Modifier.size(15.dp), tint = if (selected) Palette.Teal else Palette.Muted)
        }
    }
}

/** The sign at a row's end: its own 48 dp target, a 32 dp circle; the check mark does nothing. */
@Composable
private fun RowSign(row: AppUpdate.ChannelRow, modifier: Modifier, onPress: () -> Unit) {
    val sign = row.sign ?: return
    if (sign == AppUpdate.Sign.INSTALLED) {
        val words = stringResource(R.string.app_update_installed)
        Box(Modifier.size(48.dp).semantics { contentDescription = words }, contentAlignment = Alignment.Center) {
            Icon(painterResource(R.drawable.ic_check), null, Modifier.size(18.dp), tint = Palette.Teal)
        }
        return
    }
    val version = row.offer?.versionName.orEmpty().let(AppUpdate::shown)
    val enabled = !row.locked
    when (sign) {
        AppUpdate.Sign.DOWNLOAD -> SignButton(signIcon(sign), stringResource(R.string.app_update_sign_download, version), modifier, onPress, enabled)
        AppUpdate.Sign.RETRY -> SignButton(signIcon(sign), stringResource(R.string.app_update_sign_retry, version), modifier, onPress, enabled)
        else -> SignButton(signIcon(sign), stringResource(R.string.app_update_sign_install, version), modifier, onPress, enabled)
    }
}

/**
 * The glyph of [sign]: download, try again, install. The install sign has its own (a phone with an
 * arrow into it), not the chip's open-in-new glyph, which looks like a link (seen at the first
 * open of the update flow); its press opens Android's question again ([AppUpdateHost]), never a page.
 */
internal fun signIcon(sign: AppUpdate.Sign): Int = when (sign) {
    AppUpdate.Sign.DOWNLOAD -> R.drawable.ic_download
    AppUpdate.Sign.RETRY -> R.drawable.ic_refresh
    AppUpdate.Sign.INSTALL -> R.drawable.ic_install
    AppUpdate.Sign.INSTALLED -> R.drawable.ic_check
}

/**
 * One press on [part] of [row] ([AppUpdate.presses]): on a row that is not selected, its channel is
 * selected first (the radio moves, the pick is stored), then the control's own action.
 */
private fun press(row: AppUpdate.ChannelRow, part: RowPart, ui: AppPageUi) {
    for (step in AppUpdate.presses(row, part)) {
        when (step) {
            AppUpdate.Press.SELECT -> ui.onPick(row.channel)
            AppUpdate.Press.SIGN -> ui.onSign(row)
            AppUpdate.Press.PAGE -> row.offer?.let(AppUpdate::releasePage)?.let(ui.onPage)
        }
    }
}

/**
 * A running download on a row that is not selected (the owner, 2026-11-04, "Small progress ring"):
 * in the sign's place and at its size, a small teal ring that fills with [ring]'s progress, or spins
 * while the download is checked; no text, no press of its own (a press on the row selects it, and
 * the row then shows its lines and Stop). TalkBack reads [words].
 */
@Composable
private fun ProgressRing(ring: AppUpdate.Ring, words: String) {
    Box(Modifier.size(48.dp).semantics { contentDescription = words }, contentAlignment = Alignment.Center) {
        val fraction = ring.fraction
        if (fraction != null) {
            CircularProgressIndicator({ fraction }, Modifier.size(32.dp), color = Palette.Teal, strokeWidth = 3.dp, trackColor = Palette.Rule)
        } else {
            CircularProgressIndicator(Modifier.size(32.dp), color = Palette.Teal, strokeWidth = 3.dp, trackColor = Palette.Rule)
        }
    }
}

/**
 * A row's sign: the filled teal round button, the same for every sign on any row (the owner,
 * 2026-11-04: "Two icons, same style"); only the glyph differs ([signIcon]). Focused, a 2 dp ring
 * in the text colour. Not [enabled] (the rows are locked): grey, no focus and no press.
 */
@Composable
private fun SignButton(@DrawableRes icon: Int, words: String, modifier: Modifier, onPress: () -> Unit, enabled: Boolean = true) {
    val interaction = remember { MutableInteractionSource() }
    val focused by interaction.collectIsFocusedAsState()
    Box(
        modifier.size(48.dp)
            .then(
                if (enabled) {
                    Modifier.clickable(interactionSource = interaction, indication = null, role = Role.Button, onClick = onPress)
                } else {
                    Modifier.focusProperties { canFocus = false }
                },
            )
            .semantics(mergeDescendants = true) {
                contentDescription = words
                if (!enabled) disabled()
            },
        contentAlignment = Alignment.Center,
    ) {
        Box(
            Modifier.size(32.dp).clip(CircleShape).background(if (enabled) Palette.Teal else Palette.Rule)
                .border(if (focused) 2.dp else 0.dp, if (focused) Palette.Text else Color.Transparent, CircleShape)
                .indication(interaction, LocalIndication.current),
            contentAlignment = Alignment.Center,
        ) {
            Icon(painterResource(icon), null, Modifier.size(18.dp), tint = if (enabled) Palette.OnTeal else Palette.Faint)
        }
    }
}

/** The download of this row's build: its state, Stop on the line it stops, the bar, and what follows. [action] goes on Stop or Allow. */
@Composable
private fun WorkPart(view: AppUpdate.WorkView, onAction: (AppUpdate.Action) -> Unit, modifier: Modifier, action: Modifier) {
    Column(modifier, verticalArrangement = Arrangement.spacedBy(6.dp)) {
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            if (view.tone == Tone.WARNING || view.tone == Tone.ERROR) StatusMark(view.tone)
            Text(text(view.line), style = Type.Hint, color = Palette.Text, modifier = Modifier.weight(1f))
            if (view.action == AppUpdate.Action.STOP) {
                SlimButton(
                    stringResource(if (view.enabled) R.string.action_stop else R.string.action_stopping),
                    { onAction(AppUpdate.Action.STOP) }, action, enabled = view.enabled,
                )
            }
        }
        if (view.bar) Bar(view.progress, color = if (view.held) Palette.Faint else Palette.Teal)
        view.note?.let { Text(text(it), style = Type.Hint, color = if (view.noteWarns) Palette.Amber else Palette.Muted) }
        if (view.action == AppUpdate.Action.ALLOW) {
            SlimButton(stringResource(R.string.app_update_allow), { onAction(AppUpdate.Action.ALLOW) }, action.widthIn(min = 48.dp))
        }
    }
}
