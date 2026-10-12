package com.pd2launcher.app.ui

import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import com.pd2launcher.app.DownloadAction
import com.pd2launcher.app.DownloadState
import com.pd2launcher.app.R
import com.pd2launcher.app.Screen
import com.pd2launcher.app.Tone

/** The device check runs, before anything is unpacked. */
@Composable
fun CheckingDeviceScreen() {
    FlowShell(
        step = FlowStep.CHECK,
        title = stringResource(R.string.device_checking_title),
        intro = stringResource(R.string.device_checking_body),
        action = { CornerPill(stringResource(R.string.action_continue), {}, Kind.PRIMARY, enabled = false) },
    ) {
        Spacer(Modifier.height(12.dp))
        Bar(null, Modifier.padding(horizontal = 4.dp))
        Spacer(Modifier.height(4.dp))
        WaitNote(stringResource(R.string.hint_wait_for_check))
    }
}

@Composable
fun NotSupportedScreen(screen: Screen.NotSupported, onClose: () -> Unit) {
    val focus = remember { FocusRequester() }
    InitialFocus(focus)
    FlowShell(
        step = FlowStep.CHECK,
        failed = true,
        title = stringResource(R.string.not_supported_title),
        intro = stringResource(R.string.not_supported_body),
        action = { CornerPill(stringResource(R.string.not_supported_close), onClose, Kind.PRIMARY, focusRequester = focus) },
    ) {
        val findings = screen.problems.map { Tone.ERROR to it } + screen.warnings.map { Tone.WARNING to it }
        findings.forEachIndexed { index, (tone, line) ->
            if (index > 0) Rule()
            StatusLine(tone, text(line), emptyList())
        }
    }
}

/**
 * Step 2, Download: the engine's figure and the item downloading now; Stop keeps what is done,
 * Resume and Try again continue, Continue goes on to Install once it is done. [onBack] goes back
 * to the Diablo II source; null while the download runs, and never shown then ([backShown]).
 * Under the items, what the page must say ([downloadNote]): why it failed, or why Download waits.
 */
@Composable
fun DownloadScreen(
    screen: Screen.Download,
    onDownload: () -> Unit,
    onStopSafely: () -> Unit,
    onContinue: () -> Unit = {},
    onBack: (() -> Unit)? = null,
) {
    val action = screen.action
    val focus = remember { FocusRequester() }
    InitialFocus(focus, action)
    val state = screen.state
    val working = downloadWorking(state)
    val (title, intro) = when (state) {
        DownloadState.WAITING, DownloadState.READY -> R.string.download_title to R.string.download_intro
        DownloadState.RUNNING -> R.string.download_title_running to R.string.download_intro_running
        DownloadState.STOPPING -> R.string.download_title_stopping to R.string.download_intro_running
        DownloadState.STOPPED -> R.string.download_title_stopped to R.string.download_intro_stopped
        DownloadState.FAILED -> R.string.download_title_failed to R.string.download_intro_failed
        DownloadState.DONE -> R.string.download_title_done to R.string.download_intro_done
    }
    val amount = screen.amount?.let { text(it) }
    val current = screen.current?.takeIf { working }
    val color = when (state) {
        DownloadState.FAILED -> Palette.Red
        DownloadState.STOPPED -> Palette.Amber
        else -> Palette.Teal
    }
    val percent = if (state == DownloadState.DONE) 100 else screen.progress?.let { (it * 100).toInt() }
    FlowShell(
        step = FlowStep.DOWNLOAD,
        failed = state == DownloadState.FAILED,
        title = stringResource(title),
        intro = stringResource(intro),
        onBack = onBack,
        backButton = backShown(onBack != null, working),
        progress = stepProgress(working, percent),
        lead = if (working || state == DownloadState.DONE) ({
            // The one bar, the item downloading now's; its caption the amount, speed and time
            // left. The item's name is its row's.
            Figure(
                percent = percent,
                caption = current?.let { screen.summary ?: amount },
                color = color, moving = working,
            )
        }) else null,
        action = {
            when (action) {
                DownloadAction.DOWNLOAD ->
                    CornerPill(stringResource(R.string.action_download), onDownload, Kind.PRIMARY, focusRequester = focus)
                DownloadAction.DOWNLOAD_AGAIN -> CornerPill(
                    stringResource(if (state == DownloadState.FAILED) R.string.action_try_again else R.string.action_resume),
                    onDownload, Kind.PRIMARY, focusRequester = focus,
                )
                DownloadAction.STOP_SAFELY ->
                    CornerPill(stringResource(R.string.action_stop), onStopSafely, focusRequester = focus)
                DownloadAction.CONTINUE ->
                    CornerPill(stringResource(R.string.action_continue), onContinue, Kind.PRIMARY, focusRequester = focus)
                DownloadAction.NONE -> CornerPill(
                    stringResource(if (state == DownloadState.STOPPING) R.string.action_stopping else R.string.action_download),
                    {}, if (state == DownloadState.STOPPING) Kind.SECONDARY else Kind.PRIMARY, enabled = false, focusRequester = focus,
                )
            }
        },
    ) {
        screen.items.forEachIndexed { index, row ->
            if (index > 0) Rule()
            // How the item stands; its bytes, speed and time left are the figure's.
            val detail = when (row.state) {
                TaskState.WAITING -> null
                TaskState.RUNNING -> listOf(
                    stringResource(if (state == DownloadState.STOPPING) R.string.download_title_stopping else R.string.download_row_downloading),
                )
                TaskState.DONE -> listOf(stringResource(R.string.download_row_done))
                TaskState.STOPPED -> listOf(stringResource(R.string.download_stopped_item))
                TaskState.FAILED -> listOf(stringResource(R.string.download_failed_item))
            }
            StatusLine(
                tone = when (row.state) {
                    TaskState.WAITING -> Tone.PENDING
                    TaskState.RUNNING -> Tone.WORKING
                    TaskState.DONE -> Tone.OK
                    TaskState.STOPPED -> Tone.WARNING
                    TaskState.FAILED -> Tone.ERROR
                },
                title = stringResource(row.item.label),
                details = detail.orEmpty(),
                dim = row.state == TaskState.WAITING,
            )
        }
        when (downloadNote(state)) {
            Note.PROBLEM -> Banner(text(screen.hint), Palette.Red, Modifier.padding(vertical = 8.dp))
            Note.WAITS -> WaitNote(text(screen.hint))
            Note.FOUND, null -> {}
        }
    }
}

/** A download or its safe stop runs: the figure and Stop show, Back does not. */
fun downloadWorking(state: DownloadState): Boolean = state == DownloadState.RUNNING || state == DownloadState.STOPPING

/**
 * What Download says under its items: why it failed, with Try again (the engine's words), or why
 * Download waits (the Launcher is starting). The other hints would repeat the title or intro.
 */
fun downloadNote(state: DownloadState): Note? = when (state) {
    DownloadState.FAILED -> Note.PROBLEM
    DownloadState.WAITING -> Note.WAITS
    else -> null
}
