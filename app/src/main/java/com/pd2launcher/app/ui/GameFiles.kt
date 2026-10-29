package com.pd2launcher.app.ui

import android.content.ActivityNotFoundException
import android.content.Context
import android.content.Intent
import android.provider.DocumentsContract
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import com.pd2launcher.app.GameFilesProvider
import com.pd2launcher.app.R

/**
 * Settings → App → Game files: where the game folder is, with Open in Files (the Files app at
 * the "PD2 Launcher" root, [GameFilesProvider]; the system file picker there when no Files app
 * opens it). The player puts mods and loot filters there with the Files app; the characters
 * reach another folder through Save sync ([SaveSyncRows]).
 */
@Composable
fun ColumnScope.GameFilesRows() {
    val context = LocalContext.current
    // The fallback opens the system file picker at the game folder; what is picked there is not used.
    val browse = rememberLauncherForActivityResult(ActivityResultContracts.StartActivityForResult()) {}
    ActionRow(
        stringResource(R.string.settings_open_files),
        // Any file can go in the game folder, programs and libraries too: the row says so, as they run with the game.
        stringResource(R.string.settings_open_files_body),
        stringResource(R.string.settings_open_files_action),
    ) {
        if (!openFilesApp(context)) {
            browse.launch(
                Intent(Intent.ACTION_OPEN_DOCUMENT).addCategory(Intent.CATEGORY_OPENABLE).setType("*/*")
                    .putExtra(DocumentsContract.EXTRA_INITIAL_URI, GameFilesProvider.gameUri(context)),
            )
        }
    }
}

/** The Files app at the "PD2 Launcher" root (ACTION_VIEW of the root's URI, which DocumentsUI opens); false when nothing takes it. */
private fun openFilesApp(context: Context): Boolean = try {
    context.startActivity(
        Intent(Intent.ACTION_VIEW)
            .setDataAndType(GameFilesProvider.rootUri(context), DocumentsContract.Root.MIME_TYPE_ITEM)
            // A fresh Files task: a previous one would come back where it was left, not at the root.
            .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_CLEAR_TASK),
    )
    true
} catch (_: ActivityNotFoundException) {
    false
}

/** A title, what it does, and its button (below the words on phones). */
@Composable
private fun ActionRow(title: String, body: String, action: String, onClick: () -> Unit) {
    val phone = LocalFormFactor.current == FormFactor.PHONE
    val button = @Composable { modifier: Modifier ->
        LauncherButton(action, onClick, modifier, height = 48.dp, padding = 16.dp, style = Type.Hint.copy(fontWeight = FontWeight.SemiBold))
    }
    Row(
        Modifier.fillMaxWidth().heightIn(min = 64.dp).padding(horizontal = 14.dp, vertical = 10.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(14.dp),
    ) {
        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
            Text(title, style = Type.Body, color = Palette.Text)
            Text(body, style = Type.Hint, color = Palette.Muted)
            if (phone) button(Modifier.padding(top = 8.dp))
        }
        if (!phone) button(Modifier)
    }
}
