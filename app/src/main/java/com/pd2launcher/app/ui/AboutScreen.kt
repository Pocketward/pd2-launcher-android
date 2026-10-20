package com.pd2launcher.app.ui

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
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
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import com.pd2launcher.app.BuildConfig
import com.pd2launcher.app.R

/**
 * About, from Play and Settings → App: what PD2 Launcher is, the non-affiliation notice, the
 * major technology it is built with, and the app's version. [runtime]: the unpacked App userland's
 * version (Check's "Game support files"), its build id; null until it is unpacked.
 */
@Composable
fun AboutScreen(onBack: () -> Unit, runtime: String? = null) {
    BackHandler(onBack = onBack)
    val focus = remember { FocusRequester() }
    InitialFocus(focus)
    Column(Modifier.fillMaxSize().background(Palette.Background).windowInsetsPadding(WindowInsets.safeDrawing)) {
        Row(Modifier.heightIn(min = 56.dp).padding(horizontal = 8.dp), verticalAlignment = Alignment.CenterVertically) {
            LauncherButton(stringResource(R.string.about_back), onBack, kind = Kind.QUIET, height = 48.dp, focusRequester = focus, padding = 14.dp)
            ScreenTitle(stringResource(R.string.about_title), Modifier.padding(start = 4.dp))
        }
        Rule()
        Column(Modifier.weight(1f).fillMaxWidth().verticalScroll(rememberScrollState()), horizontalAlignment = Alignment.CenterHorizontally) {
            Column(
                Modifier.widthIn(max = 640.dp).fillMaxWidth().padding(horizontal = 20.dp, vertical = 16.dp),
                verticalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                Text(stringResource(R.string.app_name), style = Type.Title, color = Palette.Text)
                Text(stringResource(R.string.about_version, BuildConfig.VERSION_NAME), style = Type.Body, color = Palette.Muted)
                Text(stringResource(R.string.about_what), style = Type.Body, color = Palette.Text, modifier = Modifier.padding(top = 8.dp))
                Text(stringResource(R.string.about_disclaimer), style = Type.Hint, color = Palette.Muted)
                GroupLabel(stringResource(R.string.about_built_title), Modifier.padding(top = 16.dp))
                Text(stringResource(R.string.about_built_wine), style = Type.Body, color = Palette.Text)
                Text(stringResource(R.string.about_built_box64), style = Type.Body, color = Palette.Text)
                Text(stringResource(R.string.about_built_mesa), style = Type.Body, color = Palette.Text)
                Text(stringResource(R.string.about_built_dotnet), style = Type.Body, color = Palette.Text)
                if (runtime != null) Text(stringResource(R.string.about_runtime, runtime), style = Type.Hint, color = Palette.Muted)
                Text(stringResource(R.string.about_end), style = Type.Hint, color = Palette.Muted, modifier = Modifier.padding(top = 16.dp))
            }
        }
    }
}
