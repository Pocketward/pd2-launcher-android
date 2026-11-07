package com.pd2launcher.app.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import com.pd2launcher.app.Box64Build
import com.pd2launcher.app.Box64TestEnv
import com.pd2launcher.app.Line
import com.pd2launcher.app.R

/**
 * Settings → Testing (test builds only): the player's Box64 values ([choices], by knob name), the one
 * line above the rows while the game or the official PD2 launcher runs or the game starts ([lock],
 * as the App page's [com.pd2launcher.app.AppUpdate.lock]; the rows are grey then and take no press),
 * a pick ([onPick]: null is Box64's default) and Reset all ([onReset]: the knobs and the build). The
 * Box64 build ([build], [onBuild]) shows only when the APK has a second DLL ([other]).
 */
class TestingPageUi(
    val choices: Map<String, Int>,
    val lock: Line?,
    val onPick: (String, Int?) -> Unit,
    val onReset: () -> Unit,
    val build: Box64Build.Choice = Box64Build.Choice.PINNED,
    val other: Box64Build.Other? = null,
    val onBuild: (Box64Build.Choice) -> Unit = {},
)

/**
 * The warning line, the lock's line, then one row for each knob ([Box64TestEnv.KNOBS]): its name,
 * Box64's default under it, and a slim switch of Default and the knob's values. [first]: the first
 * switch's chosen option, which takes the page's first focus.
 */
@Composable
fun Box64KnobRows(ui: TestingPageUi, first: FocusRequester?) {
    val locked = ui.lock != null
    Text(
        stringResource(R.string.testing_warning), style = Type.Hint, color = Palette.Amber,
        modifier = Modifier.padding(start = 14.dp, end = 14.dp, top = 8.dp, bottom = 4.dp),
    )
    ui.lock?.let { Text(text(it), style = Type.Hint, color = Palette.Amber, modifier = Modifier.padding(start = 14.dp, end = 14.dp, bottom = 4.dp)) }
    ui.other?.let { other ->
        GroupLabel(stringResource(R.string.testing_box64_build), Modifier.padding(start = 14.dp), top = 8.dp)
        OptionRow(
            stringResource(R.string.testing_build_pinned, Box64Build.PINNED_COMMIT, Box64Build.PINNED_DATE), null,
            ui.build == Box64Build.Choice.PINNED, { ui.onBuild(Box64Build.Choice.PINNED) }, enabled = !locked,
        )
        OptionRow(
            stringResource(R.string.testing_build_upstream, other.short, other.date), null,
            ui.build == Box64Build.Choice.UPSTREAM, { ui.onBuild(Box64Build.Choice.UPSTREAM) }, enabled = !locked,
        )
        GroupLabel(stringResource(R.string.testing_settings), Modifier.padding(start = 14.dp), top = 16.dp)
    }
    val default = stringResource(R.string.testing_default)
    Box64TestEnv.KNOBS.forEachIndexed { index, knob ->
        if (index > 0) Rule(Modifier.padding(horizontal = 14.dp))
        val options = listOf(default) + knob.values.map { it.toString() }
        val picked = ui.choices[knob.name]
        Column(
            Modifier.fillMaxWidth().heightIn(min = 56.dp).padding(horizontal = 14.dp, vertical = 4.dp),
            verticalArrangement = Arrangement.spacedBy(0.dp),
        ) {
            Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                Text(knob.name, style = Type.Body, color = if (locked) Palette.Faint else Palette.Text, modifier = Modifier.weight(1f))
                Text(stringResource(R.string.testing_box64_default, knob.default), style = Type.Hint, color = Palette.Faint, maxLines = 1)
            }
            SlimSwitch(
                options, picked?.let { knob.values.indexOf(it) + 1 } ?: 0,
                { option -> ui.onPick(knob.name, if (option == 0) null else knob.values[option - 1]) },
                selectedFocus = first.takeIf { index == 0 }, enabled = !locked,
            )
        }
    }
}

/** Reset all: every knob back to Box64's default and the build back to Pinned; grey while the rows are locked. */
@Composable
fun Box64ResetRow(ui: TestingPageUi) {
    Row(
        Modifier.fillMaxWidth().heightIn(min = 56.dp).padding(horizontal = 14.dp, vertical = 4.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        SlimButton(stringResource(R.string.testing_reset_all), ui.onReset, enabled = ui.lock == null)
    }
}
