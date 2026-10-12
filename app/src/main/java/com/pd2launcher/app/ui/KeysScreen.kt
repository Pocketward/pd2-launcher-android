package com.pd2launcher.app.ui

import android.app.Activity
import android.content.ActivityNotFoundException
import android.content.Context
import android.content.Intent
import android.graphics.Typeface
import android.graphics.drawable.GradientDrawable
import android.net.Uri
import android.text.Editable
import android.text.InputFilter
import android.text.InputType
import android.text.TextWatcher
import android.text.method.HideReturnsTransformationMethod
import android.text.method.PasswordTransformationMethod
import android.view.Choreographer
import android.view.KeyEvent
import android.view.View
import android.view.Window
import android.view.WindowManager
import android.view.inputmethod.EditorInfo
import android.view.inputmethod.InputMethodManager
import android.widget.EditText
import androidx.annotation.StringRes
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.ime
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.relocation.BringIntoViewRequester
import androidx.compose.foundation.relocation.bringIntoViewRequester
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.runtime.snapshotFlow
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.graphics.toArgb
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalInputModeManager
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.compose.ui.window.DialogProperties
import androidx.compose.ui.window.SecureFlagPolicy
import com.pd2launcher.app.BuildConfig
import com.pd2launcher.app.DebugKeys
import com.pd2launcher.app.R
import com.pd2launcher.app.control.isProductKeyChar
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.collectLatest
import java.nio.CharBuffer
import java.util.WeakHashMap

/**
 * The CD keys page, the first of Install. Both keys are typed
 * into password fields Android never autofills, learns from or saves in the instance state;
 * `FLAG_SECURE` keeps the screen out of screenshots and the recents thumbnail, until the screen
 * after it is on display ([SecureWhileShown]). The keys show as
 * dots unless the player turns on Show keys. "Agree and install" is the player's acceptance of
 * Blizzard's license terms, said beside it and readable from the terms link; it empties the
 * fields and hands [onAuthorize] the keys as CharArrays, which it must clear once sent.
 *
 * The keyboard's action key finishes the page without the pills: on a phone they ride above the
 * keyboard, on a handheld they give way to the fields while it is up ([barsGiveWay]). Next goes
 * to the Lord of Destruction key, Done there is Agree and install once both keys look right
 * ([keyFieldAction]). Under that key, why Agree and install waits, then the license notice
 * ([keysLine]), beside what accepts it either way. The focused key's label and field are kept in
 * view above the keyboard and clear of the pills.
 * Android's full-screen landscape editor stays off: it shows the field without its label, so
 * the player could not tell which key they are typing.
 */
@OptIn(ExperimentalFoundationApi::class)
@Composable
fun KeysScreen(
    onAuthorize: (d2Key: CharArray, lodKey: CharArray) -> Unit,
    onBack: () -> Unit,
    /** Why the last Install Diablo II did not finish (the engine's words). */
    problem: String? = null,
    /**
     * Install Diablo II is paused and the keys entered for it went with the app: the page asks
     * for them to continue it where it stopped, not to start again.
     */
    resuming: Boolean = false,
    /**
     * Debug builds' layout preview (`--es preview keys`): the fields stay empty and cannot be
     * typed into, so no key can be on screen, and the screen may be captured.
     */
    layoutPreview: Boolean = false,
    /**
     * With [layoutPreview] (`--es preview keys-typing`): the fields take focus and the keyboard,
     * to show the page while typing; they are still never filled with the debug keys.
     */
    typingPreview: Boolean = false,
) {
    val activity = LocalContext.current as Activity
    // The fields hold the keys; Compose state holds only whether both look like keys.
    val fields = remember { arrayOfNulls<EditText>(2) }
    if (!layoutPreview) KeepSecure()
    DisposableEffect(Unit) {
        onDispose { fields.forEach { it?.takeKey()?.fill('\u0000') } }
    }
    val valid = remember { BooleanArray(2) }
    var ready by remember { mutableStateOf(false) }
    var reveal by remember { mutableStateOf(false) }
    var terms by remember { mutableStateOf(false) }
    val first = remember { FocusRequester() }
    val labels = listOf(stringResource(R.string.keys_d2_label), stringResource(R.string.keys_lod_label))
    val example = stringResource(R.string.keys_placeholder)
    val install: () -> Unit = install@{
        // Only with both keys in place: a second press (or Done) after the first took them would
        // send the emptied fields.
        if (!ready) return@install
        fields.forEach { field -> field?.let(::hideKeyboard) }
        val d2Key = fields[0]?.takeKey() ?: CharArray(0)
        val lodKey = fields[1]?.takeKey() ?: CharArray(0)
        onAuthorize(d2Key, lodKey)
    }
    // The fields' Done key, set once, installs with the caller's current onAuthorize.
    val latestInstall by rememberUpdatedState(install)
    // The key field with focus, -1 for none.
    var focused by remember { mutableIntStateOf(-1) }
    // As InitialFocus, except that a key field keeps the focus when a key typed into it (a
    // keyboard's Enter) leaves touch mode.
    val mode = LocalInputModeManager.current.inputMode
    LaunchedEffect(mode) { if (focused < 0) runCatching { first.requestFocus() } }
    // The focused key's label and field come back into view each time the keyboard settles
    // (opening, closing, the bars giving way) and when Next moves to the other key.
    val inView = remember { List(2) { BringIntoViewRequester() } }
    val density = LocalDensity.current
    val keyboard = WindowInsets.ime
    LaunchedEffect(focused) {
        val index = focused
        if (index < 0) return@LaunchedEffect
        snapshotFlow { keyboard.getBottom(density) }.collectLatest {
            delay(150)
            inView[index].bringIntoView()
        }
    }

    FlowShell(
        step = FlowStep.INSTALL,
        failed = problem != null,
        title = stringResource(if (resuming) R.string.keys_title_resume else R.string.keys_title),
        intro = stringResource(if (resuming) R.string.keys_intro_resume else R.string.keys_intro),
        // The system Back and the controller's B go back.
        onBack = onBack,
        backButton = true,
        lead = {
            GroupLabel(stringResource(R.string.keys_where_label), Modifier.padding(top = 0.dp))
            Text(stringResource(R.string.keys_where), style = Type.Body, color = Palette.Text)
            GroupLabel(stringResource(R.string.keys_example_label))
            Text(stringResource(R.string.keys_example), style = Type.Body.copy(fontFamily = androidx.compose.ui.text.font.FontFamily.Monospace), color = Palette.Muted)
            Text(stringResource(R.string.keys_example_old), style = Type.Hint, color = Palette.Faint, modifier = Modifier.padding(top = 2.dp))
            TextLink(stringResource(R.string.keys_terms_link), { terms = true }, Modifier.padding(top = 8.dp))
        },
        action = { CornerPill(stringResource(if (resuming) R.string.keys_resume else R.string.keys_install), install, Kind.PRIMARY, enabled = ready) },
    ) {
        // Why the last Install Diablo II did not finish, over the keys it may concern.
        problem?.let { Banner(it, Palette.Red, Modifier.padding(vertical = 8.dp)) }
        Column(Modifier.padding(horizontal = 4.dp, vertical = 8.dp), verticalArrangement = Arrangement.spacedBy(16.dp)) {
            labels.forEachIndexed { index, label ->
                // The label, the field and (on the last) the line under it come into view together.
                Column(Modifier.bringIntoViewRequester(inView[index]), verticalArrangement = Arrangement.spacedBy(6.dp)) {
                    Text(label, style = Type.Hint, color = Palette.Muted)
                    AndroidView(
                        factory = { context ->
                            lateinit var field: EditText
                            field = keyField(
                                context, label, example, last = index == labels.lastIndex,
                                onFocus = { has -> if (has) focused = index else if (focused == index) focused = -1 },
                            ) { looksLikeKey ->
                                // Only the current field speaks for its key (a replaced one is
                                // wiped below).
                                if (fields[index] === field) {
                                    valid[index] = looksLikeKey
                                    ready = valid.all { it }
                                }
                            }
                            field.setOnEditorActionListener { _, _, event ->
                                // An Enter key reports its press and its release: the release acts, else the
                                // next field would take the release and move focus on past itself.
                                if (event == null || event.action == KeyEvent.ACTION_UP) {
                                    when (keyFieldAction(index, labels.size, ready)) {
                                        KeyFieldAction.NEXT_FIELD -> fields[index + 1]?.requestFocus()
                                        KeyFieldAction.INSTALL -> latestInstall()
                                        KeyFieldAction.CLOSE_KEYBOARD -> hideKeyboard(field)
                                    }
                                }
                                true
                            }
                            fields[index] = field
                            if (layoutPreview) {
                                field.isEnabled = typingPreview
                            } else if (BuildConfig.DEBUG) {
                                // Debug builds pre-fill the keys a developer pushed
                                // ([DebugKeys]); no APK carries any.
                                DebugKeys.read(context.filesDir)?.let { keys ->
                                    field.setText(CharBuffer.wrap(keys[index]))
                                    keys.forEach { it.fill('\u0000') }
                                }
                            }
                            field
                        },
                        update = { field ->
                            val method = if (reveal) HideReturnsTransformationMethod.getInstance() else PasswordTransformationMethod.getInstance()
                            if (field.transformationMethod !== method) field.transformationMethod = method
                        },
                        // A layout change (rotation) makes new fields: the old ones are wiped.
                        onRelease = { old -> old.takeKey().fill('\u0000') },
                        modifier = Modifier.fillMaxWidth(),
                    )
                    // Under the last key, beside what accepts it (the pill, or the keyboard's Done):
                    // why Agree and install waits, else the license notice it accepts.
                    if (index == labels.lastIndex) {
                        Text(
                            stringResource(keysLine(ready)), style = Type.Hint, color = if (ready) Palette.Muted else Palette.Amber,
                            modifier = Modifier.padding(top = 4.dp),
                        )
                    }
                }
            }
        }
        Spacer(Modifier.height(4.dp))
        ToggleRow(
            stringResource(R.string.keys_show), stringResource(R.string.keys_show_body), reveal, { reveal = it },
            focusRequester = first,
        )
    }

    if (terms) {
        AlertDialog(
            onDismissRequest = { terms = false },
            title = { Text(stringResource(R.string.terms_title)) },
            text = { Text(stringResource(R.string.terms_body)) },
            confirmButton = { TextButton({ terms = false }) { Text(stringResource(R.string.terms_close)) } },
            dismissButton = {
                TextButton({
                    try {
                        activity.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(activity.getString(R.string.terms_url))))
                    } catch (_: ActivityNotFoundException) {
                        // No browser: the summary above stands.
                    }
                }) { Text(stringResource(R.string.terms_open)) }
            },
            properties = DialogProperties(securePolicy = if (layoutPreview) SecureFlagPolicy.Inherit else SecureFlagPolicy.SecureOn),
        )
    }
}

/**
 * Whether `FLAG_SECURE` may come off a window that showed a key page: never while one shows, and
 * only by the check its last [hide] handed out, run once the screen after it is on display, so
 * a key page shown again meanwhile keeps it on.
 */
class SecureWhileShown {
    private var shown = 0
    private var generation = 0

    fun show() {
        shown++
        generation++
    }

    /** The key page left: the check to run once the next screen is on display. */
    fun hide(): () -> Boolean {
        shown = (shown - 1).coerceAtLeast(0)
        val at = ++generation
        return { shown == 0 && generation == at }
    }
}

/** Each window's key pages, without keeping a window alive. */
private val secureKeyPages = WeakHashMap<Window, SecureWhileShown>()

/**
 * `FLAG_SECURE` on the activity's window while this is composed, out of screenshots and the
 * recents thumbnail: the key page, and the Install screen's view of the installer windows, where
 * Blizzard's installer may show a key. It comes off only once the screen that follows is on
 * display ([SecureWhileShown]), so a screenshot just after Agree and install cannot catch the key
 * page's last frame.
 */
@Composable
fun KeepSecure() {
    val window = (LocalContext.current as Activity).window
    DisposableEffect(window) {
        val secure = secureKeyPages.getOrPut(window) { SecureWhileShown() }
        secure.show()
        window.setSecure(true)
        onDispose {
            val mayClear = secure.hide()
            window.decorView.afterNextFrame { if (mayClear()) window.setSecure(false) }
        }
    }
}

/**
 * Turns `FLAG_SECURE` on or off through the window's attributes, never Window.addFlags or
 * setFlags: some vendors' Android builds drop FLAG_SECURE in both (addFlags returns for it,
 * setFlags masks it out), which would let the key page be captured.
 */
private fun Window.setSecure(on: Boolean) {
    attributes = attributes.also { it.flags = secureFlags(it.flags, on) }
}

/** [flags] with `FLAG_SECURE` [on] or off, every other flag as it was. */
fun secureFlags(flags: Int, on: Boolean): Int =
    if (on) flags or WindowManager.LayoutParams.FLAG_SECURE else flags and WindowManager.LayoutParams.FLAG_SECURE.inv()

/**
 * Runs [block] once the next frame (the screen after the key page) has gone to the compositor,
 * and one frame more, so that frame is what shows when it runs.
 */
private fun View.afterNextFrame(block: () -> Unit) {
    viewTreeObserver.registerFrameCommitCallback { Choreographer.getInstance().postFrameCallback { block() } }
    invalidate()
}

/**
 * The line under the Lord of Destruction key: why Agree and install waits while a key does not
 * look right, then the license notice it accepts.
 */
@StringRes
fun keysLine(ready: Boolean): Int = if (ready) R.string.keys_terms_notice else R.string.keys_hint

/** What the keyboard's action key does in key field [index] of [count]. */
enum class KeyFieldAction { NEXT_FIELD, INSTALL, CLOSE_KEYBOARD }

/**
 * Next in every field but the last goes to the following one; Done in the last is Agree and
 * install when both keys look right ([ready], as the button), else it only closes the keyboard.
 */
fun keyFieldAction(index: Int, count: Int, ready: Boolean): KeyFieldAction = when {
    index < count - 1 -> KeyFieldAction.NEXT_FIELD
    ready -> KeyFieldAction.INSTALL
    else -> KeyFieldAction.CLOSE_KEYBOARD
}

private fun hideKeyboard(field: EditText) {
    field.context.getSystemService(InputMethodManager::class.java)?.hideSoftInputFromWindow(field.windowToken, 0)
}

/**
 * A password field for one CD key, typed in capitals and ASCII without suggestions or
 * corrections; [onChange] says whether it holds 1 to 64 key characters, [onFocus] whether it has focus.
 */
private fun keyField(
    context: Context,
    label: String,
    example: String,
    last: Boolean,
    onFocus: (Boolean) -> Unit,
    onChange: (Boolean) -> Unit,
): EditText = EditText(context).apply {
    inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_VARIATION_PASSWORD or InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS or
        InputType.TYPE_TEXT_FLAG_CAP_CHARACTERS
    importantForAutofill = View.IMPORTANT_FOR_AUTOFILL_NO
    imeOptions = EditorInfo.IME_FLAG_NO_PERSONALIZED_LEARNING or EditorInfo.IME_FLAG_NO_EXTRACT_UI or
        EditorInfo.IME_FLAG_NO_FULLSCREEN or EditorInfo.IME_FLAG_FORCE_ASCII or
        (if (last) EditorInfo.IME_ACTION_DONE else EditorInfo.IME_ACTION_NEXT)
    // Never in the saved instance state.
    isSaveEnabled = false
    isSingleLine = true
    // After isSingleLine, which replaces the transformation: the keys show as dots.
    transformationMethod = PasswordTransformationMethod.getInstance()
    // Capitals whatever the keyboard sends, as keys are printed.
    filters = arrayOf(InputFilter.LengthFilter(64), InputFilter.AllCaps())
    contentDescription = label
    typeface = Typeface.MONOSPACE
    hint = example
    setHintTextColor(Palette.Faint.toArgb())
    setTextColor(Palette.Text.toArgb())
    textSize = 15f
    letterSpacing = 0.05f
    // A rounded 52 dp slot; the outline turns teal with focus.
    val density = resources.displayMetrics.density
    val outline = GradientDrawable().apply {
        cornerRadius = 12 * density
        setColor(Palette.Surface.toArgb())
        setStroke((1 * density).toInt(), Palette.Rule.toArgb())
    }
    background = outline
    minHeight = (52 * density).toInt()
    val inset = (14 * density).toInt()
    setPadding(inset, 0, inset, 0)
    setOnFocusChangeListener { _, focused ->
        outline.setStroke(((if (focused) 2 else 1) * density).toInt(), (if (focused) Palette.Teal else Palette.Rule).toArgb())
        onFocus(focused)
    }
    addTextChangedListener(object : TextWatcher {
        override fun beforeTextChanged(s: CharSequence?, start: Int, count: Int, after: Int) {}
        override fun onTextChanged(s: CharSequence?, start: Int, before: Int, count: Int) {}
        override fun afterTextChanged(s: Editable) {
            onChange(s.isNotEmpty() && (0 until s.length).all { isProductKeyChar(s[it]) })
        }
    })
}

/** The field's characters, without a String; the field is overwritten and emptied. */
private fun EditText.takeKey(): CharArray {
    val text = text
    val key = CharArray(text.length)
    text.getChars(0, text.length, key, 0)
    text.replace(0, text.length, "\u0000".repeat(text.length))
    text.clear()
    return key
}
