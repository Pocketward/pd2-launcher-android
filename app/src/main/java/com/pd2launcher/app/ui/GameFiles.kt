package com.pd2launcher.app.ui

import android.content.ActivityNotFoundException
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.provider.DocumentsContract
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.relocation.BringIntoViewRequester
import androidx.compose.foundation.relocation.bringIntoViewRequester
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import com.pd2launcher.app.DocumentPaths
import com.pd2launcher.app.FileTransfer
import com.pd2launcher.app.FileTransfer.Choice
import com.pd2launcher.app.FileTransfer.Outcome
import com.pd2launcher.app.GameFilesProvider
import com.pd2launcher.app.R
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/** What the Game files rows show below them while an import or export is being set up. */
private sealed interface Step {
    /** Import: where in the game folder the picked [uris] go; [folder] is the one shown. */
    data class Destination(val uris: List<Uri>, val folder: String) : Step

    /** Import: [name] is already there. */
    data class Ask(val name: String, val answer: CompletableDeferred<Choice>) : Step

    /** Export: what to copy out; [folder] is the one shown, [selected] what is chosen anywhere. */
    data class Select(val folder: String, val selected: Set<String>) : Step
}

/**
 * Settings → App → Game files: Open in Files (the Files app at the "PD2 Launcher" root,
 * [GameFilesProvider]; the system file picker there when no Files app opens it), Import files…
 * (Android's picker, then a folder of the game folder) and Export files… (files and folders of
 * the game folder, then a folder through Android's picker).
 */
@Composable
fun ColumnScope.GameFilesRows() {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    var message by remember { mutableStateOf<String?>(null) }
    var step by remember { mutableStateOf<Step?>(null) }
    var exporting by remember { mutableStateOf<List<String>>(emptyList()) }
    val show: (Outcome) -> Unit = { outcome ->
        message = when (outcome) {
            is Outcome.Refused -> context.getString(
                when (outcome.refusal) {
                    DocumentPaths.Refusal.LAUNCHER_BUSY -> R.string.files_refused_busy
                    else -> R.string.files_refused_game_running
                },
            )
            is Outcome.Done -> context.getString(R.string.settings_files_copied, outcome.copied, outcome.skipped)
            is Outcome.Failed -> context.getString(R.string.settings_transfer_failed, outcome.reason)
        }
    }
    val pickFiles = rememberLauncherForActivityResult(ActivityResultContracts.OpenMultipleDocuments()) { uris ->
        if (uris.isNotEmpty()) step = Step.Destination(uris, DocumentPaths.ROOT)
    }
    val pickFolder = rememberLauncherForActivityResult(ActivityResultContracts.OpenDocumentTree()) { tree ->
        val ids = exporting
        exporting = emptyList()
        if (tree != null && ids.isNotEmpty()) scope.launch { show(withContext(Dispatchers.IO) { FileTransfer.export(context, ids, tree) }) }
    }
    // The fallback opens the system file picker at the game folder; what is picked there is not used.
    val browse = rememberLauncherForActivityResult(ActivityResultContracts.StartActivityForResult()) {}
    val documents = remember(step) { GameFilesProvider.documents(context) }

    ActionRow(
        stringResource(R.string.settings_open_files), stringResource(R.string.settings_open_files_body),
        stringResource(R.string.settings_open_files_action),
    ) {
        if (!openFilesApp(context)) {
            browse.launch(
                Intent(Intent.ACTION_OPEN_DOCUMENT).addCategory(Intent.CATEGORY_OPENABLE).setType("*/*")
                    .putExtra(DocumentsContract.EXTRA_INITIAL_URI, GameFilesProvider.gameUri(context)),
            )
        }
    }
    Rule(Modifier.padding(horizontal = 14.dp))
    ActionRow(
        stringResource(R.string.settings_import_files), stringResource(R.string.settings_import_files_body),
        stringResource(R.string.settings_import_files_action),
    ) {
        message = null
        step = null
        pickFiles.launch(arrayOf("*/*"))
    }
    Rule(Modifier.padding(horizontal = 14.dp))
    ActionRow(
        stringResource(R.string.settings_export_files), stringResource(R.string.settings_export_files_body),
        stringResource(R.string.settings_export_files_action),
    ) {
        message = null
        step = if (documents.available()) Step.Select(DocumentPaths.ROOT, emptySet()) else null
        if (step == null) message = context.getString(R.string.settings_no_game_folder)
    }

    when (val current = step) {
        is Step.Destination -> StepPanel(stringResource(R.string.settings_import_to, current.uris.size, shown(current.folder))) {
            FolderList(documents, current.folder, foldersOnly = true, selected = emptySet(), onOpen = { step = current.copy(folder = it) }, onToggle = {})
            Buttons {
                current.folder.takeIf { it != DocumentPaths.ROOT }?.let { folder ->
                    LauncherButton(stringResource(R.string.settings_files_up), { step = current.copy(folder = documents.parent(folder)!!) }, height = 48.dp, padding = 16.dp)
                }
                LauncherButton(stringResource(R.string.settings_files_cancel), { step = null }, height = 48.dp, padding = 16.dp, kind = Kind.QUIET)
                LauncherButton(stringResource(R.string.settings_files_copy_here), {
                    scope.launch {
                        val outcome = withContext(Dispatchers.IO) {
                            FileTransfer.import(context, current.uris, current.folder) { name ->
                                withContext(Dispatchers.Main) {
                                    val answer = CompletableDeferred<Choice>()
                                    step = Step.Ask(name, answer)
                                    answer.await()
                                }
                            }
                        }
                        step = null
                        show(outcome)
                    }
                }, height = 48.dp, padding = 16.dp, kind = Kind.PRIMARY)
            }
        }
        is Step.Ask -> StepPanel(stringResource(R.string.settings_files_exists, current.name)) {
            Buttons {
                for ((label, choice) in listOf(
                    R.string.settings_files_overwrite to Choice.OVERWRITE,
                    R.string.settings_files_skip to Choice.SKIP,
                    R.string.settings_files_keep_both to Choice.KEEP_BOTH,
                )) {
                    LauncherButton(stringResource(label), { current.answer.complete(choice) }, height = 48.dp, padding = 16.dp)
                }
            }
        }
        is Step.Select -> StepPanel(stringResource(R.string.settings_export_from, shown(current.folder))) {
            FolderList(
                documents, current.folder, foldersOnly = false, selected = current.selected,
                onOpen = { step = current.copy(folder = it) },
                onToggle = { id -> step = current.copy(selected = if (id in current.selected) current.selected - id else current.selected + id) },
            )
            Buttons {
                current.folder.takeIf { it != DocumentPaths.ROOT }?.let { folder ->
                    LauncherButton(stringResource(R.string.settings_files_up), { step = current.copy(folder = documents.parent(folder)!!) }, height = 48.dp, padding = 16.dp)
                }
                LauncherButton(stringResource(R.string.settings_files_cancel), { step = null }, height = 48.dp, padding = 16.dp, kind = Kind.QUIET)
                LauncherButton(
                    stringResource(R.string.settings_export_selected, current.selected.size),
                    {
                        exporting = current.selected.sorted()
                        step = null
                        pickFolder.launch(null)
                    },
                    height = 48.dp, padding = 16.dp, kind = Kind.PRIMARY, enabled = current.selected.isNotEmpty(),
                )
            }
        }
        null -> Unit
    }
    message?.let { StepPanel(it) {} }
}

/** A folder's path as the player sees it: `Diablo II/ProjectD2`. */
@Composable
private fun shown(folder: String): String =
    stringResource(R.string.files_game) + folder.removePrefix(DocumentPaths.ROOT)

/** A step's panel: what it asks, then its content; scrolled into view when it changes. */
@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun StepPanel(title: String, content: @Composable () -> Unit) {
    val requester = remember { BringIntoViewRequester() }
    LaunchedEffect(title) { requester.bringIntoView() }
    Column(
        Modifier.fillMaxWidth().bringIntoViewRequester(requester).padding(horizontal = 14.dp, vertical = 8.dp),
        verticalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        Text(title, style = Type.Body.copy(fontWeight = FontWeight.SemiBold), color = Palette.Text)
        content()
    }
}

@Composable
private fun Buttons(content: @Composable () -> Unit) {
    FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) { content() }
}

/**
 * The entries of [folder]: folders open with ›; with [foldersOnly] (a destination) only folders,
 * else every entry toggles in [selected] too.
 */
@Composable
private fun FolderList(
    documents: DocumentPaths,
    folder: String,
    foldersOnly: Boolean,
    selected: Set<String>,
    onOpen: (String) -> Unit,
    onToggle: (String) -> Unit,
) {
    val entries = remember(folder) {
        runCatching { documents.children(folder) }.getOrDefault(emptyList()).map { it to documents.file(it).isDirectory }
            .filter { (_, isFolder) -> isFolder || !foldersOnly }
            .sortedWith(compareBy({ !it.second }, { it.first.lowercase() }))
    }
    for ((id, isFolder) in entries) {
        val name = id.substringAfterLast('/')
        Row(verticalAlignment = Alignment.CenterVertically) {
            Column(Modifier.weight(1f)) {
                if (foldersOnly) {
                    FocusRow({ onOpen(id) }, minHeight = 48.dp) {
                        Text("$name/", style = Type.Body, color = Palette.Text, modifier = Modifier.weight(1f))
                        Text("›", style = Type.Title, color = Palette.Teal)
                    }
                } else {
                    ToggleRow(if (isFolder) "$name/" else name, "", id in selected, { onToggle(id) })
                }
            }
            if (isFolder && !foldersOnly) LauncherButton("›", { onOpen(id) }, height = 48.dp, padding = 16.dp, kind = Kind.QUIET)
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
