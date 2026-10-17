package com.pd2launcher.app.ui

import androidx.annotation.StringRes
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import com.pd2launcher.app.R

/**
 * Share logs and Save logs as the screens show them: which of the two ran last ([action]), how it
 * went ([state]), and [onShare] and [onSave], which start one.
 */
class LogShareUi(val onShare: () -> Unit, val onSave: () -> Unit) {
    enum class Action { SHARE, SAVE }
    enum class State { IDLE, WORKING, FAILED, SAVED }

    var action by mutableStateOf(Action.SHARE)
    var state by mutableStateOf(State.IDLE)

    /** How the last one ended is said only where it ran: leaving that screen forgets it, a running one goes on. */
    fun forgetEnd() {
        if (state == State.FAILED || state == State.SAVED) state = State.IDLE
    }
}

/** Share logs and Save logs for the screens that offer them; null where the app provides none (previews). */
val LocalLogShare = staticCompositionLocalOf<LogShareUi?> { null }

/** What the screens say about the last share or save; null when there is nothing to say. */
@StringRes
private fun LogShareUi.note(): Int? = when (state) {
    LogShareUi.State.IDLE -> null
    LogShareUi.State.WORKING -> R.string.share_logs_working
    LogShareUi.State.FAILED -> if (action == LogShareUi.Action.SAVE) R.string.save_logs_failed else R.string.share_logs_failed
    LogShareUi.State.SAVED -> R.string.save_logs_saved
}

/** The colour of [note]: a failure red, a save teal, else muted. */
private fun LogShareUi.noteColor() = when (state) {
    LogShareUi.State.FAILED -> Palette.Red
    LogShareUi.State.SAVED -> Palette.Teal
    else -> Palette.Muted
}

/** Forgets how the last share or save ended when the screen that shows it goes ([LogShareUi.forgetEnd]). */
@Composable
private fun ForgetEndOnLeave(share: LogShareUi) {
    DisposableEffect(share) { onDispose { share.forgetEnd() } }
}

/**
 * Share logs and Save logs under a failure on Check, Download and Install, and under Install's tasks
 * while it runs or is paused: two quiet links, and how the last one went.
 */
@Composable
fun ShareLogsLink(modifier: Modifier = Modifier) {
    val share = LocalLogShare.current ?: return
    ForgetEndOnLeave(share)
    val note = share.note()
    Column(modifier) {
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            TextLink(stringResource(R.string.share_logs), share.onShare)
            TextLink(stringResource(R.string.save_logs), share.onSave)
        }
        if (note != null) Text(stringResource(note), style = Type.Hint, color = share.noteColor(), modifier = Modifier.padding(horizontal = 10.dp))
    }
}

/**
 * Share logs or Save logs ([action]) in Settings → App: a row that says what it does, or how it went
 * last; while either is prepared, both rows say so, as neither starts meanwhile.
 */
@Composable
fun ShareLogsRow(action: LogShareUi.Action) {
    val share = LocalLogShare.current ?: return
    ForgetEndOnLeave(share)
    val (title, body, onClick) = when (action) {
        LogShareUi.Action.SHARE -> Triple(R.string.share_logs, R.string.share_logs_body, share.onShare)
        LogShareUi.Action.SAVE -> Triple(R.string.save_logs, R.string.save_logs_body, share.onSave)
    }
    val note = if (share.action == action || share.state == LogShareUi.State.WORKING) share.note() else null
    FocusRow(onClick) {
        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
            Text(stringResource(title), style = Type.Body, color = Palette.Text)
            Text(stringResource(note ?: body), style = Type.Hint, color = if (note != null) share.noteColor() else Palette.Muted)
        }
        Text("›", style = Type.Title, color = Palette.Teal)
    }
}
