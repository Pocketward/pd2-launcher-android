package com.pd2launcher.app

import android.content.Context
import android.content.SharedPreferences
import android.content.pm.ActivityInfo
import android.content.res.Configuration
import android.os.Build
import android.os.Bundle
import android.os.SystemClock
import android.util.Log
import android.view.ContextThemeWrapper
import android.view.InputDevice
import android.view.MotionEvent
import android.view.View
import android.view.WindowManager
import androidx.activity.ComponentActivity
import androidx.activity.SystemBarStyle
import androidx.activity.enableEdgeToEdge
import androidx.annotation.StringRes
import androidx.compose.foundation.LocalIndication
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.safeDrawing
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.ComposeView
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.unit.sp
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import androidx.core.view.WindowInsetsControllerCompat
import androidx.lifecycle.lifecycleScope
import com.pd2launcher.app.input.Controls
import com.pd2launcher.app.input.ControlsLayout
import com.pd2launcher.app.input.ControlsStore
import com.pd2launcher.app.input.CustomButtons
import com.pd2launcher.app.input.Keys
import com.pd2launcher.app.input.LayoutStore
import com.pd2launcher.app.input.PadAction
import com.pd2launcher.app.input.PadButton
import com.pd2launcher.app.input.TouchMode
import com.pd2launcher.app.input.VirtualPad
import com.pd2launcher.app.input.isController
import com.pd2launcher.app.ui.ControlsEditor
import com.pd2launcher.app.ui.ControlsScreen
import com.pd2launcher.app.ui.FpsCounter
import com.pd2launcher.app.ui.FrameRateInput
import com.pd2launcher.app.ui.GameLayer
import com.pd2launcher.app.ui.GameMenu
import com.pd2launcher.app.ui.LauncherTheme
import com.pd2launcher.app.ui.MenuActions
import com.pd2launcher.app.ui.MenuState
import com.pd2launcher.app.ui.MenuStop
import com.pd2launcher.app.ui.MenuTab
import com.pd2launcher.app.ui.MenuGlyph
import com.pd2launcher.app.ui.OnScreenControls
import com.pd2launcher.app.ui.PressTint
import com.pd2launcher.app.ui.TapToClose
import com.pd2launcher.app.ui.frameRateLabel
import java.util.concurrent.atomic.AtomicBoolean
import kotlin.math.roundToInt
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch

/**
 * Debug builds only: the in-game overlay (ui/GameOverlay.kt, ui/ControlsScreen.kt and the
 * surfaces they open) with fake state over a neutral dark gradient, never a game picture, for
 * screen captures in design reviews. It calls the real overlay composables and layers them as the
 * Game view does (GameView's `if (live)` block); nothing here changes them or the app's stored
 * settings.
 *
 * ```
 * adb shell am start -S -n com.pd2launcher.app.dev/com.pd2launcher.app.OverlayPreviewActivity \
 *     --es screen quick [--ef fontScale 1.15] [--ez onScreen true] [--es run <id>]
 * ```
 *
 * - `screen`: one of [SCREENS] (an unknown name logs the list and closes).
 * - `fontScale`: this activity's font scale only (its own configuration, so the dialogs' windows
 *   and their Android fields follow too); the system setting does not change. Absent: the system's.
 * - `onScreen`: the on-screen controls on or off; absent: Auto, as the Game view decides it (off
 *   while a controller is connected, so off on a handheld with built-in controls).
 * - `run`: a tag for this run's log lines.
 *
 * After the screen's steps (presses that open its surface, [PreviewStep]) and a short settle,
 * [OverlayProbe] logs each text that is cut off and the menu's and the Controls screen's sizes
 * (tag `pd2-overlay-eval`), then a `kind=READY` line: capture the screen after that line.
 *
 * In a process of its own (debug manifest), with its own preferences ([PreviewContext]): the
 * fake controls and on-screen layout never reach the app's own.
 */
class OverlayPreviewActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val name = intent.getStringExtra("screen") ?: "quick"
        val screen = SCREENS[name]
        if (screen == null) {
            Log.e(OverlayProbe.TAG, "unknown screen '$name'; screens: ${SCREENS.keys.joinToString(" ")}")
            finish()
            return
        }
        val fontScale = if (intent.hasExtra("fontScale")) intent.getFloatExtra("fontScale", 1f) else null
        val run = intent.getStringExtra("run") ?: SystemClock.elapsedRealtime().toString()
        val controller = controllerConnected()
        val onScreen = if (intent.hasExtra("onScreen")) intent.getBooleanExtra("onScreen", true) else !controller

        // As the Game view: edge to edge, no system bars, landscape, the screen kept on, and
        // Android's keyboard over the game rather than pushing it up.
        enableEdgeToEdge(
            statusBarStyle = SystemBarStyle.dark(android.graphics.Color.TRANSPARENT),
            navigationBarStyle = SystemBarStyle.dark(android.graphics.Color.TRANSPARENT),
        )
        requestedOrientation = ActivityInfo.SCREEN_ORIENTATION_SENSOR_LANDSCAPE
        window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        window.setSoftInputMode(WindowManager.LayoutParams.SOFT_INPUT_ADJUST_NOTHING)
        WindowCompat.getInsetsController(window, window.decorView).run {
            systemBarsBehavior = WindowInsetsControllerCompat.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
            hide(WindowInsetsCompat.Type.systemBars())
        }

        val context = PreviewContext(this, fontScale)
        // The editor reads the controls from their store: the fake ones, in the preview's preferences.
        ControlsStore.save(context, screen.controls)
        val prefix = "run=$run screen=$name fs=${fontScale ?: "system"}"
        val view = ComposeView(context)
        view.setContent {
            LauncherTheme {
                OverlayPreview(screen, onScreen, prefix) { w, h -> fakeLayout(context, w, h) }
            }
        }
        setContentView(view)
        Log.i(
            OverlayProbe.TAG,
            "$prefix kind=START device=${Build.MANUFACTURER}/${Build.MODEL} sdk=${Build.VERSION.SDK_INT} " +
                "systemFontScale=${resources.configuration.fontScale} controller=$controller onScreen=$onScreen",
        )
        val reported = AtomicBoolean(false)
        lifecycleScope.launch {
            delay(screen.startMs)
            for (step in screen.steps) {
                perform(step, view, context, prefix)
                delay(STEP_MS)
            }
            delay(SETTLE_MS)
            OverlayProbe.report(prefix, windowManager.currentWindowMetrics.bounds)
            reported.set(true)
        }
        // No READY in time: the main thread is stuck (a layout that never ends); log where it is.
        val main = Thread.currentThread()
        Thread {
            Thread.sleep(screen.startMs + STEP_MS * screen.steps.size + SETTLE_MS + STUCK_MS)
            if (!reported.get() && !isFinishing && !isDestroyed) {
                Log.e(OverlayProbe.TAG, "$prefix kind=STUCK main thread:\n" + main.stackTrace.take(60).joinToString("\n") { "  at $it" })
            }
        }.apply { isDaemon = true }.start()
    }

    private fun controllerConnected(): Boolean = InputDevice.getDeviceIds().any { id ->
        InputDevice.getDevice(id)?.let { isController(it.sources, it.isVirtual) } == true
    }

    private suspend fun perform(step: PreviewStep, view: View, context: Context, prefix: String) {
        when (step) {
            is PreviewStep.Click -> {
                val label = context.getString(step.label)
                val done = OverlayProbe.click(label)
                Log.i(OverlayProbe.TAG, "$prefix kind=STEP click=\"$label\" done=$done")
            }
            is PreviewStep.ClickText -> {
                val done = OverlayProbe.click(step.text)
                Log.i(OverlayProbe.TAG, "$prefix kind=STEP click=\"${step.text}\" done=$done")
            }
            is PreviewStep.Type -> {
                val field = OverlayProbe.firstEditText()
                field?.setText(step.text)
                Log.i(OverlayProbe.TAG, "$prefix kind=STEP type=\"${step.text}\" done=${field != null}")
            }
            is PreviewStep.TapPiece -> {
                // The editor's layout for this size, as ControlsEditor finds it (dp rounded as it rounds them).
                val density = context.resources.displayMetrics.density
                val layout = LayoutStore.layout(context, (view.width / density).roundToInt(), (view.height / density).roundToInt())
                val area = layout.piece(step.id)?.area(view.width.toFloat(), view.height.toFloat())
                if (area != null) tap(view, area.centerX, area.centerY)
                Log.i(OverlayProbe.TAG, "$prefix kind=STEP tap=${step.id} done=${area != null}")
            }
        }
    }

    /** A finger's short tap at [x], [y] of [view], as the editor's own gesture detector takes one. */
    private suspend fun tap(view: View, x: Float, y: Float) {
        val down = SystemClock.uptimeMillis()
        fun event(action: Int, time: Long): MotionEvent = MotionEvent.obtain(
            down, time, action, 1,
            arrayOf(MotionEvent.PointerProperties().apply { id = 0; toolType = MotionEvent.TOOL_TYPE_FINGER }),
            arrayOf(MotionEvent.PointerCoords().apply { this.x = x; this.y = y; pressure = 1f; size = 1f }),
            0, 0, 1f, 1f, 0, 0, InputDevice.SOURCE_TOUCHSCREEN, 0,
        )
        event(MotionEvent.ACTION_DOWN, down).also { view.dispatchTouchEvent(it) }.recycle()
        delay(60)
        event(MotionEvent.ACTION_UP, SystemClock.uptimeMillis()).also { view.dispatchTouchEvent(it) }.recycle()
    }
}

/**
 * The activity's Context for the overlay: its own font scale when one is asked ([fontScale]: an
 * override of this activity's configuration only; Compose's LocalDensity, the dialogs' windows and
 * Android's own views all take it from here), and preferences of its own, so the stores that read
 * them ([ControlsStore], [LayoutStore]) read and write the preview's, never the app's.
 */
private class PreviewContext(base: Context, fontScale: Float?) : ContextThemeWrapper(base, android.R.style.Theme_Material_NoActionBar) {
    init {
        if (fontScale != null) applyOverrideConfiguration(Configuration().apply { this.fontScale = fontScale })
    }

    override fun getSharedPreferences(name: String?, mode: Int): SharedPreferences =
        super.getSharedPreferences("overlay_preview_$name", mode)
}

/** What a screen does after it shows, before the probe: the presses that open its surface. */
private sealed interface PreviewStep {
    /** The node with this text or description, through its click action, as a tap would. */
    data class Click(@param:StringRes val label: Int) : PreviewStep

    /** The node with this literal text (a button's cap), through its click action. */
    data class ClickText(val text: String) : PreviewStep

    /** The first text field of the activity's windows (a number input's) gets [text]. */
    data class Type(val text: String) : PreviewStep

    /** A finger taps the middle of the on-screen piece [id] (the editor selects it). */
    data class TapPiece(val id: String) : PreviewStep
}

/**
 * One preview: the Game view's [layer] at the start, the menu's [tab] and state ([asking],
 * [launcher], [frameRate], 0 for Full; [tilesOn]: every tile on, Android's keyboard shown as up;
 * [mouseActive]: the controller in mouse mode),
 * the player's [controls], the Frame rate input open over the menu, or only the fps counter
 * ([fpsOnly]); then [steps], from [startMs] after the start.
 */
private class PreviewScreen(
    val layer: GameLayer,
    val tab: MenuTab = MenuTab.QUICK,
    val asking: Boolean = false,
    val launcher: Boolean = false,
    val frameRate: Int = AppSettings.FULL_FRAME_RATE,
    val tilesOn: Boolean = false,
    val mouseActive: Boolean = false,
    val controls: Controls = PLAYER_CONTROLS,
    val frameRateInput: Boolean = false,
    val fpsOnly: FpsReading? = null,
    val steps: List<PreviewStep> = emptyList(),
    val startMs: Long = 600,
)

/** A player's controls with a few changes: three buttons in pad mode and one in mouse mode. */
private val PLAYER_CONTROLS = Controls()
    .bind(PadButton.L2, false, PadAction.Key(Keys.XK_ALT_L))
    .bind(PadButton.L3, false, PadAction.Key(Keys.XK_TAB))
    .bind(PadButton.R3, false, PadAction.Key('w'.code))
    .bind(PadButton.Y, true, PadAction.Key('i'.code))

/** The fake layout's custom button: F5 (a skill key), labelled "TP". */
private const val CUSTOM_ID = "custom-1"

/** The editor's hint goes 4 s after it opens, with no fade: steps in the editor start after that. */
private const val AFTER_HINT_MS = 5_000L
private const val STEP_MS = 700L
private const val SETTLE_MS = 900L

/** How long after the settle the preview waits for READY before it logs a stuck main thread. */
private const val STUCK_MS = 5_000L

private val MENU = PreviewStep.Click(R.string.game_view_menu)

private val SCREENS: Map<String, PreviewScreen> = linkedMapOf(
    // Over the game: the menu glyph, the on-screen controls and the fps counter.
    "ingame" to PreviewScreen(GameLayer.NONE),
    // The menu's Quick tab: Display Fit, Frame rate the screen's most (Full), FPS counter on.
    "quick" to PreviewScreen(GameLayer.MENU),
    "quick-on" to PreviewScreen(GameLayer.MENU, tilesOn = true, frameRate = 60),
    "quick-launcher" to PreviewScreen(GameLayer.MENU, launcher = true),
    // The Settings tab (the gear), and the Stop question in its place.
    "settings" to PreviewScreen(GameLayer.MENU, tab = MenuTab.SETTINGS),
    "stop" to PreviewScreen(GameLayer.MENU, tab = MenuTab.SETTINGS, asking = true),
    "stop-launcher" to PreviewScreen(GameLayer.MENU, tab = MenuTab.SETTINGS, asking = true, launcher = true),
    // The Controls screen, as the menu's gamepad opens it: pad mode, mouse mode, the defaults.
    // Pad mode in use, viewed; mouse mode viewed (its Use mouse mode button); mouse mode in use as
    // the screen opens; after Use mouse mode; the defaults.
    "controls" to PreviewScreen(GameLayer.CONTROLS),
    "controls-mouse" to PreviewScreen(GameLayer.CONTROLS, steps = listOf(PreviewStep.Click(R.string.controls_mouse_mode))),
    "controls-mouse-active" to PreviewScreen(GameLayer.CONTROLS, mouseActive = true),
    "controls-use" to PreviewScreen(
        GameLayer.CONTROLS,
        steps = listOf(PreviewStep.Click(R.string.controls_mouse_mode), PreviewStep.Click(R.string.controls_use_mouse)),
    ),
    "controls-default" to PreviewScreen(GameLayer.CONTROLS, controls = Controls()),
    // A binding row's press: the action picker for the d-pad's up in mouse mode.
    "picker" to PreviewScreen(
        GameLayer.CONTROLS,
        steps = listOf(PreviewStep.Click(R.string.controls_mouse_mode), PreviewStep.ClickText("↑")),
    ),
    "pointer" to PreviewScreen(GameLayer.CONTROLS, steps = listOf(PreviewStep.Click(R.string.controls_pointer_speed_title))),
    "pointer-wrong" to PreviewScreen(
        GameLayer.CONTROLS, startMs = 600,
        steps = listOf(PreviewStep.Click(R.string.controls_pointer_speed_title), PreviewStep.Type("9"), PreviewStep.Click(R.string.settings_frame_rate_ok)),
    ),
    // The Quick tab's Frame rate opens its input over the menu.
    "framerate" to PreviewScreen(GameLayer.MENU, frameRateInput = true),
    "framerate-wrong" to PreviewScreen(
        GameLayer.MENU, frameRateInput = true, startMs = 1_500,
        steps = listOf(PreviewStep.Type("10"), PreviewStep.Click(R.string.settings_frame_rate_ok)),
    ),
    "fps" to PreviewScreen(GameLayer.NONE, fpsOnly = FpsReading(60f, null)),
    "fps-onscreen" to PreviewScreen(GameLayer.NONE, fpsOnly = FpsReading(73f, 60f)),
    // The on-screen controls editor, as Edit on-screen layout opens it over the game: its hint shows for 4 s.
    "editor" to PreviewScreen(GameLayer.EDIT),
    "editor-menu" to PreviewScreen(GameLayer.EDIT, startMs = AFTER_HINT_MS, steps = listOf(MENU)),
    "editor-reset" to PreviewScreen(GameLayer.EDIT, startMs = AFTER_HINT_MS, steps = listOf(MENU, PreviewStep.Click(R.string.editor_reset_default))),
    "editor-piece" to PreviewScreen(GameLayer.EDIT, startMs = AFTER_HINT_MS, steps = listOf(PreviewStep.TapPiece("face"))),
    "editor-button" to PreviewScreen(GameLayer.EDIT, startMs = AFTER_HINT_MS, steps = listOf(PreviewStep.TapPiece(CUSTOM_ID))),
    "editor-add" to PreviewScreen(GameLayer.EDIT, startMs = AFTER_HINT_MS, steps = listOf(MENU, PreviewStep.Click(R.string.editor_add))),
    "editor-anykey" to PreviewScreen(
        GameLayer.EDIT, startMs = AFTER_HINT_MS,
        steps = listOf(MENU, PreviewStep.Click(R.string.editor_add), PreviewStep.Click(R.string.editor_any_key)),
    ),
)

/**
 * The default layout for a [width] x [height] dp screen with one custom button (F5, "TP") where a
 * player might put it, stored as the preview's layout for that size so the editor finds it too.
 */
private fun fakeLayout(context: Context, width: Int, height: Int): ControlsLayout {
    val (withButton, id) = ControlsLayout.default(width.toFloat(), height.toFloat())
        .withCustom(PadAction.Key(Keys.XK_F1 + 4), "TP")
    val layout = withButton.piece(id)?.let { withButton.replace(it.copy(x = 0.36f, y = 0.3f)) } ?: withButton
    LayoutStore.save(context, width, height, layout)
    return layout
}

/** A neutral dark gradient where the game's picture would be. */
private val NEUTRAL = Brush.linearGradient(listOf(Color(0xFF3C434A), Color(0xFF22282D), Color(0xFF141719)))

/**
 * The overlay as the Game view layers it over the game (GameView.kt, its `if (live)` block), with
 * the [screen]'s fake state; its controls work, so a person can also tap through it.
 */
@Composable
private fun OverlayPreview(screen: PreviewScreen, onScreenAtStart: Boolean, prefix: String, layoutFor: (Int, Int) -> ControlsLayout) {
    val context = LocalContext.current
    // The Controls screen opens from the menu, as in the game: its strip takes the menu's place.
    var layer by remember { mutableStateOf(if (screen.layer == GameLayer.CONTROLS) GameLayer.MENU else screen.layer) }
    var menuHeight by remember { mutableStateOf<Int?>(null) }
    var tab by remember { mutableStateOf(screen.tab) }
    var asking by remember { mutableStateOf(screen.asking) }
    var keyboard by remember { mutableStateOf(screen.tilesOn) }
    var onScreen by remember { mutableStateOf(onScreenAtStart || screen.tilesOn) }
    var showFps by remember { mutableStateOf(true) }
    var display by remember { mutableStateOf(GameDisplay.FIT) }
    var frameRate by remember { mutableIntStateOf(screen.frameRate) }
    var frameRateInput by remember { mutableStateOf(screen.frameRateInput) }
    var slower by remember { mutableStateOf(true) }
    var sound by remember { mutableStateOf(true) }
    var keepOn by remember { mutableStateOf(true) }
    var controls by remember { mutableStateOf(screen.controls) }
    var mouseActive by remember { mutableStateOf(screen.mouseActive) }
    var touch by remember { mutableStateOf(TouchMode.TRACKPAD) }
    val full = remember { ScreenRate.load(context) }
    val pad = remember { VirtualPad {} }
    val buttons = remember { CustomButtons {} }
    val fps = FpsReading(60f, null)
    // As the Game view: any change of layer forgets the menu's question, and a closed menu opens on Quick.
    val go = { to: GameLayer ->
        layer = to
        asking = false
        if (to != GameLayer.MENU) tab = MenuTab.QUICK
    }
    var opened by remember { mutableStateOf(false) }
    LaunchedEffect(menuHeight) {
        if (screen.layer == GameLayer.CONTROLS && menuHeight != null && !opened) {
            opened = true
            go(GameLayer.CONTROLS)
        }
    }
    BoxWithConstraints(Modifier.fillMaxSize().background(NEUTRAL)) {
        val widthDp = maxWidth.value.roundToInt()
        val heightDp = maxHeight.value.roundToInt()
        val layout = remember(widthDp, heightDp) { layoutFor(widthDp, heightDp) }
        Geometry(prefix)
        if (screen.fpsOnly != null) {
            FpsCounter({ screen.fpsOnly }, Modifier.align(Alignment.TopCenter).windowInsetsPadding(WindowInsets.safeDrawing))
        } else CompositionLocalProvider(LocalIndication provides PressTint) {
            // As the Game view: a press shows a plain tint, not Material's ripple.
            if (layer.tapCloses) TapToClose { go(GameLayer.NONE) }
            if (layer.padShown(asking) && onScreen && !screen.launcher) {
                OnScreenControls(layout, controls.onScreenOpacity, pad, buttons, inert = layer.padInert(slower))
            }
            if (layer == GameLayer.NONE) MenuGlyph(Modifier.align(Alignment.TopEnd).windowInsetsPadding(WindowInsets.safeDrawing)) { go(GameLayer.MENU) }
            if (layer.fpsShown(showFps, screen.launcher)) FpsCounter({ fps }, Modifier.align(Alignment.TopCenter).windowInsetsPadding(WindowInsets.safeDrawing))
            if (layer == GameLayer.EDIT) ControlsEditor(sample = false, onClose = { go(GameLayer.NONE) })
            GameMenu(
                visible = layer == GameLayer.MENU,
                tab = tab,
                onTab = {
                    tab = it
                    asking = false
                },
                asking = asking,
                onAsking = { asking = it },
                state = MenuState(
                    launcher = screen.launcher,
                    onScreen = onScreen,
                    showFps = showFps,
                    display = display,
                    keyboard = keyboard,
                    frameRate = frameRateLabel(frameRate, full),
                    slowerInBackground = slower,
                    soundInBackground = sound,
                    keepScreenOn = keepOn,
                ),
                actions = MenuActions(
                    onClose = { go(GameLayer.NONE) },
                    onOnScreen = { onScreen = it },
                    onShowFps = { showFps = it },
                    onDisplay = { display = display.other },
                    onKeyboard = { on ->
                        keyboard = on
                        if (on) go(GameLayer.NONE)
                    },
                    onFrameRate = { frameRateInput = true },
                    onControls = { go(GameLayer.CONTROLS) },
                    onSlowerInBackground = { slower = it },
                    onSoundInBackground = { sound = it },
                    onKeepScreenOn = { keepOn = it },
                    onStop = { go(GameLayer.NONE) },
                ),
                focusTab = 0,
                onPanelHeight = { menuHeight = it },
            )
            ControlsScreen(
                visible = layer == GameLayer.CONTROLS,
                controls = controls,
                activeMouse = mouseActive,
                canUseMode = true,
                save = { controls = it },
                touch = touch,
                onTouch = { touch = it },
                onUseMode = { mouseActive = it },
                menuHeight = menuHeight,
                onStop = { stop ->
                    if (stop != MenuStop.CONTROLS) {
                        go(GameLayer.MENU)
                        tab = if (stop == MenuStop.QUICK) MenuTab.QUICK else MenuTab.SETTINGS
                    }
                },
                onClose = { go(GameLayer.NONE) },
                onPopup = {},
                onEditOnScreen = { go(GameLayer.EDIT) },
            )
            if (frameRateInput) {
                FrameRateInput(frameRate, full, { frameRate = it }) { frameRateInput = false }
            }
        }
    }
}

/** Logs the safe area this screen has and what 14 sp is here (one `kind=GEOMETRY` line). */
@Composable
private fun Geometry(prefix: String) {
    BoxWithConstraints(Modifier.fillMaxSize().windowInsetsPadding(WindowInsets.safeDrawing)) {
        val density = LocalDensity.current
        val line = "$prefix kind=GEOMETRY safeArea=${maxWidth.value.roundToInt()}x${maxHeight.value.roundToInt()}dp " +
            "density=${density.density} fontScale=${density.fontScale} sp14=${with(density) { 14.sp.toDp().value }}dp"
        LaunchedEffect(line) { Log.i(OverlayProbe.TAG, line) }
    }
}
