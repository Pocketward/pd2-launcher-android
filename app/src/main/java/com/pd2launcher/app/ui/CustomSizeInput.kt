package com.pd2launcher.app.ui

import android.view.Gravity
import android.view.WindowManager
import android.widget.EditText
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.widthIn
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalView
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import androidx.compose.ui.window.DialogWindowProvider
import com.pd2launcher.app.GameResolution
import com.pd2launcher.app.R

/**
 * The game resolution's Custom input: Width and Height
 * fields for Android's number keyboard, the size a Play uses now in them ([current], the kept
 * Custom size, else the game's own size on the landscape [display]), and OK and Cancel. OK takes a
 * size from [GameResolution.CUSTOM_MIN] to the display's own, the width even and the height a
 * multiple of 4 ([GameResolution.typedCustom]), and gives [onSet] the size to keep (none for the
 * game's own size, [GameResolution.customPick]); any other size shows one line and the dialog
 * stays. At the top of the screen, so the keyboard does not cover it.
 */
@Composable
internal fun CustomSizeInput(current: Pair<Int, Int>?, display: Pair<Int, Int>, onSet: (Pair<Int, Int>?) -> Unit, onClose: () -> Unit) {
    val (displayWidth, displayHeight) = display
    val (minWidth, minHeight) = GameResolution.CUSTOM_MIN
    val (maxWidth, maxHeight) = GameResolution.customMax(displayWidth, displayHeight)
    val (shownWidth, shownHeight) = GameResolution.forPlay(displayWidth, displayHeight, current)
    var wrong by remember { mutableStateOf(false) }
    var widthField by remember { mutableStateOf<EditText?>(null) }
    var heightField by remember { mutableStateOf<EditText?>(null) }
    val accept = {
        val size = GameResolution.typedCustom(
            widthField?.text?.toString().orEmpty(), heightField?.text?.toString().orEmpty(), displayWidth, displayHeight,
        )
        if (size == null) {
            wrong = true
        } else {
            onSet(GameResolution.customPick(size, displayWidth, displayHeight))
            onClose()
        }
    }
    // The fields are made once: their keyboard's Done calls the newest accept, with the newest onSet.
    val latestAccept by rememberUpdatedState(accept)
    Dialog(onClose, DialogProperties(usePlatformDefaultWidth = false)) {
        // At the top, the keyboard below: the window resizes for it rather than moving the fields.
        val window = (LocalView.current.parent as? DialogWindowProvider)?.window
        DisposableEffect(window) {
            window?.setGravity(Gravity.TOP)
            window?.setSoftInputMode(WindowManager.LayoutParams.SOFT_INPUT_ADJUST_RESIZE or WindowManager.LayoutParams.SOFT_INPUT_STATE_VISIBLE)
            // A controller's A selects and B is Back here too.
            val base = window?.let(::withNavigationKeys)
            onDispose { if (window != null && base != null) window.callback = base }
        }
        Panel(Modifier.padding(top = 12.dp, start = 16.dp, end = 16.dp).widthIn(max = 480.dp).fillMaxWidth()) {
            Text(stringResource(R.string.settings_section_resolution), style = Type.Body, color = Palette.Text)
            Row(
                Modifier.fillMaxWidth().padding(top = 8.dp),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                Text(stringResource(R.string.settings_resolution_custom_width), style = Type.Hint, color = Palette.Muted)
                AndroidView(
                    factory = { context ->
                        numberField(
                            context, shownWidth.toString(), 4, "$minWidth–$maxWidth",
                            onNext = { heightField?.let { it.requestFocus(); it.selectAll() } },
                            onDone = { latestAccept() },
                        ).also { widthField = it }
                    },
                    modifier = Modifier.weight(1f).height(48.dp),
                )
                Text("×", style = Type.Body, color = Palette.Muted)
                Text(stringResource(R.string.settings_resolution_custom_height), style = Type.Hint, color = Palette.Muted)
                AndroidView(
                    factory = { context ->
                        numberField(context, shownHeight.toString(), 4, "$minHeight–$maxHeight", opens = false, onDone = { latestAccept() }).also { heightField = it }
                    },
                    modifier = Modifier.weight(1f).height(48.dp),
                )
            }
            Row(
                Modifier.fillMaxWidth().padding(top = 8.dp),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(8.dp, Alignment.End),
            ) {
                LauncherButton(stringResource(R.string.settings_frame_rate_cancel), onClose, kind = Kind.QUIET, height = 48.dp, padding = 14.dp)
                LauncherButton(stringResource(R.string.settings_frame_rate_ok), accept, kind = Kind.PRIMARY, height = 48.dp, padding = 18.dp)
            }
            Text(
                stringResource(
                    if (wrong) R.string.settings_resolution_custom_wrong else R.string.settings_resolution_custom_range,
                    minWidth, minHeight, maxWidth, maxHeight,
                ),
                style = Type.Hint, color = if (wrong) Palette.Amber else Palette.Muted, modifier = Modifier.padding(top = 6.dp),
            )
        }
    }
}
