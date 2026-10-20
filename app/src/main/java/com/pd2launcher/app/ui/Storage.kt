package com.pd2launcher.app.ui

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawing
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.State
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.produceState
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import com.pd2launcher.app.DeviceCheck
import com.pd2launcher.app.GameFilesProvider
import com.pd2launcher.app.InstallerDownloads
import com.pd2launcher.app.InstallerDownloads.Outcome
import com.pd2launcher.app.InstallerDownloads.Refusal
import com.pd2launcher.app.R
import com.pd2launcher.app.SessionPaths
import com.pd2launcher.app.SessionService
import com.pd2launcher.app.SourceImport
import com.pd2launcher.app.UpgradeImport
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

/** The installer downloads' size now ([InstallerDownloads.bytes]), counted again when [key] changes; null while counting. */
@Composable
fun rememberInstallerDownloadBytes(key: Any?): State<Long?> {
    val context = LocalContext.current
    return produceState<Long?>(null, key) {
        value = withContext(Dispatchers.IO) {
            runCatching { InstallerDownloads.bytes(SessionPaths(DeviceCheck.dataDir(context)).managedRoot) }.getOrNull()
        }
    }
}

/**
 * Whether the installation was imported from a folder the notice and Settings speak of
 * ([InstallerDownloads.imported], [UpgradeImport.playerCopied] with the engine's [gameFolder] and the
 * Launcher's record, and [InstallerDownloads.importedFolderShown]: not another app's folder), read
 * again when [key] or [gameFolder] changes; null while reading.
 */
@Composable
fun rememberImported(key: Any?, gameFolder: String?): State<Boolean?> {
    val context = LocalContext.current
    return produceState<Boolean?>(null, key, gameFolder) {
        value = withContext(Dispatchers.IO) {
            runCatching {
                val paths = SessionPaths(DeviceCheck.dataDir(context))
                InstallerDownloads.importedFolderShown(
                    InstallerDownloads.imported(paths.managedRoot) &&
                        UpgradeImport.playerCopied(
                            gameFolder, GameFilesProvider.recordedGameFolder(paths), paths.upgradeGame, SourceImport.pickedFolder(context),
                        ),
                    SourceImport.pickedFrom(context),
                )
            }.getOrNull()
        }
    }
}

/**
 * The one-time notice over Play after the first game session: it offers to free the installer
 * downloads and, after an import, says what to know about the picked folder
 * ([InstallerDownloads.notice]): one from Android's own storage can be deleted; for Lite's
 * folder, Lite still has its own copy. A sheet from the bottom on phones, a panel in the middle on
 * handhelds; Back is Later (or OK).
 */
@Composable
fun DownloadsNotice(notice: InstallerDownloads.Notice, onRemove: () -> Unit, onLater: () -> Unit) {
    BackHandler(onBack = onLater)
    val phone = LocalFormFactor.current == FormFactor.PHONE
    val remove = notice.remove
    val focus = remember { FocusRequester() }
    InitialFocus(focus)
    Box(Modifier.fillMaxSize()) {
        Box(Modifier.matchParentSize().background(Color.Black.copy(alpha = 0.55f)).clickable(remember { MutableInteractionSource() }, null) {})
        Column(
            (if (phone) Modifier.align(Alignment.BottomCenter).fillMaxWidth() else Modifier.align(Alignment.Center).widthIn(max = 460.dp))
                .windowInsetsPadding(WindowInsets.safeDrawing).padding(12.dp)
                .clip(RowShape).background(Palette.Surface).border(1.dp, Palette.Rule, RowShape)
                .padding(horizontal = 20.dp, vertical = 18.dp),
            verticalArrangement = Arrangement.spacedBy(10.dp),
        ) {
            GroupLabel(stringResource(R.string.settings_section_storage))
            Text(text(notice.title), style = Type.Title, color = Palette.Text)
            notice.body.forEach { Text(text(it), style = Type.Body, color = Palette.Text) }
            if (remove != null) Text(stringResource(R.string.downloads_notice_where), style = Type.Hint, color = Palette.Muted)
            Spacer(Modifier.heightIn(min = 4.dp))
            Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(10.dp, Alignment.End)) {
                if (remove != null) {
                    LauncherButton(stringResource(R.string.downloads_notice_later), onLater, kind = Kind.QUIET, focusRequester = focus, height = 48.dp, padding = 18.dp)
                    LauncherButton(text(remove), onRemove, kind = Kind.PRIMARY, height = 48.dp, padding = 18.dp)
                } else {
                    LauncherButton(stringResource(R.string.downloads_notice_ok), onLater, kind = Kind.PRIMARY, focusRequester = focus, height = 48.dp, padding = 18.dp)
                }
            }
        }
    }
}

/**
 * Settings → App → Storage: the installer downloads' size and Remove, with a confirmation in
 * place; after an import from Android's own storage, that the copied Diablo II folder can go, and
 * after an import of Lite's game folder, that Lite still has its own copy.
 */
@Composable
fun ColumnScope.StorageRows() {
    val status by SessionService.status.collectAsState()
    var counted by remember { mutableIntStateOf(0) }
    val bytes by rememberInstallerDownloadBytes(counted)
    val imported by rememberImported(status.snapshot?.installation, status.snapshot?.gameFolder?.path)
    var confirm by remember { mutableStateOf(false) }
    var working by remember { mutableStateOf(false) }
    var message by remember { mutableStateOf<String?>(null) }
    val context = LocalContext.current
    val known = bytes
    val remove = {
        confirm = false
        val refusal = InstallerDownloads.refusal(status.snapshot)
        if (refusal != null) {
            message = context.getString(refusalText(refusal))
        } else {
            working = true
            message = null
            val freed = InstallerDownloads.size(known ?: 0)
            SessionService.removeInstallerDownloads { outcome ->
                working = false
                message = when (outcome) {
                    Outcome.Removed -> context.getString(R.string.settings_downloads_removed, freed)
                    is Outcome.Refused -> context.getString(refusalText(outcome.refusal))
                    is Outcome.Failed -> context.getString(R.string.downloads_failed, outcome.reason)
                }
                counted++
            }
        }
    }
    Row(
        Modifier.fillMaxWidth().heightIn(min = 64.dp).padding(horizontal = 14.dp, vertical = 10.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(14.dp),
    ) {
        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
            Text(stringResource(R.string.settings_downloads), style = Type.Body, color = Palette.Text)
            Text(
                when {
                    working -> stringResource(R.string.settings_downloads_removing)
                    known == null -> stringResource(R.string.settings_downloads_counting)
                    known == 0L -> stringResource(R.string.settings_downloads_none)
                    else -> stringResource(R.string.settings_downloads_body, InstallerDownloads.size(known))
                },
                style = Type.Hint, color = Palette.Muted,
            )
        }
        if (known != null && known > 0 && !working && !confirm) {
            LauncherButton(
                stringResource(R.string.settings_downloads_remove), { message = null; confirm = true },
                height = 48.dp, padding = 16.dp, style = Type.Hint.copy(fontWeight = FontWeight.SemiBold),
            )
        }
    }
    if (confirm && known != null) {
        Column(Modifier.fillMaxWidth().padding(horizontal = 14.dp, vertical = 6.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Text(stringResource(R.string.settings_downloads_confirm, InstallerDownloads.size(known)), style = Type.Body, color = Palette.Text)
            FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                LauncherButton(stringResource(R.string.settings_downloads_cancel), { confirm = false }, kind = Kind.QUIET, height = 48.dp, padding = 16.dp)
                LauncherButton(stringResource(R.string.settings_downloads_remove), remove, kind = Kind.DANGER, height = 48.dp, padding = 16.dp)
            }
        }
    }
    message?.let { Text(it, style = Type.Hint, color = Palette.Text, modifier = Modifier.padding(horizontal = 14.dp, vertical = 6.dp)) }
    if (imported == true) {
        val folder = remember { SourceImport.pickedFolder(context) }
        val from = remember { SourceImport.pickedFrom(context) }
        InstallerDownloads.copiedFolder(folder, from)?.let {
            Text(
                text(it), style = Type.Hint, color = Palette.Muted,
                modifier = Modifier.padding(horizontal = 14.dp, vertical = 6.dp),
            )
        }
    }
}

private fun refusalText(refusal: Refusal): Int = when (refusal) {
    Refusal.GAME_RUNNING -> R.string.downloads_refused_game_running
    Refusal.BUSY -> R.string.downloads_refused_busy
    Refusal.NOT_READY -> R.string.downloads_refused_not_ready
}
