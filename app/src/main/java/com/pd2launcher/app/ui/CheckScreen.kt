package com.pd2launcher.app.ui

import androidx.compose.foundation.LocalIndication
import androidx.compose.foundation.clickable
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.interaction.collectIsFocusedAsState
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.unit.dp
import com.pd2launcher.app.CheckRow
import com.pd2launcher.app.Grant
import com.pd2launcher.app.Line
import com.pd2launcher.app.MainAction
import com.pd2launcher.app.R
import com.pd2launcher.app.Screen
import com.pd2launcher.app.Tone

/**
 * Step 1, Check, in plain words: this device, notifications, battery, the game support files and
 * whether it can run the game. The rows that passed fold into one line ("3 checks passed") that
 * opens in place; what needs the player (an ask, a warning, a problem) and what still runs stay
 * open. A row Android lets the app fix has its button ([onGrant]); the row follows once the
 * player is back. [onRepair]: the installation's game folder is gone, Repair (Start over) goes on.
 */
@Composable
fun CheckScreen(
    screen: Screen.Check,
    onCheckAgain: () -> Unit,
    onContinue: () -> Unit,
    onRepair: () -> Unit = {},
    onGrant: (Grant) -> Unit = {},
) {
    val action = screen.action
    val focus = remember { FocusRequester() }
    InitialFocus(focus, action.kind, action.enabled)
    val tone = screen.tone
    var showPassed by rememberSaveable { mutableStateOf(false) }
    FlowShell(
        step = FlowStep.CHECK,
        failed = tone == Tone.ERROR,
        title = text(screen.title),
        intro = stringResource(screen.intro),
        action = {
            val (label, onClick) = when (action.kind) {
                MainAction.Kind.CHECK_AGAIN -> R.string.action_check_again to onCheckAgain
                MainAction.Kind.CONTINUE -> R.string.action_continue to onContinue
                MainAction.Kind.REPAIR -> R.string.play_repair to onRepair
            }
            CornerPill(stringResource(label), onClick, Kind.PRIMARY, enabled = action.enabled, focusRequester = focus)
        },
    ) {
        val passed = screen.passed
        if (passed.isNotEmpty()) {
            PassedLine(passed.size, showPassed) { showPassed = !showPassed }
            if (showPassed) {
                passed.forEach { row ->
                    Rule()
                    CheckLine(row, onGrant)
                }
            }
        }
        screen.open.forEachIndexed { index, row ->
            if (index > 0 || passed.isNotEmpty()) Rule()
            CheckLine(row, onGrant)
        }
        checkWaits(action)?.let { WaitNote(text(it)) }
    }
}

/**
 * What Check says beside its rows: only why Continue waits (the checks still run). A problem is
 * its row's, and the title and intro say the rest.
 */
fun checkWaits(action: MainAction): Line? = action.hint.takeUnless { action.enabled }

/** One check: its mark, label and lines, its grant button, and its bar while it unpacks. */
@Composable
private fun CheckLine(row: CheckRow, onGrant: (Grant) -> Unit) {
    val grant = row.grant
    StatusLine(
        row.tone, stringResource(row.label), row.lines.map { text(it) },
        action = grant?.let {
            {
                LauncherButton(
                    stringResource(if (it == Grant.NOTIFICATIONS) R.string.check_grant_notifications else R.string.check_grant_battery),
                    { onGrant(it) }, height = 44.dp, padding = 18.dp,
                )
            }
        },
    )
    row.progress?.let { Bar(it, Modifier.padding(start = 38.dp, end = 4.dp, bottom = 10.dp), height = 3.dp) }
}

/** The passed checks as one line: a tap, A or Enter shows them under it, again hides them. */
@Composable
private fun PassedLine(count: Int, expanded: Boolean, onToggle: () -> Unit) {
    val interaction = remember { MutableInteractionSource() }
    val focused by interaction.collectIsFocusedAsState()
    val toggle = stringResource(if (expanded) R.string.check_passed_hide else R.string.check_passed_show)
    Row(
        Modifier.fillMaxWidth().heightIn(min = 56.dp).focusMark(focused)
            .clickable(interactionSource = interaction, indication = LocalIndication.current, role = Role.Button, onClickLabel = toggle, onClick = onToggle)
            .padding(horizontal = 4.dp, vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(14.dp),
    ) {
        StatusMark(Tone.OK)
        Text(
            if (count == 1) stringResource(R.string.check_passed_one) else stringResource(R.string.check_passed, count),
            style = Type.Body, color = Palette.Text, modifier = Modifier.weight(1f),
        )
        Text(toggle, style = Type.Hint, color = Palette.Teal, modifier = Modifier.padding(end = 10.dp))
    }
}
