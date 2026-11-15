package com.pd2launcher.app.ui

import android.content.ActivityNotFoundException
import android.content.Intent
import android.net.Uri
import androidx.activity.compose.BackHandler
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.LocalIndication
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.animateScrollBy
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.interaction.collectIsFocusedAsState
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxScope
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
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
import androidx.compose.foundation.layout.wrapContentHeight
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyListScope
import androidx.compose.foundation.lazy.LazyListState
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.selection.selectableGroup
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.input.InputMode
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalInputModeManager
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.pd2launcher.app.LootFilterStore
import com.pd2launcher.app.LootFilters
import com.pd2launcher.app.R
import com.pd2launcher.app.Tone

/**
 * Settings → Game → Loot filters: the final design E1 + F2 (owner, 2026-11-06; mock docs/design/loot-filters/mock/e1f2.html,
 * its parts in mock/kit.css). The official PD2 launcher's Filters page in the app's own parts.
 *
 * - A handheld: the Settings two-pane: a one-line author list (240 dp, as Settings' list) and the author's pane (its
 *   header, then the filters).
 * - A phone (P1): two steps: a first page (In use as the prominent row, Your files, PD2's authors, Help), then an
 *   author's filters.
 *
 * Every filter is a [FilterRow] of a fixed 64 dp: the radio marks the filter in use, the pick only tints and never grows;
 * the [Slot] at the end of the name line holds Keep my own copy (Remove on your files), "Downloading…", or what the last
 * press there did. The filter in use has no slot. Loading, the error (E1) and an empty Your files are one [EmptyState]
 * where the rows would be. Use this filter is the corner pill; a picked row scrolls clear of it, and with a controller, A
 * on a row moves the focus to it. Back steps from a phone's filters to its first page, else returns to Game ([onBack]).
 */
@Composable
fun LootFiltersPage(onBack: () -> Unit) {
    val context = LocalContext.current
    LaunchedEffect(Unit) { LootFilterStore.open(context) }
    val state by LootFilterStore.state.collectAsState()
    val phone = LocalFormFactor.current == FormFactor.PHONE
    var filtersStep by rememberSaveable { mutableStateOf(false) }
    var picked by remember { mutableStateOf<String?>(null) }
    var linkFailed by remember { mutableStateOf(false) }
    // A controller's A on a row: the focus goes to Use this filter one frame later, once the pill is enabled (its focus
    // node changes with it, so an earlier request is lost and the focus falls back to the screen's first control).
    var focusPill by remember { mutableStateOf(0) }
    BackHandler { if (phone && filtersStep) filtersStep = false else onBack() }
    val selected = state.shownFilters.firstOrNull { it.file == picked }
    val addFile = rememberLauncherForActivityResult(ActivityResultContracts.OpenDocument()) { uri -> uri?.let { LootFilterStore.add(context, it) } }
    val keyboard = LocalInputModeManager.current.inputMode == InputMode.Keyboard
    val back = remember { FocusRequester() }
    val usePill = remember { FocusRequester() }
    val list = rememberLazyListState()
    InitialFocus(back, filtersStep)
    LaunchedEffect(focusPill) {
        if (focusPill == 0) return@LaunchedEffect
        androidx.compose.runtime.withFrameNanos { }
        runCatching { usePill.requestFocus() }
    }
    val actions = Actions(
        open = { url ->
            linkFailed = try {
                context.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(url)))
                false
            } catch (_: ActivityNotFoundException) {
                true
            }
        },
        show = { author ->
            picked = null
            linkFailed = false
            LootFilterStore.show(context, author)
            filtersStep = true
        },
        pick = { f ->
            if (picked != f.file) LootFilterStore.picked(context)
            picked = f.file
            linkFailed = false
            // A controller goes straight to Use this filter (owner, 2026-11-06).
            if (keyboard) focusPill++
        },
        keep = { LootFilterStore.keep(context, it) },
        remove = { LootFilterStore.remove(context, it.file) },
        undo = { LootFilterStore.undo(context) },
        add = { addFile.launch(arrayOf("*/*")) },
        retry = { LootFilterStore.retry(context) },
    )
    val corners = Corners.of(phone)
    // A picked row scrolls clear of the corner pill, so its slot is never under it.
    ClearOfPill(list, state, picked, corners.phantom)
    Box(Modifier.fillMaxSize().background(Palette.Background).windowInsetsPadding(WindowInsets.safeDrawing)) {
        Column(Modifier.fillMaxSize()) {
            val step = phone && filtersStep
            Row(Modifier.heightIn(min = 56.dp).padding(horizontal = 8.dp), verticalAlignment = Alignment.CenterVertically) {
                LauncherButton(
                    stringResource(if (step) R.string.loot_back_list else R.string.back_to_game),
                    { if (step) filtersStep = false else onBack() },
                    kind = Kind.QUIET, height = 48.dp, focusRequester = back, padding = 14.dp,
                )
                ScreenTitle(if (step) authorTitle(state) else stringResource(R.string.loot_title), Modifier.padding(start = 4.dp).weight(1f))
                if (!phone) ExternalLink(stringResource(R.string.loot_help)) { actions.open(LootFilters.HELP_URL) }
            }
            Rule()
            when {
                phone && !filtersStep -> LazyColumn(Modifier.weight(1f).fillMaxWidth(), contentPadding = PaddingValues(start = 8.dp, end = 8.dp, bottom = corners.phantom)) {
                    firstPage(state, actions)
                }
                phone -> Box(Modifier.weight(1f).fillMaxWidth()) { Pane(state, selected, actions, list, corners.phantom, header = false) }
                else -> Row(Modifier.weight(1f).fillMaxWidth()) {
                    LazyColumn(Modifier.width(LIST_WIDTH).fillMaxHeight(), contentPadding = PaddingValues(8.dp)) { authorList(state, actions) }
                    Rule(Modifier.fillMaxHeight().padding(vertical = 12.dp), vertical = true)
                    Box(Modifier.weight(1f).fillMaxHeight()) { Pane(state, selected, actions, list, corners.phantom, header = true) }
                }
            }
        }
        if (!phone || filtersStep) {
            Box(Modifier.align(Alignment.BottomEnd)) {
                val inUse = selected != null && state.inUse(state.shown, selected.file)
                CornerPill(
                    stringResource(if (inUse) R.string.loot_in_use_button else R.string.loot_use),
                    { selected?.let { LootFilterStore.use(context, it) } },
                    Kind.PRIMARY, enabled = state.using == null && selected != null && !inUse, focusRequester = usePill,
                )
            }
        }
        if (linkFailed) {
            Text(
                stringResource(R.string.loot_no_browser), style = Type.Hint, color = Palette.Amber,
                modifier = Modifier.align(Alignment.BottomStart).padding(20.dp).semantics { liveRegion = LiveRegionMode.Polite },
            )
        }
    }
}

/** Settings' list width (SettingsScreen.kt LIST_WIDTH). */
private val LIST_WIDTH = 240.dp

/** What the rows can do. */
private class Actions(
    val open: (String) -> Unit,
    val show: (String) -> Unit,
    val pick: (LootFilters.Filter) -> Unit,
    val keep: (LootFilters.Filter) -> Unit,
    val remove: (LootFilters.Filter) -> Unit,
    val undo: () -> Unit,
    val add: () -> Unit,
    val retry: () -> Unit,
)

@Composable
private fun authorTitle(state: LootFilterStore.State): String =
    if (state.shown == LootFilters.LOCAL) stringResource(R.string.loot_own) else state.shown.orEmpty()

/** A picked row whose end is under the corner pill scrolls up until it is [clearance] above the bottom. */
@Composable
private fun ClearOfPill(list: LazyListState, state: LootFilterStore.State, picked: String?, clearance: androidx.compose.ui.unit.Dp) {
    val px = with(LocalDensity.current) { clearance.roundToPx() }
    LaunchedEffect(picked) {
        val key = picked ?: return@LaunchedEffect
        val item = list.layoutInfo.visibleItemsInfo.firstOrNull { it.key == "filter:$key" } ?: return@LaunchedEffect
        val bottom = list.layoutInfo.viewportEndOffset - px
        val end = item.offset + item.size
        if (end > bottom) list.animateScrollBy((end - bottom).toFloat())
    }
}

// ---------------------------------------------------------------- the pane (handheld and phone)

/** The author's (or Your files') pane: its header and note, then the filters, or the [EmptyState]. */
@Composable
private fun BoxScope.Pane(
    state: LootFilterStore.State,
    selected: LootFilters.Filter?,
    actions: Actions,
    list: LazyListState,
    phantom: androidx.compose.ui.unit.Dp,
    header: Boolean,
) {
    val local = state.shown == LootFilters.LOCAL
    val empty: (@Composable () -> Unit)? = when {
        state.shown == null -> null
        !local && state.listLoad == LootFilterStore.Load.FAILED -> {
            { EmptyState(R.drawable.ic_filter_alt_off, stringResource(R.string.loot_error_title), stringResource(R.string.loot_error_body), stringResource(R.string.loot_try_again), actions.retry) }
        }
        !local && state.shownFilters.isEmpty() && state.listLoad != LootFilterStore.Load.OK -> {
            { EmptyState(null, stringResource(R.string.loot_loading_title), stringResource(R.string.loot_loading_body), null, null) }
        }
        local && state.own.isEmpty() -> {
            { EmptyState(R.drawable.ic_folder_open, stringResource(R.string.loot_own_empty_title), stringResource(R.string.loot_own_empty_body), stringResource(R.string.loot_add), actions.add) }
        }
        else -> null
    }
    LazyColumn(Modifier.fillMaxSize(), state = list, contentPadding = PaddingValues(start = 8.dp, end = 8.dp, bottom = phantom)) {
        item(key = "header") { PaneHeader(state, header, actions) }
        if (empty == null) filterRows(state, selected, actions)
    }
    if (empty != null) Box(Modifier.matchParentSize().padding(top = if (header) 72.dp else 48.dp, bottom = phantom), contentAlignment = Alignment.Center) { empty() }
}

/** The pane's header (the Settings pane: its title and one line, the end slot ending 14 dp in), then its note. */
@Composable
private fun PaneHeader(state: LootFilterStore.State, title: Boolean, actions: Actions) {
    val local = state.shown == LootFilters.LOCAL
    Column {
        // A phone's header is one line (its title is the screen's): the hint and the link on one centred line.
        Row(
            Modifier.fillMaxWidth().padding(start = 14.dp, end = 4.dp, top = if (title) 12.dp else 4.dp, bottom = 4.dp),
            verticalAlignment = if (title) Alignment.Top else Alignment.CenterVertically,
        ) {
            Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(1.dp)) {
                if (title) Text(if (local) stringResource(R.string.loot_own) else state.author(state.shown)?.name.orEmpty(), style = Type.PageTitle, color = Palette.Text)
                Text(stringResource(if (local) R.string.loot_rule_own else R.string.loot_rule_online), style = Type.Hint, color = Palette.Muted)
            }
            if (local) {
                TextLink(stringResource(R.string.loot_add), actions.add)
            } else {
                state.author(state.shown)?.url?.let(LootFilters::authorPage)?.let { page -> ExternalLink(stringResource(R.string.loot_author_page)) { actions.open(page) } }
            }
        }
        // The pane's note: no network (amber), no filter in use yet (amber), or a failed action (a red Banner).
        val message = state.message
        when {
            message != null && state.messageTone == Tone.ERROR -> Banner(text(message), Palette.Red, Modifier.padding(horizontal = 6.dp, vertical = 4.dp))
            message != null -> PaneNote(text(message))
            !local && state.authorsLoad == LootFilterStore.Load.FAILED && state.authors.isNotEmpty() -> PaneNote(stringResource(R.string.loot_no_network_saved))
            state.managedOutside -> PaneNote(managedOutsideText(state))
            state.choice == null -> PaneNote(stringResource(R.string.loot_no_filter_note))
        }
    }
}

/** The note while the official launcher (or another app) holds `loot.filter` (owner, 2026-11-14). */
@Composable
private fun managedOutsideText(state: LootFilterStore.State): String =
    state.launcherPick?.let { stringResource(R.string.loot_managed_outside_named, "${it.author} · ${it.file}") }
        ?: stringResource(R.string.loot_managed_outside)

@Composable
private fun PaneNote(words: String) {
    Text(words, style = Type.Hint, color = Palette.Amber, modifier = Modifier.padding(start = 14.dp, end = 14.dp, top = 2.dp, bottom = 6.dp).semantics { liveRegion = LiveRegionMode.Polite })
}

/** The filters, with a 14 dp-inset Rule between rows (none beside the pick); only the rows form the radio group. */
private fun LazyListScope.filterRows(state: LootFilterStore.State, selected: LootFilters.Filter?, actions: Actions) {
    val filters = state.shownFilters
    items(filters, key = { "filter:" + it.file }) { f ->
        val i = filters.indexOf(f)
        val near = f.file == selected?.file || filters.getOrNull(i - 1)?.file == selected?.file
        Column(Modifier.selectableGroup()) {
            if (i > 0 && !near) Rule(Modifier.padding(horizontal = 14.dp))
            FilterRow(state, f, f.file == selected?.file, actions)
        }
    }
}

/**
 * FilterRow (F2): a fixed 64 dp: the radio (the filter in use), then the name line (the name cut to one line, In use, the
 * [Slot] at its end) and one description line, cut. The pick is focusMark(selected); it never grows.
 */
@Composable
private fun FilterRow(state: LootFilterStore.State, filter: LootFilters.Filter, picked: Boolean, actions: Actions) {
    val interaction = remember { MutableInteractionSource() }
    val focused by interaction.collectIsFocusedAsState()
    val local = state.shown == LootFilters.LOCAL
    val inUse = state.inUse(state.shown, filter.file)
    val inUseWord = stringResource(R.string.loot_in_use_tag)
    val description = when {
        local -> state.copies[filter.file]?.let { stringResource(R.string.loot_copy_of, it.author) } ?: stringResource(R.string.loot_added_here)
        filter.description.isNotEmpty() -> filter.description
        else -> stringResource(R.string.loot_no_description)
    }
    Row(
        Modifier.fillMaxWidth().height(64.dp).focusMark(focused, selected = picked)
            .clickable(interaction, LocalIndication.current, role = Role.RadioButton) { actions.pick(filter) }
            .semantics {
                selected = inUse
                if (inUse) stateDescription = inUseWord
            }
            .padding(start = 14.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(14.dp),
    ) {
        RadioMark(inUse, ring = Palette.Muted)
        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
            Row(Modifier.height(24.dp), verticalAlignment = Alignment.CenterVertically) {
                // The name and its tag take the room first; the slot gets what is left at the end (no spacer that would
                // split the room with the name and cut it short).
                Row(Modifier.weight(1f), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    Text(
                        filter.display, style = if (picked) Type.Body.copy(fontWeight = FontWeight.SemiBold) else Type.Body, color = Palette.Text,
                        maxLines = 1, overflow = TextOverflow.Ellipsis, modifier = Modifier.weight(1f, fill = false),
                    )
                    if (inUse) InUseTag(onTint = picked || focused)
                }
                if (picked) Slot(state, filter, local, inUse, actions)
            }
            // Just put in use: the description line says when it works (owner, 2026-11-06: PD2 loads loot.filter at each start).
            val used = state.noteOn(filter.file) == LootFilterStore.NoteKind.USED
            Text(
                if (used) stringResource(R.string.loot_works_next_start) else description, style = Type.Hint,
                color = when {
                    used -> Palette.Teal
                    !local && filter.description.isEmpty() && !picked -> Palette.Faint
                    else -> Palette.Muted
                },
                maxLines = 1, overflow = TextOverflow.Ellipsis, modifier = Modifier.padding(end = 14.dp),
            )
        }
    }
}

/**
 * The Slot at the end of the name line: its 48 dp target reaches past the 24 dp line (as VersionChip's), so the row keeps
 * its height. A link is a TextLink (its words end 24 dp from the edge); a status ends at the same place. TalkBack reads it.
 */
@Composable
private fun Slot(state: LootFilterStore.State, filter: LootFilters.Filter, local: Boolean, inUse: Boolean, actions: Actions) {
    val status: (@Composable () -> Unit)? = when {
        state.using == filter.file -> { { StatusMark(Tone.WORKING); Text(stringResource(R.string.loot_slot_downloading), style = Type.Hint, color = Palette.Muted) } }
        // An error on the tinted row: the red mark and words in Text (red words on the tint are 4.4:1).
        state.noteOn(filter.file) == LootFilterStore.NoteKind.FAILED -> { { StatusMark(Tone.ERROR); Text(stringResource(R.string.loot_slot_failed), style = Type.Hint, color = Palette.Text) } }
        state.noteOn(filter.file) == LootFilterStore.NoteKind.COPIED -> { { Text(stringResource(R.string.loot_slot_copied), style = Type.Hint, color = Palette.Teal) } }
        else -> null
    }
    Box(Modifier.height(24.dp).wrapContentHeight(unbounded = true), contentAlignment = Alignment.CenterEnd) {
        when {
            status != null -> Row(
                Modifier.heightIn(min = 48.dp).padding(start = 10.dp, end = 24.dp).semantics(mergeDescendants = true) { liveRegion = LiveRegionMode.Polite },
                verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) { status() }
            state.noteOn(filter.file) == LootFilterStore.NoteKind.REMOVED -> Row(Modifier.padding(end = 14.dp), verticalAlignment = Alignment.CenterVertically) {
                Text(stringResource(R.string.loot_slot_removed), style = Type.Hint, color = Palette.Muted, modifier = Modifier.semantics { liveRegion = LiveRegionMode.Polite })
                TextLink(stringResource(R.string.loot_undo), actions.undo)
            }
            inUse -> Unit
            local -> TextLink(stringResource(R.string.loot_remove_short), { actions.remove(filter) }, Modifier.padding(end = 14.dp))
            else -> TextLink(stringResource(R.string.loot_keep), { actions.keep(filter) }, Modifier.padding(end = 14.dp))
        }
    }
}

/** InUseTag (GpuPage): Label, Teal, the Teal 16 % pill; on a tinted row on the Background, so it stays 7.7:1. */
@Composable
private fun InUseTag(onTint: Boolean = false) {
    Text(
        stringResource(R.string.loot_in_use_tag), style = Type.Label, color = Palette.Teal, maxLines = 1,
        modifier = Modifier.clip(Pill).background(if (onTint) Palette.Background else Palette.Teal.copy(alpha = 0.16f)).padding(horizontal = 8.dp, vertical = 1.dp),
    )
}

/** E1: where the rows would be: a 48 dp icon (or the spinner), one Body line, one Hint line, an optional button. */
@Composable
private fun EmptyState(icon: Int?, title: String, body: String, button: String?, onButton: (() -> Unit)?) {
    Column(
        Modifier.padding(horizontal = 24.dp).semantics(mergeDescendants = true) { liveRegion = LiveRegionMode.Polite },
        horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        if (icon == null) {
            CircularProgressIndicator(Modifier.size(48.dp), strokeWidth = 3.dp, color = Palette.Teal, trackColor = Palette.Rule)
        } else {
            Icon(painterResource(icon), null, Modifier.size(48.dp), tint = Palette.Faint)
        }
        Text(title, style = Type.Body, color = Palette.Text, textAlign = TextAlign.Center)
        Text(body, style = Type.Hint, color = Palette.Muted, textAlign = TextAlign.Center, modifier = Modifier.widthIn(max = 280.dp))
        if (button != null && onButton != null) LauncherButton(button, onButton, Modifier.padding(top = 8.dp), height = 48.dp)
    }
}

/** A TextLink that opens the browser, with the link mark after it (as VersionChip's): open_in_new, 15 dp, 4 dp after. */
@Composable
private fun ExternalLink(label: String, onClick: () -> Unit) {
    val interaction = remember { MutableInteractionSource() }
    val focused by interaction.collectIsFocusedAsState()
    val words = stringResource(R.string.loot_opens_browser, label)
    Row(
        Modifier.heightIn(min = 48.dp).focusMark(focused, shape = Small)
            .clickable(interactionSource = interaction, indication = LocalIndication.current, role = Role.Button, onClick = onClick)
            .clearAndSetSemantics { contentDescription = words }
            .padding(start = 10.dp, end = 10.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(label, style = Type.Body, color = Palette.Teal)
        Icon(painterResource(R.drawable.ic_open_in_new), null, Modifier.padding(start = 4.dp).size(15.dp), tint = Palette.Teal)
    }
}

// ---------------------------------------------------------------- a handheld's author list

/** The one-line author list (Settings' list rows): Your files with its count, then PD2's authors; In use beside the author in use. */
private fun LazyListScope.authorList(state: LootFilterStore.State, actions: Actions) {
    item(key = "own") {
        AuthorRow(stringResource(R.string.loot_own), state.shown == LootFilters.LOCAL, state.inUseChoice?.local == true, state.own.size.toString()) {
            actions.show(LootFilters.LOCAL)
        }
    }
    if (state.authors.isEmpty() && state.authorsLoad == LootFilterStore.Load.FAILED) {
        item(key = "offline") { PaneNote(stringResource(R.string.loot_authors_offline)) }
    }
    items(state.authors, key = { "author:" + it.author }) { a ->
        AuthorRow(a.author, state.shown == a.author, state.inUseChoice?.author == a.author, null) { actions.show(a.author) }
    }
}

/** PageRow on a handheld: min 56, 6/12/6/14; the open one TealTint with a SemiBold Teal title. */
@Composable
private fun AuthorRow(name: String, shown: Boolean, inUse: Boolean, count: String?, onClick: () -> Unit) {
    val interaction = remember { MutableInteractionSource() }
    val focused by interaction.collectIsFocusedAsState()
    Row(
        Modifier.fillMaxWidth().heightIn(min = 56.dp).focusMark(focused, shown)
            .clickable(interaction, LocalIndication.current, role = Role.Button, onClick = onClick)
            .semantics { selected = shown }
            .padding(start = 14.dp, end = 12.dp, top = 6.dp, bottom = 6.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        Text(
            name, style = Type.Body.copy(fontWeight = if (shown) FontWeight.SemiBold else null), color = if (shown) Palette.Teal else Palette.Text,
            maxLines = 1, overflow = TextOverflow.Ellipsis, modifier = Modifier.weight(1f, fill = false),
        )
        if (count != null) Text(count, style = Type.Hint, color = Palette.Muted)
        if (inUse) InUseTag(onTint = shown || focused)
    }
}

// ---------------------------------------------------------------- a phone's first page (P1)

/** In use first, as the prominent row (Detailed logging's), then Your files, PD2's authors, Help last (owner, 2026-11-06). */
private fun LazyListScope.firstPage(state: LootFilterStore.State, actions: Actions) {
    item { GroupLabel(stringResource(R.string.loot_section_in_use), Modifier.padding(start = 14.dp), top = 12.dp) }
    item {
        val c = state.inUseChoice
        if (state.managedOutside) {
            PhoneRow(state.launcherPick?.file ?: stringResource(R.string.loot_set_outside), stringResource(R.string.loot_by_launcher), null, prominent = true, onClick = null)
        } else if (c == null) {
            PhoneRow(stringResource(R.string.loot_no_filter), null, stringResource(R.string.loot_choose_one), prominent = true, onClick = null)
        } else {
            PhoneRow(
                if (c.local) state.copies[c.file]?.display ?: c.file else c.display,
                if (c.local) stringResource(R.string.loot_own) else c.author,
                stringResource(if (c.local) R.string.loot_rule_own_file else R.string.loot_rule_online), prominent = true,
            ) { actions.show(c.author) }
        }
    }
    if (state.managedOutside) item { PaneNote(managedOutsideText(state)) }
    item { GroupLabel(stringResource(R.string.loot_section_device), Modifier.padding(start = 14.dp)) }
    item {
        PhoneRow(
            stringResource(R.string.loot_own), pluralStringResource(R.plurals.loot_own_count, state.own.size, state.own.size), null,
            inUse = state.inUseChoice?.local == true,
        ) { actions.show(LootFilters.LOCAL) }
    }
    item { GroupLabel(stringResource(R.string.loot_section_authors), Modifier.padding(start = 14.dp)) }
    when {
        state.authors.isEmpty() && state.authorsLoad == LootFilterStore.Load.FAILED -> {
            item { PaneNote(stringResource(R.string.loot_authors_offline)) }
            item { PhoneRow(stringResource(R.string.loot_try_again), null, null, accent = true) { actions.retry() } }
        }
        state.authors.isEmpty() -> item { Box(Modifier.fillMaxWidth().padding(24.dp), contentAlignment = Alignment.Center) { CircularProgressIndicator(Modifier.size(24.dp), strokeWidth = 2.dp, color = Palette.Teal, trackColor = Palette.Rule) } }
        else -> items(state.authors, key = { "author:" + it.author }) { a ->
            Column {
                if (a != state.authors.first()) Rule(Modifier.padding(horizontal = 14.dp))
                PhoneRow(a.author, null, null, inUse = state.inUseChoice?.author == a.author) { actions.show(a.author) }
            }
        }
    }
    item { GroupLabel(stringResource(R.string.loot_section_help), Modifier.padding(start = 14.dp)) }
    item { PhoneRow(stringResource(R.string.loot_help), null, stringResource(R.string.loot_help_body), external = true) { actions.open(LootFilters.HELP_URL) } }
}

/**
 * PageRow on a phone: the title (SemiBold when [prominent]), a teal value, one Hint line, In use beside the title, the
 * teal "›" (or the link mark for [external]). [prominent]: ToggleRow's prominent look (Surface, a Rule edge, RowShape).
 */
@Composable
private fun PhoneRow(
    title: String,
    value: String?,
    body: String?,
    inUse: Boolean = false,
    prominent: Boolean = false,
    external: Boolean = false,
    accent: Boolean = false,
    onClick: (() -> Unit)?,
) {
    val interaction = remember { MutableInteractionSource() }
    val focused by interaction.collectIsFocusedAsState()
    val opens = stringResource(R.string.loot_opens_browser, title)
    Row(
        Modifier
            .then(
                if (prominent) {
                    Modifier.padding(top = 6.dp, bottom = 4.dp).clip(RowShape).background(Palette.Surface)
                        .border(1.dp, if (focused) androidx.compose.ui.graphics.Color.Transparent else Palette.Rule, RowShape)
                } else {
                    Modifier
                },
            )
            .fillMaxWidth().heightIn(min = if (prominent) 60.dp else 56.dp).focusMark(focused)
            .then(if (onClick != null) Modifier.clickable(interaction, LocalIndication.current, role = Role.Button, onClick = onClick) else Modifier)
            .then(if (external) Modifier.semantics { contentDescription = opens } else Modifier)
            .padding(start = 14.dp, end = 12.dp, top = if (prominent) 10.dp else 6.dp, bottom = if (prominent) 10.dp else 6.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(14.dp),
    ) {
        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(1.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Text(
                    title, style = if (prominent) Type.Body.copy(fontWeight = FontWeight.SemiBold) else Type.Body,
                    color = if (accent) Palette.Teal else Palette.Text, maxLines = 1, overflow = TextOverflow.Ellipsis, modifier = Modifier.weight(1f, fill = false),
                )
                if (inUse) InUseTag()
            }
            if (value != null) Text(value, style = Type.Hint, color = Palette.Teal, maxLines = 1, overflow = TextOverflow.Ellipsis)
            if (body != null) Text(body, style = Type.Hint, color = Palette.Muted, maxLines = 1, overflow = TextOverflow.Ellipsis)
        }
        if (onClick != null) {
            if (external) Icon(painterResource(R.drawable.ic_open_in_new), null, Modifier.size(18.dp), tint = Palette.Teal)
            else Text("›", style = Type.Title, color = Palette.Teal, modifier = Modifier.clearAndSetSemantics {})
        }
    }
}
