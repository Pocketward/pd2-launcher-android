package com.pd2launcher.app.ui

import android.content.Context
import android.content.Intent
import android.net.Uri
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.LocalIndication
import androidx.compose.foundation.clickable
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.interaction.collectIsFocusedAsState
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.produceState
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import com.pd2launcher.app.AppSettings
import com.pd2launcher.app.R
import com.pd2launcher.app.SaveSyncFolder
import com.pd2launcher.app.SessionService
import com.pd2launcher.app.Tone
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/**
 * Settings → App → Game files → Save sync ([SaveSyncFolder]): the switch, off by default, whose On
 * opens Android's folder picker (the setting stays off when the player picks nothing); How it
 * works, which shows when the app copies and what it does with two copies and with a delete;
 * while on, the folder with Change, and how the last sync went with Sync now. A pick, Change and
 * Sync now each have the session service sync ([SessionService.ACTION_SYNC_SAVES]). Off keeps
 * both copies.
 */
@Composable
fun ColumnScope.SaveSyncRows(app: AppSettings) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    remember { SaveSyncFolder.load(context) }
    val state by SaveSyncFolder.state.collectAsState()
    var message by remember { mutableStateOf<String?>(null) }
    var how by rememberSaveable { mutableStateOf(false) }
    // What a pick is for, in the log: the setting turned on, or another folder.
    var pickWhy by remember { mutableStateOf("turned on") }
    val pick = rememberLauncherForActivityResult(PickSyncFolder()) { uri ->
        if (uri == null) return@rememberLauncherForActivityResult
        val why = pickWhy
        scope.launch {
            val refusal = withContext(Dispatchers.IO) { SaveSyncFolder.choose(context, uri) }
            message = refusal?.let {
                context.getString(
                    when (it) {
                        SaveSyncFolder.Refusal.OWN_FOLDER -> R.string.sync_own_folder
                        SaveSyncFolder.Refusal.NO_ACCESS -> R.string.sync_no_access
                    },
                )
            }
            if (refusal == null) SessionService.send(context, SessionService.ACTION_SYNC_SAVES, syncWhy = why)
        }
    }
    val folder = app.syncFolder

    ToggleRow(
        stringResource(R.string.sync_switch), stringResource(R.string.sync_help), folder != null,
        { on ->
            message = null
            if (on) {
                pickWhy = "turned on"
                pick.launch(SaveSyncFolder.pickerStart(null))
            } else {
                scope.launch {
                    withContext(Dispatchers.IO) { SaveSyncFolder.turnOff(context) }
                    message = context.getString(R.string.sync_off)
                }
            }
        },
    )
    HowItWorks(how) { how = !how }
    if (how) {
        Column(Modifier.padding(start = 14.dp, end = 14.dp, bottom = 10.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
            for (line in listOf(R.string.sync_how_in, R.string.sync_how_out, R.string.sync_how_never, R.string.sync_how_both, R.string.sync_how_backups, R.string.sync_how_delete, R.string.sync_how_off)) {
                Text("•  " + stringResource(line), style = Type.Hint, color = Palette.Muted)
            }
        }
    }
    if (folder != null) {
        val label by produceState("", folder) { value = withContext(Dispatchers.IO) { SaveSyncFolder.label(context, Uri.parse(folder)) } }
        Rule(Modifier.padding(horizontal = 14.dp))
        ButtonRow(stringResource(R.string.sync_folder, label), null, stringResource(R.string.sync_folder_change)) {
            message = null
            pickWhy = "another folder"
            pick.launch(SaveSyncFolder.pickerStart(folder))
        }
        Rule(Modifier.padding(horizontal = 14.dp))
        // "… min ago" moves on with the clock.
        var now by remember { mutableLongStateOf(System.currentTimeMillis()) }
        LaunchedEffect(state) {
            while (true) {
                now = System.currentTimeMillis()
                delay(30_000)
            }
        }
        val (line, tone) = state.line(now)
        ButtonRow(text(line), tone, stringResource(R.string.sync_now), enabled = !state.running) {
            message = null
            SessionService.send(context, SessionService.ACTION_SYNC_SAVES, syncWhy = "Sync now")
        }
    }
    message?.let { Text(it, style = Type.Hint, color = Palette.Muted, modifier = Modifier.padding(horizontal = 14.dp, vertical = 8.dp)) }
}

/** Android's folder picker, from [input] (a folder to start in), local storage only (the device's and an SD card, no cloud). */
private class PickSyncFolder : ActivityResultContracts.OpenDocumentTree() {
    override fun createIntent(context: Context, input: Uri?): Intent = super.createIntent(context, input).putExtra(Intent.EXTRA_LOCAL_ONLY, true)
}

/** How it works, as one line: a tap, A or Enter shows its lines under it, again hides them. */
@Composable
private fun HowItWorks(shown: Boolean, onToggle: () -> Unit) {
    val interaction = remember { MutableInteractionSource() }
    val focused by interaction.collectIsFocusedAsState()
    val toggle = stringResource(if (shown) R.string.check_passed_hide else R.string.check_passed_show)
    Row(
        Modifier.fillMaxWidth().heightIn(min = 48.dp).focusMark(focused)
            .clickable(interactionSource = interaction, indication = LocalIndication.current, role = Role.Button, onClickLabel = toggle, onClick = onToggle)
            .padding(horizontal = 14.dp, vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(stringResource(R.string.sync_how), style = Type.Hint, color = Palette.Text, modifier = Modifier.weight(1f))
        Text(toggle, style = Type.Hint, color = Palette.Teal)
    }
}

/** A line of words, with its [tone]'s mark when there is one, and its button (below the words on phones). */
@Composable
private fun ButtonRow(words: String, tone: Tone?, action: String, enabled: Boolean = true, onClick: () -> Unit) {
    val phone = LocalFormFactor.current == FormFactor.PHONE
    val button = @Composable { modifier: Modifier ->
        LauncherButton(action, onClick, modifier, enabled = enabled, height = 48.dp, padding = 16.dp, style = Type.Hint.copy(fontWeight = FontWeight.SemiBold))
    }
    Row(
        Modifier.fillMaxWidth().heightIn(min = 64.dp).padding(horizontal = 14.dp, vertical = 10.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(14.dp),
    ) {
        tone?.let { StatusMark(it) }
        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
            Text(words, style = if (tone == null) Type.Body else Type.Hint, color = if (tone == null) Palette.Text else Palette.Muted)
            if (phone) button(Modifier.padding(top = 8.dp))
        }
        if (!phone) button(Modifier)
    }
}
