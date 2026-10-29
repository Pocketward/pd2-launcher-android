package com.pd2launcher.app.ui

import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import com.pd2launcher.app.DiabloSource
import com.pd2launcher.app.R
import com.pd2launcher.app.SourceChoice
import com.pd2launcher.app.Tone

/**
 * Where Diablo II comes from, the first page of Download: Fast download (the default: the engine
 * fetches and installs Diablo II and Lord of Destruction itself), Blizzard's installers, or an
 * existing Diablo II folder, picked with the system picker ([onPickFolder], or Continue before a
 * folder is accepted) and copied into the app before the engine checks it. Continue then starts the download at once; no page asks first. Under the
 * options, what the page must say ([sourceNote]): a folder refused or a copy failed, what a
 * picked folder holds, or why Continue waits. An import keeps the Project Diablo 2 in the folder as
 * it is, whatever Check for game updates says (the owner's rule of 2026-10-26), so the page has no
 * switch for it.
 */
@Composable
fun SourceScreen(
    view: DiabloSource.View,
    onSelect: (SourceChoice) -> Unit,
    onPickFolder: () -> Unit,
    onContinue: () -> Unit,
    onBack: () -> Unit,
) {
    val focus = remember { FocusRequester() }
    InitialFocus(focus)
    val progress = view.progress
    val copying = progress != null
    FlowShell(
        step = FlowStep.DOWNLOAD,
        title = stringResource(R.string.source_title),
        intro = stringResource(R.string.source_intro),
        // Not while a pick is copied in.
        onBack = onBack.takeUnless { copying },
        backButton = true,
        lead = if (progress != null) ({ Figure((progress * 100).toInt(), text(view.status)) }) else null,
        action = {
            CornerPill(stringResource(R.string.source_continue), onContinue, Kind.PRIMARY, enabled = view.canContinue, focusRequester = focus)
        },
    ) {
        Option(SourceChoice.FAST, R.string.source_fast, R.string.source_fast_body, view.selected, !copying, onSelect)
        Option(SourceChoice.BLIZZARD, R.string.source_blizzard, R.string.source_blizzard_body, view.selected, !copying, onSelect)
        Option(SourceChoice.EXISTING, R.string.source_existing, R.string.source_existing_body, view.selected, !copying, onSelect)
        if (view.selected == SourceChoice.EXISTING) {
            LauncherButton(
                stringResource(R.string.source_existing_pick_action), onPickFolder,
                Modifier.fillMaxWidth().padding(start = 48.dp, top = 4.dp, bottom = 8.dp), enabled = !copying,
            )
        }
        // Under the options and the folder button, in line with the options' words.
        when (sourceNote(view)) {
            Note.PROBLEM -> Banner(text(view.status), Palette.Red, Modifier.padding(start = 48.dp, top = 4.dp, bottom = 8.dp))
            Note.FOUND -> Box(Modifier.padding(start = 44.dp)) { StatusLine(Tone.OK, text(view.status), emptyList()) }
            Note.WAITS -> WaitNote(text(view.status), Modifier.padding(start = 44.dp))
            null -> {}
        }
    }
}

/**
 * The line the source page shows under its options, if any: a folder refused, too large or not
 * copied ([Note.PROBLEM], the engine's or the copy's words); what the accepted folder holds and
 * what Install does with it ([Note.FOUND]); why Continue waits ([Note.WAITS]: the Launcher is
 * starting). The copy's progress is the figure's caption; what Continue does next (start the
 * download, open the picker) would repeat the options and is not said.
 */
fun sourceNote(view: DiabloSource.View): Note? = when {
    view.progress != null -> null
    view.tone == Tone.ERROR -> Note.PROBLEM
    !view.canContinue -> Note.WAITS
    view.selected == SourceChoice.EXISTING && !view.continuePicks -> Note.FOUND
    else -> null
}

@Composable
private fun Option(choice: SourceChoice, title: Int, body: Int, selected: SourceChoice, enabled: Boolean, onSelect: (SourceChoice) -> Unit) {
    OptionRow(stringResource(title), stringResource(body), selected = choice == selected, onSelect = { onSelect(choice) }, enabled = enabled, tinted = true)
}
