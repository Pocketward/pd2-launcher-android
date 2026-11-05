package com.pd2launcher.app.ui

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
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
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.paneTitle
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import com.pd2launcher.app.R

/**
 * The device check's question for a GPU that Turnip and PanVK do not support where another route
 * can run ([com.pd2launcher.app.PhoneDriverAdvice]; the user's request of 2026-10-24): what the
 * player can expect (slower, or the game may not work at all), that Blizzard's installers are the
 * least likely to work on this phone (the owner, 2026-10-29; Fast download and an import are
 * equal, so the text names neither), then Try anyway ([onTryAnyway], which goes on) or Close
 * ([onClose], which does not). [newLauncherDriver]: PD2 Launcher's RADV, mali-panvk or libhybris tries first
 * ([com.pd2launcher.app.DriverOptions.newLauncherDriver]), so the text says that PD2 Launcher's
 * driver for the chip is new, not that it has none. Only "PD2 Launcher's driver" and "native
 * driver", no route names (the owner, 2026-10-29). [forced]: a debug build forced the route where
 * the built-in driver runs; one more line says so. An amber "EXPERIMENTAL" label stands over the
 * title. A sheet from the bottom on phones, a panel in the middle on handhelds; Try anyway has
 * the focus, and Back (a controller's B) is Close. The text scrolls on a low screen.
 */
@Composable
fun PhoneDriverDialog(onTryAnyway: () -> Unit, onClose: () -> Unit, forced: Boolean = false, newLauncherDriver: Boolean = false) {
    BackHandler(onBack = onClose)
    val phone = LocalFormFactor.current == FormFactor.PHONE
    val focus = remember { FocusRequester() }
    InitialFocus(focus)
    val title = stringResource(if (newLauncherDriver) R.string.phone_driver_title_launcher else R.string.phone_driver_title)
    Box(Modifier.fillMaxSize()) {
        Box(Modifier.matchParentSize().background(Color.Black.copy(alpha = 0.55f)).clickable(remember { MutableInteractionSource() }, null) {})
        Column(
            (if (phone) Modifier.align(Alignment.BottomCenter).fillMaxWidth() else Modifier.align(Alignment.Center).widthIn(max = 520.dp))
                .windowInsetsPadding(WindowInsets.safeDrawing).padding(12.dp)
                .clip(RowShape).background(Palette.Surface).border(1.dp, Palette.Rule, RowShape)
                .padding(horizontal = 20.dp, vertical = 18.dp)
                .semantics { paneTitle = title },
            verticalArrangement = Arrangement.spacedBy(10.dp),
        ) {
            Column(Modifier.weight(1f, fill = false).verticalScroll(rememberScrollState()), verticalArrangement = Arrangement.spacedBy(10.dp)) {
                // "EXPERIMENTAL" over the title, in amber (the user's request of 2026-10-24).
                Column(verticalArrangement = Arrangement.spacedBy(2.dp)) {
                    Text(stringResource(R.string.phone_driver_experimental), style = Type.Label, color = Palette.Amber)
                    Text(title, style = Type.Title, color = Palette.Text)
                }
                Text(stringResource(if (newLauncherDriver) R.string.phone_driver_body_launcher else R.string.phone_driver_body), style = Type.Body, color = Palette.Text)
                if (forced) Text(stringResource(R.string.phone_driver_forced), style = Type.Hint, color = Palette.Amber)
            }
            Spacer(Modifier.heightIn(min = 4.dp))
            Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(10.dp, Alignment.End)) {
                LauncherButton(stringResource(R.string.not_supported_close), onClose, kind = Kind.QUIET, height = 48.dp, padding = 18.dp)
                LauncherButton(stringResource(R.string.phone_driver_try), onTryAnyway, kind = Kind.PRIMARY, focusRequester = focus, height = 48.dp, padding = 18.dp)
            }
        }
    }
}
