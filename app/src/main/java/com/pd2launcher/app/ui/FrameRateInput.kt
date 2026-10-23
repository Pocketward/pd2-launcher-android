package com.pd2launcher.app.ui

import android.graphics.drawable.GradientDrawable
import android.text.InputFilter
import android.text.InputType
import android.view.KeyEvent
import android.view.MotionEvent
import android.view.View
import android.view.ViewTreeObserver
import android.view.Window
import android.view.WindowManager
import android.view.inputmethod.EditorInfo
import android.view.inputmethod.InputMethodManager
import android.widget.EditText
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.widthIn
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
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
import androidx.compose.ui.layout.layout
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalInputModeManager
import androidx.compose.ui.platform.LocalView
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import androidx.compose.ui.window.DialogWindowProvider
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import androidx.core.view.WindowInsetsControllerCompat
import com.pd2launcher.app.AppSettings
import com.pd2launcher.app.FrameRateVote
import com.pd2launcher.app.R
import com.pd2launcher.app.input.Controls
import com.pd2launcher.app.input.PointerSpeed
import java.util.Locale

/**
 * The Frame rate input, from Settings and from the in-game menu: the number in use in a field
 * ([current], the stored [AppSettings.frameRate]; Full: the screen's most), "fps" after it, all
 * of it selected. Opened by a tap, Android's number keyboard opens at once; with a controller,
 * the D-pad's up and down change it by [FrameRateVote.STEP] ([NumberInput]). OK takes a whole
 * number from [FrameRateVote.MIN] to the screen's most ([full], its full rate,
 * [com.pd2launcher.app.ScreenRate]; the display's highest mode is stored as Full, a held screen's
 * most as its number, [FrameRateVote.typed]) and gives [onSet] the value to store; any other
 * number turns the help line into what to type, and the dialog stays.
 */
@Composable
internal fun FrameRateInput(current: Int, full: Float, onSet: (Int) -> Unit, onClose: () -> Unit) {
    val context = LocalContext.current
    val highest = remember { FrameRateVote.highest(context) }
    val most = FrameRateVote.most(full)
    val start = FrameRateVote.shown(current, full)
    // The number the field got as the dialog opened (the full rate can change meanwhile).
    val opened = remember { start }
    NumberInput(
        title = stringResource(R.string.settings_frame_rate),
        start = start.toString(),
        suffix = stringResource(R.string.frame_rate_unit),
        decimal = false,
        maxLength = 3,
        help = stringResource(R.string.frame_rate_help, FrameRateVote.MIN, most),
        wrong = stringResource(R.string.settings_frame_rate_input_wrong, FrameRateVote.MIN, most),
        step = { text, steps -> FrameRateVote.stepped(text.trim().toIntOrNull() ?: start, steps, full).toString() },
        // OK on the number as it came keeps the stored pick (Full shown as a held screen's 60 stays Full).
        accept = { text -> text.trim() == opened.toString() || FrameRateVote.typed(text, full, highest)?.also(onSet) != null },
        onClose = onClose,
    )
}

/**
 * The Pointer speed input, from the in-game Controls screen: mouse mode's pointer speed in a field
 * ([current]), "×" after it, in the Frame rate input's style. The D-pad's up and down change it by
 * [PointerSpeed.STEP]. OK takes a number inside [PointerSpeed.RANGE], put on the step grid
 * ([PointerSpeed.typed]), and gives [onSet] the value to store.
 */
@Composable
internal fun PointerSpeedInput(current: Float, onSet: (Float) -> Unit, onClose: () -> Unit) {
    val range = PointerSpeed.RANGE
    NumberInput(
        title = stringResource(R.string.controls_pointer_speed_title),
        start = speedText(current),
        suffix = stringResource(R.string.controls_factor_unit),
        decimal = true,
        maxLength = 4,
        help = stringResource(
            R.string.controls_pointer_speed_help,
            speedText(range.start), speedText(range.endInclusive), speedText(PointerSpeed.STEP), speedText(PointerSpeed.DEFAULT),
        ),
        wrong = stringResource(R.string.controls_pointer_speed_wrong, speedText(range.start), speedText(range.endInclusive)),
        // From what the field holds, a number outside the range too.
        step = { text, steps ->
            val from = text.trim().replace(',', '.').toFloatOrNull()?.coerceIn(range) ?: current
            speedText(Controls.stepped(from, PointerSpeed.STEP, steps, range))
        },
        accept = { text -> PointerSpeed.typed(text)?.also(onSet) != null },
        onClose = onClose,
    )
}

/** A pointer speed as the input shows it: two decimals with a point, "0.50". */
internal fun speedText(value: Float): String = String.format(Locale.ROOT, "%.2f", value)

/**
 * A dialog for one number: [title], a field with [start] in it and [suffix] after it, one help
 * line ([help], or [wrong] after a number OK refused), and Cancel and OK. The field has the focus
 * with the whole number selected, so a number typed replaces it. Opened by a touch, Android's
 * number keyboard opens at once; opened with a controller or keys (out of touch mode), the
 * keyboard stays closed: the D-pad's up and down change the number ([step]: the text after that
 * many steps), its right goes to OK. A tap on the field selects the whole number again and opens
 * the keyboard, whose Done is OK. OK gives the text to [accept], which keeps it and says so, and
 * then the dialog closes. With the keyboard up the dialog keeps above it: where the room is short,
 * Cancel and OK move up beside the title, so they always show. A controller's A selects and B is
 * Back, as on the native screens.
 */
@Composable
private fun NumberInput(
    title: String,
    start: String,
    suffix: String,
    decimal: Boolean,
    maxLength: Int,
    help: String,
    wrong: String,
    step: (String, Int) -> String,
    accept: (String) -> Boolean,
    onClose: () -> Unit,
) {
    var refused by remember { mutableStateOf(false) }
    var field by remember { mutableStateOf<EditText?>(null) }
    var focused by remember { mutableStateOf(false) }
    val ok = remember { FocusRequester() }
    // How the player opened it, as the dialog came: a tap (touch mode) or a controller's or a key's press.
    val mode = LocalInputModeManager.current.inputMode
    val byTouch = remember { mode == InputMode.Touch }
    val tryAccept = { if (accept(field?.text?.toString().orEmpty())) onClose() else refused = true }
    // The field is made once: its keys and its keyboard's Done call the newest lambdas.
    val latestAccept by rememberUpdatedState(tryAccept)
    val latestStep by rememberUpdatedState(step)
    Dialog(onClose, DialogProperties(usePlatformDefaultWidth = false)) {
        val window = (LocalView.current.parent as? DialogWindowProvider)?.window
        DisposableEffect(window) {
            // The keyboard as the dialog opens only after a tap; after a controller's press only on a tap.
            val keyboard = if (byTouch) WindowManager.LayoutParams.SOFT_INPUT_STATE_VISIBLE else WindowManager.LayoutParams.SOFT_INPUT_STATE_ALWAYS_HIDDEN
            window?.setSoftInputMode(WindowManager.LayoutParams.SOFT_INPUT_ADJUST_RESIZE or keyboard)
            // Over the game, as the game: no system bars (they would also shift the screen under the dialog).
            window?.let {
                WindowCompat.getInsetsController(it, it.decorView).run {
                    systemBarsBehavior = WindowInsetsControllerCompat.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
                    hide(WindowInsetsCompat.Type.systemBars())
                }
            }
            val base = window?.let(::withNavigationKeys)
            onDispose { if (window != null && base != null) window.callback = base }
        }
        // The height the dialog needs with its buttons under the field, at its width, whatever room it has now.
        var tall by remember { mutableIntStateOf(0) }
        BoxWithConstraints(Modifier.padding(16.dp), contentAlignment = Alignment.Center) {
            val room = with(LocalDensity.current) { maxHeight.roundToPx() }
            val compact = tall > room
            Panel(
                Modifier.widthIn(max = 420.dp).fillMaxWidth().testTag(NUMBER_INPUT_TAG).layout { measurable, constraints ->
                    if (!compact) tall = measurable.maxIntrinsicHeight(constraints.maxWidth)
                    val placeable = measurable.measure(constraints)
                    layout(placeable.width, placeable.height) { placeable.place(0, 0) }
                },
                padding = if (compact) 12.dp else 16.dp,
            ) {
                val buttons = @Composable {
                    LauncherButton(stringResource(R.string.settings_frame_rate_cancel), onClose, kind = Kind.QUIET, height = 48.dp, padding = 18.dp)
                    LauncherButton(stringResource(R.string.settings_frame_rate_ok), { latestAccept() }, kind = Kind.PRIMARY, focusRequester = ok, height = 48.dp, padding = 22.dp)
                }
                if (compact) {
                    Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                        Text(title, style = Type.Body.copy(fontWeight = FontWeight.SemiBold), color = Palette.Text, modifier = Modifier.weight(1f))
                        buttons()
                    }
                } else {
                    Text(title, style = Type.Title, color = Palette.Text)
                }
                Spacer(Modifier.height(if (compact) 8.dp else 12.dp))
                // The same call in both shapes, so the field (and the keyboard on it) stays.
                Row(
                    Modifier.fillMaxWidth().height(if (compact) 48.dp else 56.dp).clip(RowShape).background(Palette.Background)
                        .border(if (focused) 2.dp else 1.dp, if (focused) Palette.Teal else Palette.Rule, RowShape)
                        .clickable(remember { MutableInteractionSource() }, null) { field?.let(::openKeyboard) }
                        .padding(start = 16.dp, end = 10.dp),
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(6.dp),
                ) {
                    AndroidView(
                        factory = { context ->
                            numberField(
                                context, start, maxLength, "", decimal = decimal, framed = false, opens = byTouch, focuses = true,
                                onStep = { text, steps -> latestStep(text, steps) },
                                onRight = { runCatching { ok.requestFocus() } },
                                onDone = { latestAccept() },
                            ).also { view ->
                                view.setOnFocusChangeListener { _, has -> focused = has }
                                field = view
                            }
                        },
                    )
                    Text(suffix, style = Type.Body.copy(fontSize = FIELD_TEXT), color = Palette.Muted)
                    Spacer(Modifier.weight(1f))
                    Column(horizontalAlignment = Alignment.CenterHorizontally) {
                        Icon(painterResource(R.drawable.ic_step_up), null, Modifier.size(20.dp), tint = Palette.Muted)
                        Icon(painterResource(R.drawable.ic_step_down), null, Modifier.size(20.dp), tint = Palette.Muted)
                    }
                }
                Text(
                    if (refused) wrong else help, style = Type.Hint, color = if (refused) Palette.Amber else Palette.Muted,
                    modifier = Modifier.padding(top = if (compact) 6.dp else 8.dp),
                )
                if (!compact) {
                    Row(Modifier.fillMaxWidth().padding(top = 16.dp), horizontalArrangement = Arrangement.spacedBy(8.dp, Alignment.End)) { buttons() }
                }
            }
        }
    }
}

/** The number fields' text size, Android's sp. */
private val FIELD_TEXT = 18.sp

/**
 * A tap on [field] or around it: it takes the focus with its whole text selected, so a number
 * typed replaces it, and Android's keyboard opens (asked as the player's own request, as the
 * field's own tap does).
 */
private fun openKeyboard(field: EditText) {
    field.requestFocus()
    field.selectAll()
    field.context.getSystemService(InputMethodManager::class.java)?.showSoftInput(field, 0)
}

/**
 * A controller's A as the d-pad's centre and B as Back in [window] (a dialog's), as
 * MainActivity does for its own window ([navigationKey]); returns the callback it wraps.
 */
internal fun withNavigationKeys(window: Window): Window.Callback {
    val base = window.callback
    window.callback = object : Window.Callback by base {
        override fun dispatchKeyEvent(event: KeyEvent): Boolean = base.dispatchKeyEvent(navigationKey(event))
    }
    return base
}

/**
 * A controller's A as the d-pad's centre (select) and B as Back: Android does not treat the
 * gamepad's A as a confirm key. Every other key as it is.
 */
internal fun navigationKey(event: KeyEvent): KeyEvent {
    val mapped = when (event.keyCode) {
        KeyEvent.KEYCODE_BUTTON_A -> KeyEvent.KEYCODE_DPAD_CENTER
        KeyEvent.KEYCODE_BUTTON_B -> KeyEvent.KEYCODE_BACK
        else -> return event
    }
    return KeyEvent(
        event.downTime, event.eventTime, event.action, mapped, event.repeatCount,
        event.metaState, event.deviceId, event.scanCode, event.flags, event.source,
    )
}

/**
 * A field for Android's number keyboard: digits only (and a decimal point with [decimal]),
 * [maxLength] characters at most, [text] in it and selected, so a number typed replaces it,
 * [hint] while empty. A tap on it selects its whole text again (no cursor at the tap) and opens
 * the keyboard. [framed]: its own rounded field; else bare, for a field drawn around it.
 * The keyboard's Done (or Enter) is OK ([onDone]); with [onNext] the keyboard offers Next
 * instead, and it (or Enter) goes to the next field. [onStep]: the D-pad's up and down give the
 * text one step up or down; [onRight]: the D-pad's right leaves the field. Where it [focuses] (the
 * first field) it takes the focus as its window gets it; the keyboard opens with it only where it
 * [opens] (never full screen).
 */
internal fun numberField(
    context: android.content.Context,
    text: String,
    maxLength: Int,
    hint: String,
    decimal: Boolean = false,
    framed: Boolean = true,
    opens: Boolean = true,
    focuses: Boolean = opens,
    onNext: (() -> Unit)? = null,
    onStep: ((String, Int) -> String)? = null,
    onRight: (() -> Unit)? = null,
    onDone: () -> Unit,
): EditText = EditText(context).apply {
    inputType = InputType.TYPE_CLASS_NUMBER or (if (decimal) InputType.TYPE_NUMBER_FLAG_DECIMAL else 0)
    imeOptions = (if (onNext != null) EditorInfo.IME_ACTION_NEXT else EditorInfo.IME_ACTION_DONE) or
        EditorInfo.IME_FLAG_NO_EXTRACT_UI or EditorInfo.IME_FLAG_NO_FULLSCREEN
    isSingleLine = true
    filters = arrayOf(InputFilter.LengthFilter(maxLength))
    this.hint = hint
    setHintTextColor(Palette.Faint.toArgb())
    setTextColor(Palette.Text.toArgb())
    highlightColor = Palette.Teal.copy(alpha = 0.35f).toArgb()
    textSize = FIELD_TEXT.value
    val density = resources.displayMetrics.density
    if (framed) {
        background = GradientDrawable().apply {
            cornerRadius = 12 * density
            setColor(Palette.Background.toArgb())
            setStroke((2 * density).toInt(), Palette.Teal.toArgb())
        }
        val inset = (14 * density).toInt()
        setPadding(inset, 0, inset, 0)
    } else {
        background = null
        setPadding(0, 0, 0, 0)
        minWidth = 0
        minimumWidth = 0
    }
    setText(text)
    selectAll()
    // The focus coming back (from OK, Next) selects it too.
    setSelectAllOnFocus(true)
    // A tap selects the whole number (a few digits: no cursor to place) and opens the keyboard.
    setOnTouchListener { view, event ->
        val inside = event.x >= 0f && event.y >= 0f && event.x < view.width && event.y < view.height
        if (event.actionMasked == MotionEvent.ACTION_UP && inside) {
            openKeyboard(this)
            view.performClick()
        }
        true
    }
    // Done or Next on the keyboard, or Enter on a hardware keyboard (asked on its press and its release).
    setOnEditorActionListener { _, action, event ->
        val enter = action == EditorInfo.IME_NULL && event?.keyCode == KeyEvent.KEYCODE_ENTER
        val key = action == EditorInfo.IME_ACTION_DONE || action == EditorInfo.IME_ACTION_NEXT
        if (key || (enter && event?.action == KeyEvent.ACTION_UP)) (onNext ?: onDone)()
        key || enter
    }
    if (onStep != null || onRight != null) setOnKeyListener { _, code, event ->
        val steps = when (code) {
            KeyEvent.KEYCODE_DPAD_UP -> 1
            KeyEvent.KEYCODE_DPAD_DOWN -> -1
            else -> 0
        }
        val down = event.action == KeyEvent.ACTION_DOWN
        when {
            steps != 0 && onStep != null -> {
                if (down) {
                    setText(onStep(this.text.toString(), steps))
                    // Selected, so a number typed after A opened the keyboard replaces it.
                    selectAll()
                }
                true
            }
            code == KeyEvent.KEYCODE_DPAD_RIGHT && onRight != null -> {
                if (down) onRight()
                true
            }
            else -> false
        }
    }
    // The focus (and the keyboard where it opens) once the dialog's window has the focus; before
    // that, Android shows no keyboard.
    val focus = {
        requestFocus()
        if (opens) context.getSystemService(InputMethodManager::class.java)?.showSoftInput(this, InputMethodManager.SHOW_IMPLICIT)
    }
    if (focuses) addOnAttachStateChangeListener(object : View.OnAttachStateChangeListener {
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
