package com.pd2launcher.app.ui

import android.text.InputFilter
import android.text.InputType
import android.view.Gravity
import android.view.KeyEvent
import android.view.View
import android.view.ViewTreeObserver
import android.view.WindowManager
import android.view.inputmethod.EditorInfo
import android.view.inputmethod.InputMethodManager
import android.widget.EditText
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.shape.RoundedCornerShape
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
import androidx.compose.ui.draw.clip
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.graphics.toArgb
import androidx.compose.ui.input.InputMode
import androidx.compose.ui.input.key.Key
import androidx.compose.ui.input.key.KeyEventType
import androidx.compose.ui.input.key.key
import androidx.compose.ui.input.key.onPreviewKeyEvent
import androidx.compose.ui.input.key.type
import androidx.compose.ui.platform.LocalInputModeManager
import androidx.compose.ui.platform.LocalView
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import androidx.compose.ui.window.DialogWindowProvider
import com.pd2launcher.app.LauncherSettings
import com.pd2launcher.app.R

/**
 * A Custom start options input: the title, the line that says what the options do ([intro]: the
 * game's or the official PD2 launcher's), one text field with the options now ([current]), one
 * help line, and Clear, Cancel and OK. Clear empties the field; OK takes text that the engine
 * takes ([LauncherSettings.typedCustomArguments]: at most 200 characters, no double quote) and
 * gives [onSet] the trimmed text (empty: none); other text turns the help line into what to
 * change, and the input stays ([SettingTextInput]).
 */
@Composable
internal fun CustomArgumentsInput(current: String, intro: String, onSet: (String) -> Unit, onClose: () -> Unit) {
    val wrong = stringResource(R.string.settings_custom_arguments_wrong, LauncherSettings.CUSTOM_ARGUMENTS_MAX)
    SettingTextInput(
        title = stringResource(R.string.settings_custom_arguments),
        intro = intro,
        prefix = null,
        current = current,
        maxLength = LauncherSettings.CUSTOM_ARGUMENTS_MAX,
        empty = stringResource(R.string.settings_custom_arguments_none),
        help = stringResource(R.string.settings_custom_arguments_help, LauncherSettings.CUSTOM_ARGUMENTS_MAX),
        reset = stringResource(R.string.settings_custom_arguments_clear) to "",
        accept = { typed ->
            val arguments = LauncherSettings.typedCustomArguments(typed)
            if (arguments == null) {
                wrong
            } else {
                onSet(arguments)
                null
            }
        },
        onClose = onClose,
    )
}

/**
 * A one-line text input over Settings: the [title], the [intro] line, one text field with
 * [current] in it (after the fixed [prefix] text, when there is one; [empty] as its hint), one
 * help line, and the [reset] action (its label and the text it puts in the field), Cancel and
 * OK. OK gives [accept] the field's text: null closes the input, a text keeps it open with that
 * text as the help line, in amber, and an amber field. Opened by a tap, Android's keyboard opens
 * at once; opened with a controller, the keyboard opens on A on the field, the D-pad's down goes
 * from the field to OK and its up back to the field. A controller's A selects and B is Back, which
 * is Cancel. At the top of the screen, so the keyboard does not cover it.
 */
@Composable
internal fun SettingTextInput(
    title: String,
    intro: String,
    prefix: String?,
    current: String,
    maxLength: Int,
    empty: String,
    help: String,
    reset: Pair<String, String>,
    accept: (String) -> String?,
    onClose: () -> Unit,
) {
    var refused by remember { mutableStateOf<String?>(null) }
    var field by remember { mutableStateOf<EditText?>(null) }
    val ok = remember { FocusRequester() }
    // How the player opened it, as the input came: a tap (touch mode) or a controller's or a key's press.
    val mode = LocalInputModeManager.current.inputMode
    val opensKeyboard = remember { mode == InputMode.Touch }
    val onAccept = {
        refused = accept(field?.text?.toString().orEmpty())
        if (refused == null) onClose()
    }
    // The field is made once: its keyboard's Done calls the newest accept, with the newest onSet.
    val latestAccept by rememberUpdatedState(onAccept)
    Dialog(onClose, DialogProperties(usePlatformDefaultWidth = false)) {
        val window = (LocalView.current.parent as? DialogWindowProvider)?.window
        DisposableEffect(window) {
            window?.setGravity(Gravity.TOP)
            // The keyboard as the input opens only after a tap; after a controller's press only on A on the field.
            val keyboard = if (opensKeyboard) WindowManager.LayoutParams.SOFT_INPUT_STATE_VISIBLE else WindowManager.LayoutParams.SOFT_INPUT_STATE_ALWAYS_HIDDEN
            window?.setSoftInputMode(WindowManager.LayoutParams.SOFT_INPUT_ADJUST_RESIZE or keyboard)
            // As the other inputs: it shows and goes at once, and the screen behind it is not dimmed (its panel is opaque).
            window?.setWindowAnimations(0)
            window?.clearFlags(WindowManager.LayoutParams.FLAG_DIM_BEHIND)
            val base = window?.let(::withNavigationKeys)
            onDispose { if (window != null && base != null) window.callback = base }
        }
        Panel(Modifier.padding(top = 12.dp, start = 16.dp, end = 16.dp).widthIn(max = 480.dp).fillMaxWidth()) {
            Text(title, style = Type.Title, color = Palette.Text)
            Text(intro, style = Type.Hint, color = Palette.Muted, modifier = Modifier.padding(top = 6.dp))
            val shape = RoundedCornerShape(12.dp)
            Row(
                Modifier.fillMaxWidth().padding(top = 12.dp).height(52.dp)
                    .border(2.dp, if (refused != null) Palette.Amber else Palette.Teal, shape).clip(shape).background(Palette.Background),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                // The fixed text before the field ("Diablo II/"): not typed, not taken.
                if (prefix != null) Text(prefix, Modifier.padding(start = 14.dp), style = Type.Body.copy(fontSize = 18.sp), color = Palette.Muted, maxLines = 1)
                AndroidView(
                    factory = { context ->
                        textField(
                            context, current, maxLength, empty, opensKeyboard, start = prefix == null,
                            onDown = { runCatching { ok.requestFocus() } }, onDone = { latestAccept() },
                        ).also { field = it }
                    },
                    modifier = Modifier.weight(1f).fillMaxHeight(),
                )
            }
            Text(
                refused ?: help,
                style = Type.Hint, color = if (refused != null) Palette.Amber else Palette.Muted, modifier = Modifier.padding(top = 6.dp),
            )
            Row(
                Modifier.fillMaxWidth().padding(top = 12.dp).onPreviewKeyEvent { event ->
                    // The D-pad's up goes back to the field, which Compose's own search does not reach.
                    val up = event.key == Key.DirectionUp
                    if (up && event.type == KeyEventType.KeyDown) field?.requestFocus()
                    up
                },
                horizontalArrangement = Arrangement.spacedBy(8.dp, Alignment.End),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                // Default or Clear: the field's text, which OK then saves.
                TextLink(reset.first, {
                    field?.setText(reset.second)
                    field?.setSelection(reset.second.length)
                    refused = null
                })
                Spacer(Modifier.weight(1f))
                LauncherButton(stringResource(R.string.settings_frame_rate_cancel), onClose, kind = Kind.QUIET, height = 48.dp, padding = 14.dp)
                LauncherButton(stringResource(R.string.settings_frame_rate_ok), { latestAccept() }, kind = Kind.PRIMARY, focusRequester = ok, height = 48.dp, padding = 18.dp)
            }
        }
    }
}

/**
 * A field for Android's text keyboard, one line, with no suggestions and no capital letters,
 * [maxLength] characters at most, [text] in it with the cursor at its end, [empty] as its hint,
 * and room before the text where it [start]s the field. It takes the focus as its window gets it, and the keyboard opens with it where it [opens].
 * The keyboard's Done (or Enter) is OK ([onDone]); the D-pad's down leaves the field ([onDown]).
 */
private fun textField(
    context: android.content.Context,
    text: String,
    maxLength: Int,
    empty: String,
    opens: Boolean,
    start: Boolean,
    onDown: () -> Unit,
    onDone: () -> Unit,
): EditText = EditText(context).apply {
    inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS or InputType.TYPE_TEXT_VARIATION_VISIBLE_PASSWORD
    imeOptions = EditorInfo.IME_ACTION_DONE or EditorInfo.IME_FLAG_NO_EXTRACT_UI or EditorInfo.IME_FLAG_NO_FULLSCREEN
    isSingleLine = true
    filters = arrayOf(InputFilter.LengthFilter(maxLength))
    hint = empty
    setHintTextColor(Palette.Faint.toArgb())
    setTextColor(Palette.Text.toArgb())
    highlightColor = Palette.Teal.copy(alpha = 0.35f).toArgb()
    textSize = 18f
    // The border is the input's own, around the fixed text before the field too.
    background = null
    val inset = (14 * resources.displayMetrics.density).toInt()
    setPadding(if (start) inset else 0, 0, inset, 0)
    setText(text)
    setSelection(this.text.length)
    // Done on the keyboard, or Enter on a hardware keyboard (asked on its press and its release).
    setOnEditorActionListener { _, action, event ->
        val enter = action == EditorInfo.IME_NULL && event?.keyCode == KeyEvent.KEYCODE_ENTER
        val done = action == EditorInfo.IME_ACTION_DONE
        if (done || (enter && event.action == KeyEvent.ACTION_UP)) onDone()
        done || enter
    }
    setOnKeyListener { _, code, event ->
        if (code != KeyEvent.KEYCODE_DPAD_DOWN) return@setOnKeyListener false
        if (event.action == KeyEvent.ACTION_DOWN) onDown()
        true
    }
    // The focus (and the keyboard where it opens) once the input's window has the focus; before
    // that, Android shows no keyboard.
    val focus = {
        requestFocus()
        if (opens) context.getSystemService(InputMethodManager::class.java)?.showSoftInput(this, InputMethodManager.SHOW_IMPLICIT)
    }
    addOnAttachStateChangeListener(object : View.OnAttachStateChangeListener {
        override fun onViewAttachedToWindow(view: View) {
            if (view.hasWindowFocus()) {
                focus()
                return
            }
            view.viewTreeObserver.addOnWindowFocusChangeListener(object : ViewTreeObserver.OnWindowFocusChangeListener {
                override fun onWindowFocusChanged(hasFocus: Boolean) {
                    if (!hasFocus) return
                    view.viewTreeObserver.removeOnWindowFocusChangeListener(this)
                    focus()
                }
            })
        }

        override fun onViewDetachedFromWindow(view: View) {}
    })
}
