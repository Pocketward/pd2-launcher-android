package com.pd2launcher.app.ui

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.LocalIndication
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.focusGroup
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.interaction.collectIsFocusedAsState
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxScope
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawing
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusProperties
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.unit.dp
import com.pd2launcher.app.R
import com.pd2launcher.app.input.Controls
import com.pd2launcher.app.input.PadAction
import com.pd2launcher.app.input.PadButton

/**
 * The binding picker for one controller [button] in one mode ([mouse]), from Settings →
 * Controller and from the in-game Controls screen: what the button may do (the game pad in pad
 * mode, nothing, the mouse, then Diablo II's keys), the current choice selected and focused. A
 * sheet from the right ([side]: handhelds, and the game, which is always landscape) or from the
 * bottom (phones in Settings), the rest dimmed. A pick saves at once ([save]) and closes it;
 * Done, Back or a tap on the dim closes it unchanged ([onClose]).
 */
@Composable
internal fun BoxScope.BindingPicker(
    controls: Controls,
    button: PadButton,
    mouse: Boolean,
    side: Boolean,
    save: (Controls) -> Unit,
    onClose: () -> Unit,
) {
    BackHandler(onBack = onClose)
    // A tap on the dim closes the sheet; the dim takes no focus.
    val close by rememberUpdatedState(onClose)
    Box(Modifier.matchParentSize().background(Color.Black.copy(alpha = 0.5f)).pointerInput(Unit) { detectTapGestures { close() } })
    val sheet = if (side) {
        Modifier.align(Alignment.CenterEnd).width(SIDE_SHEET).fillMaxHeight()
    } else {
        Modifier.align(Alignment.BottomCenter).fillMaxWidth().fillMaxHeight(0.72f)
    }
    val current = controls.action(button, mouse)
    val first = remember { FocusRequester() }
    LaunchedEffect(button, mouse) { runCatching { first.requestFocus() } }
    // The d-pad stays in the sheet: nothing behind the dim takes the focus.
    Column(
        sheet.windowInsetsPadding(WindowInsets.safeDrawing).padding(8.dp).clip(RowShape).background(Palette.Surface).border(1.dp, Palette.Rule, RowShape)
            .takesTouches().focusProperties { onExit = { cancelFocusChange() } }.focusGroup(),
    ) {
        Row(Modifier.fillMaxWidth().heightIn(min = 56.dp).padding(start = 16.dp, end = 4.dp), verticalAlignment = Alignment.CenterVertically) {
            KeyCap(buttonCap(button))
            Column(Modifier.weight(1f).padding(start = 12.dp)) {
                Text(button.label, style = Type.Body, color = Palette.Text)
                Text(stringResource(if (mouse) R.string.controls_mouse_mode else R.string.controls_pad_mode), style = Type.Hint, color = Palette.Muted)
            }
            LauncherButton(stringResource(R.string.controls_picker_done), onClose, kind = Kind.QUIET, height = 48.dp, padding = 14.dp)
        }
        Rule()
        Column(Modifier.weight(1f).verticalScroll(rememberScrollState()).padding(6.dp)) {
            val general = generalChoices(mouse)
            val pick: (PadAction) -> Unit = {
                save(controls.bind(button, mouse, it))
                onClose()
            }
            val focusOn = current.takeIf { it in general || it in keyChoices } ?: general.first()
            ChoiceGroup(stringResource(R.string.controls_picker_general), general, current, focusOn, first, pick)
            ChoiceGroup(stringResource(R.string.controls_picker_keys), keyChoices, current, focusOn, first, pick)
        }
    }
}

@Composable
private fun ChoiceGroup(title: String, choices: List<PadAction>, current: PadAction, focusOn: PadAction, first: FocusRequester, onPick: (PadAction) -> Unit) {
    GroupLabel(title, Modifier.padding(start = 10.dp))
    choices.chunked(2).forEach { row ->
        Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(4.dp)) {
            row.forEach { action -> ChoiceCell(action, action == current, { onPick(action) }, Modifier.weight(1f), if (action == focusOn) first else null) }
            if (row.size == 1) Spacer(Modifier.weight(1f))
        }
    }
}

@Composable
private fun ChoiceCell(action: PadAction, selected: Boolean, onClick: () -> Unit, modifier: Modifier, focus: FocusRequester?) {
    val interaction = remember { MutableInteractionSource() }
    val focused by interaction.collectIsFocusedAsState()
    Row(
        modifier.focusOn(focus)
            .heightIn(min = 48.dp).focusMark(focused, selected)
            .selectable(selected, interaction, LocalIndication.current, role = Role.RadioButton, onClick = onClick)
            .padding(horizontal = 10.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        // Every row keeps the same cap column, so the names line up whether or not it has a cap.
        Box(Modifier.width(CHOICE_CAP), contentAlignment = Alignment.Center) {
            choiceCap(action)?.let { KeyCap(it, Modifier.fillMaxWidth(), selected = selected) }
        }
        Text(actionName(action), style = Type.Hint, color = if (selected) Palette.Teal else Palette.Text)
    }
}

/** The width of a choice row's key cap, enough for "Space" or "Enter". */
private val CHOICE_CAP = 56.dp

/** The side sheet's width: two choices a row, each with room for "(previous" beside its cap, also at a large font. */
private val SIDE_SHEET = 400.dp
