package com.pd2launcher.app.ui

import android.app.Activity
import android.content.Context
import android.content.ContextWrapper
import android.hardware.input.InputManager
import android.os.Handler
import android.os.Looper
import android.os.SystemClock
import android.text.InputType
import android.util.Log
import android.view.Choreographer
import android.view.Display
import android.view.InputDevice
import android.view.KeyEvent
import android.view.MotionEvent
import android.view.PointerIcon
import android.view.Surface
import android.view.SurfaceHolder
import android.view.SurfaceView
import android.view.ViewConfiguration
import android.view.Window
import android.view.WindowManager
import android.view.inputmethod.BaseInputConnection
import android.view.inputmethod.EditorInfo
import android.view.inputmethod.InputConnection
import android.view.inputmethod.InputMethodManager
import androidx.activity.compose.BackHandler
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.isImeVisible
import androidx.compose.foundation.layout.safeDrawing
import androidx.compose.foundation.layout.requiredSize
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.viewinterop.AndroidView
import androidx.core.view.ViewCompat
import androidx.core.view.WindowInsetsCompat
import com.pd2launcher.app.AppSettings
import com.pd2launcher.app.AppSettingsStore
import com.pd2launcher.app.BuildConfig
import com.pd2launcher.app.FpsReading
import com.pd2launcher.app.FrameClock
import com.pd2launcher.app.FrameCopy
import com.pd2launcher.app.FrameCounts
import com.pd2launcher.app.FrameRateVote
import com.pd2launcher.app.Framebuffer
import com.pd2launcher.app.GameEnvironment
import com.pd2launcher.app.GameFrameRate
import com.pd2launcher.app.GameDisplay
import com.pd2launcher.app.GameInput
import com.pd2launcher.app.GameResolution
import com.pd2launcher.app.LauncherSettings
import com.pd2launcher.app.PresentServer
import com.pd2launcher.app.R
import com.pd2launcher.app.ScreenRate
import com.pd2launcher.app.ScreenRateWatch
import com.pd2launcher.app.SessionService
import com.pd2launcher.app.SessionStatus
import com.pd2launcher.app.XwdHeader
import com.pd2launcher.app.input.Controls
import com.pd2launcher.app.input.ControlsStore
import com.pd2launcher.app.input.Keys
import com.pd2launcher.app.input.CustomButtons
import com.pd2launcher.app.input.GamePicture
import com.pd2launcher.app.input.PictureDebounce
import com.pd2launcher.app.input.isController
import com.pd2launcher.app.input.LayoutStore
import com.pd2launcher.app.input.Mouse
import com.pd2launcher.app.input.PadAxes
import com.pd2launcher.app.input.PadInput
import com.pd2launcher.app.input.RelayEvent
import com.pd2launcher.app.input.TouchGestures
import com.pd2launcher.app.input.VirtualPad
import com.pd2launcher.app.input.PadState
import java.io.File
import kotlin.math.roundToInt
import java.io.IOException
import java.util.concurrent.Executors
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.withContext

/** What shows over the game: nothing, the in-game menu, the Controls screen, or the on-screen controls editor. */
enum class GameLayer {
    NONE,
    MENU,
    CONTROLS,
    EDIT,
    ;

    /**
     * The on-screen controls show and play (when [Controls.onScreenShown] says so): over the game
     * and beside the menu, which draws over the pieces it overlaps and takes their touches, so its
     * On-screen controls tile shows at once (with Slower game in the background on, the game is in
     * the background beside the menu: [suspends]). The menu [asking] whether to stop, the Controls
     * screen and the editor (which has its own sample) hide them.
     */
    fun padShown(asking: Boolean = false): Boolean = this == NONE || (this == MENU && !asking)

    /**
     * The on-screen controls show dimmed and take no input ([OnScreenControls]): the game is in
     * the background for this overlay ([suspends] with Slower game in the background on,
     * [tellBackground]); else they play as over the game. The same rule as GameSurface.overlayFocus.
     */
    fun padInert(tellBackground: Boolean): Boolean = suspends && tellBackground

    /**
     * The game lets go of the controller and of touch: the controller drives the native screens
     * and the on-screen controls are the only way into the game (beside the menu). With Slower
     * game in the background on, the game goes to the background meanwhile, as off the screen.
     */
    val suspends: Boolean get() = this != NONE

    /**
     * Android's keyboard stays as it is: over the game, and beside the menu, whose Keyboard tile
     * shows it and hides it. The Controls screen and the editor close it: they need the whole height.
     */
    val keepsKeyboard: Boolean get() = this == NONE || this == MENU

    /** A tap on the game picture, outside the menu and the on-screen controls, closes the menu and never reaches the game. */
    val tapCloses: Boolean get() = this == MENU

    /** The fps counter shows, when [showFps] is on, over the game PD2 draws (never over the official [launcher]) and beside the menu, so its tile shows at once. */
    fun fpsShown(showFps: Boolean, launcher: Boolean): Boolean = showFps && !launcher && (this == NONE || this == MENU)
}

/**
 * The Game view: the X screen ([framebuffer]) copied into a SurfaceView every display frame,
 * placed as Display says ([GameDisplay], from Settings or the in-game menu, followed live;
 * [GameResolution.viewGeometry]): with Fit to screen what PD2 shows (its 4:3 menu while
 * [GamePicture] says MENU, else its 16:9 game picture) as large as the screen allows, the X
 * screen's other parts past the screen's edges where they do not fit; with Center at its own
 * pixels in the middle. Or the game's own buffers on a layer above it while it
 * presents them there ([PresentServer]), the display asked for its highest mode (at the screen's
 * full rate, [ScreenRate]) or the player's reduced frame rate, which also holds the game to it
 * ([AppSettings.frameRate]), and the player's
 * input into the game: touch and a mouse as the X screen's mouse (touch through gestures, a mouse 1:1),
 * keyboard keys and Android's keyboard as X keys, the controller as the game's XInput pad
 * (the chord pressed briefly: mouse mode), and the on-screen controls as game keys. The in-game
 * menu ([GameMenu]) opens from the glyph, the chord held, or Back; the game keeps running and
 * showing beside it, the on-screen controls stay shown and playable where the menu does not cover
 * them ([GameLayer.padShown]; dimmed and inert with Slower game in the background on,
 * [GameLayer.padInert]), the controller lets go of the game and drives the menu, and a tap
 * elsewhere on the game closes it. Its gamepad opens the Controls screen ([ControlsScreen]).
 * Android's keyboard never pushes the game up; the menu keeps it as it is (its Keyboard tile
 * shows the real state), the Controls screen and the editor close it ([GameLayer.keepsKeyboard]).
 * The screen stays on as Settings → Keep the screen on says (MainActivity sets the window's flag;
 * a view of its own would keep it on whatever the setting).
 *
 * [presentCounter]: the game's present counter, for the fps counter ([SessionPaths.presentCounter]).
 * [preview] (debug builds): the overlay over an empty screen, as if the game ran, at that layer;
 * [previewLauncher]: as if the official PD2 launcher ran.
 */
@OptIn(ExperimentalLayoutApi::class)
@Composable
fun GameView(
    framebuffer: Framebuffer?,
    running: Boolean,
    presentCounter: File? = null,
    preview: GameLayer? = null,
    previewLauncher: Boolean = false,
) {
    var header by remember { mutableStateOf<XwdHeader?>(null) }
    LaunchedEffect(running, framebuffer) {
        header = null
        while (running && framebuffer != null && header == null) {
            header = withContext(Dispatchers.IO) { framebuffer.header() }
            if (header == null) delay(200)
        }
    }
    val context = LocalContext.current
    remember { ControlsStore.load(context) }
    val controls by ControlsStore.current.collectAsState()
    val input by GameEnvironment.input.collectAsState()
    var surface by remember { mutableStateOf<GameSurface?>(null) }
    var layer by remember(preview) { mutableStateOf(preview ?: GameLayer.NONE) }
    // The menu slides in from the game and out to it; to and from the Controls screen, which keeps
    // the menu's strip in place, it only fades.
    var menuFades by remember { mutableStateOf(false) }
    val go = { to: GameLayer ->
        menuFades = layer == GameLayer.CONTROLS || to == GameLayer.CONTROLS
        layer = to
    }
    // The menu opens on Quick, with no question; a change of tab forgets the question. Both change
    // as the menu opens, not as it leaves, so its look stays while it slides out.
    var menuTab by remember { mutableStateOf(MenuTab.QUICK) }
    var asking by remember { mutableStateOf(false) }
    // The shoulder buttons' steps into a menu tab: each moves the focus into it.
    var focusTab by remember { mutableIntStateOf(0) }
    // The menu's panel's height (px): the Controls screen puts its strip where the menu has it.
    var menuHeight by remember { mutableStateOf<Int?>(null) }
    // The Controls screen shows its action picker or the Pointer speed input: no step then.
    var controlsPopup by remember { mutableStateOf(false) }
    val openMenu = {
        menuTab = MenuTab.QUICK
        asking = false
        go(GameLayer.MENU)
    }
    // The strip's stop shows: a menu tab, or the Controls screen.
    val showStop = { stop: MenuStop ->
        if (stop == MenuStop.CONTROLS) {
            go(GameLayer.CONTROLS)
        } else {
            menuTab = if (stop == MenuStop.QUICK) MenuTab.QUICK else MenuTab.SETTINGS
            asking = false
            go(GameLayer.MENU)
        }
    }
    val controller = controllerConnected(onRemoved = { surface?.releaseController() })
    val status by SessionService.status.collectAsState()
    // The launcher's own window; a game it started is the game's ([Play.officialLauncherShown]).
    val launcher = previewLauncher || Play.officialLauncherShown(status)
    // The strip has the Controls stop (not over the official launcher); read when a key comes.
    val stripControls by rememberUpdatedState(MenuItems.of(launcher).controls)
    // The Launcher's Sound in background, once it has sent its settings and takes changes.
    val soundInBackground = LauncherSettings.view(status.snapshot?.settings, status.engine == SessionStatus.Engine.CONNECTED)
        ?.takeIf { it.editable }?.soundInBackground
    val app by AppSettingsStore.current.collectAsState()
    // The launcher is a mouse/touch window: no menu/world picture to read.
    val picture = gamePicture(framebuffer, if (running && !launcher) header else null)
    // Android's keyboard as it is, closed by its own controls too: here it opens only for the game.
    val keyboard = WindowInsets.isImeVisible
    val buttons = remember { CustomButtons { events -> surface?.send(events) } }
    val virtualPad = remember { VirtualPad { state -> surface?.virtual(state) } }
    val live = running || preview != null
    var frameRateInput by remember { mutableStateOf(false) }
    // L1 and R1 step one stop along the strip from the menu and from the Controls screen, wherever
    // the focus is (also nowhere, in touch mode), and stop at its ends; true: the key was theirs.
    val shoulder = { event: KeyEvent ->
        val from = MenuStop.from(event.keyCode, layer, menuTab, popup = frameRateInput || controlsPopup)
        if (from != null && event.action == KeyEvent.ACTION_DOWN && event.repeatCount == 0) {
            MenuStop.step(from, event.keyCode, stripControls)?.let { to ->
                showStop(to)
                if (to != MenuStop.CONTROLS) focusTab++
            }
        }
        from != null
    }
    // The controller's mode now (the chord switches it, the Controls screen too).
    var mouseMode by remember { mutableStateOf(false) }
    // The screen's full rate, live: the Game view's measurement can change it ([ScreenRate]).
    remember { ScreenRate.load(context) }
    val fullFrameRate by ScreenRate.full.collectAsState()
    // The game's own frame rate (its presents, counted where it sends them), for the counter.
    var fps by remember { mutableStateOf<FpsReading?>(null) }
    LaunchedEffect(surface) {
        val rate = GameFrameRate()
        while (true) {
            fps = rate.sample(System.nanoTime(), surface?.counts())
            delay(250)
        }
    }

    // Android's keyboard covers the bottom of the game instead of panning the whole window.
    DisposableEffect(Unit) {
        val window = context.activity()?.window
        val previous = window?.attributes?.softInputMode
        window?.setSoftInputMode(WindowManager.LayoutParams.SOFT_INPUT_ADJUST_NOTHING)
        onDispose { if (previous != null) window?.setSoftInputMode(previous) }
    }
    BackHandler(enabled = live) {
        when (layer) {
            GameLayer.NONE, GameLayer.CONTROLS -> openMenu()
            GameLayer.MENU, GameLayer.EDIT -> go(GameLayer.NONE)
        }
    }

    BoxWithConstraints(Modifier.fillMaxSize().background(Color.Black), contentAlignment = Alignment.Center) {
        val screen = header
        val widthDp = maxWidth.value.roundToInt()
        val heightDp = maxHeight.value.roundToInt()
        val saved by LayoutStore.version.collectAsState()
        val layout = remember(widthDp, heightDp, saved) { LayoutStore.layout(context, widthDp, heightDp) }
        if (preview == null) {
            if (!running || screen == null || framebuffer == null) {
                Text(stringResource(R.string.game_view_waiting), style = Type.Body, color = Palette.Text)
            } else key(screen, framebuffer) {
                // Display: what PD2 shows as large as fits, or the X screen's own pixels in the middle; the
                // official launcher's whole window always fits (it has a resolution of its own). A new size
                // keeps both layers (GameSurface.surfaceChanged).
                val display = if (launcher) GameDisplay.FIT else app.gameDisplay
                val geometry = GameResolution.viewGeometry(
                    display, GameResolution.shownPicture(launcher, picture == GamePicture.MENU),
                    screen.width to screen.height, constraints.maxWidth, constraints.maxHeight,
                )
                if (BuildConfig.DEBUG) {
                    LaunchedEffect(geometry) {
                        val what = if (launcher) "LAUNCHER" else if (picture == GamePicture.MENU) "MENU" else "GAME"
                        Log.i(
                            GEOMETRY_TAG,
                            "display $display $what x-screen ${screen.width}x${screen.height} " +
                                "area ${constraints.maxWidth}x${constraints.maxHeight} " +
                                "view ${geometry.view.first}x${geometry.view.second} picture ${geometry.picture}",
                        )
                    }
                }
                val (viewWidth, viewHeight) = geometry.view
                val density = LocalDensity.current
                AndroidView(
                    modifier = with(density) { Modifier.requiredSize(viewWidth.toDp(), viewHeight.toDp()) },
                    factory = { context ->
                        GameSurface(context, framebuffer, presentCounter, screen)
                            .also { surface = it }
                    },
                    // Gone with its X screen: no old pad to switch, and the next one starts in pad mode.
                    onRelease = {
                        if (surface === it) surface = null
                        mouseMode = false
                    },
                    update = {
                        it.onMenu = openMenu
                        it.onMouseMode = { on ->
                            mouseMode = on
                            if (BuildConfig.DEBUG) Log.i(PAD_MODE_TAG, "mouse mode $on")
                        }
                        it.onOverlayKey = shoulder
                        it.controls = controls
                        it.input = input
                        it.tellBackground = app.lowFrameRateInBackground
                        it.keepsKeyboard = layer.keepsKeyboard
                        it.suspended = layer.suspends
                        it.customButtons = buttons
                        it.virtualPad = virtualPad
                        it.frameRate = app.frameRate
                    },
                )
            }
        }
        if (live) {
            val onScreen = controls.onScreenOn(controller)
            // Under the on-screen controls and the menu, over the game: what they leave of the screen.
            if (layer.tapCloses) TapToClose { go(GameLayer.NONE) }
            if (layer.padShown(asking) && controls.onScreenShown(controller, picture, launcher)) {
                OnScreenControls(layout, controls.onScreenOpacity, virtualPad, buttons, inert = layer.padInert(app.lowFrameRateInBackground))
            }
            if (layer == GameLayer.NONE) MenuGlyph(Modifier.align(Alignment.TopEnd).windowInsetsPadding(WindowInsets.safeDrawing), openMenu)
            // Top centre, clear of the default controls and of the menu; it takes no touch.
            if (layer.fpsShown(app.showFps, launcher)) FpsCounter({ fps }, Modifier.align(Alignment.TopCenter).windowInsetsPadding(WindowInsets.safeDrawing))
            if (layer == GameLayer.EDIT) ControlsEditor(sample = preview != null, onClose = { go(GameLayer.NONE) })
            GameMenu(
                visible = layer == GameLayer.MENU,
                tab = menuTab,
                onTab = {
                    menuTab = it
                    asking = false
                },
                asking = asking,
                onAsking = { asking = it },
                fades = menuFades,
                state = MenuState(
                    launcher = launcher,
                    onScreen = onScreen,
                    showFps = app.showFps,
                    display = app.gameDisplay,
                    keyboard = keyboard,
                    frameRate = frameRateLabel(app.frameRate, fullFrameRate),
                    slowerInBackground = app.lowFrameRateInBackground,
                    soundInBackground = soundInBackground,
                    keepScreenOn = app.keepScreenOn,
                ),
                actions = MenuActions(
                    onClose = { go(GameLayer.NONE) },
                    onOnScreen = { ControlsStore.save(context, controls.withOnScreen(it)) },
                    onShowFps = { AppSettingsStore.save(context, app.copy(showFps = it)) },
                    // At once, the menu open: the Game view follows the setting.
                    onDisplay = { AppSettingsStore.save(context, app.copy(gameDisplay = app.gameDisplay.other)) },
                    onKeyboard = { on ->
                        // Typing needs the game: the menu closes as the keyboard opens; it stays as the keyboard closes.
                        if (on) go(GameLayer.NONE)
                        surface?.showKeyboard(on)
                    },
                    onFrameRate = { frameRateInput = true },
                    onControls = { go(GameLayer.CONTROLS) },
                    onSlowerInBackground = { AppSettingsStore.save(context, app.copy(lowFrameRateInBackground = it)) },
                    onSoundInBackground = { SessionService.command(LauncherSettings.soundInBackground(it)) },
                    onKeepScreenOn = { AppSettingsStore.save(context, app.copy(keepScreenOn = it)) },
                    onStop = {
                        go(GameLayer.NONE)
                        SessionService.send(context, SessionService.ACTION_GAME_STOP)
                    },
                ),
                focusTab = focusTab,
                onPanelHeight = { menuHeight = it },
            )
            ControlsScreen(
                visible = layer == GameLayer.CONTROLS,
                controls = controls,
                activeMouse = mouseMode,
                save = { ControlsStore.save(context, it) },
                // As the chord: at once, in the running game; a preview has no game, only the screen follows.
                canUseMode = preview != null || (surface != null && input != null),
                onUseMode = { on -> if (preview != null) mouseMode = on else surface?.useMouseMode(on) },
                menuHeight = menuHeight,
                onStop = showStop,
                onClose = { go(GameLayer.NONE) },
                onPopup = { controlsPopup = it },
                onEditOnScreen = { go(GameLayer.EDIT) },
            )
            // Over the menu, which stays behind it.
            if (frameRateInput) {
                FrameRateInput(app.frameRate, fullFrameRate, { pick ->
                    AppSettingsStore.save(context, app.copy(frameRate = pick))
                    // At once: the clock and both layers' rate for the display.
                    surface?.applyFrameRate(pick) ?: FrameClock.setRate(FrameRateVote.rate(fullFrameRate, pick))
                }) { frameRateInput = false }
            }
        }
    }
}

/**
 * A controller (a gamepad device, built in or attached; [isController]) is there, followed live: a
 * pad that connects or wakes while the game shows counts at once. [onRemoved] runs when a
 * device goes, so what it held is let go.
 */
@Composable
private fun controllerConnected(onRemoved: () -> Unit): Boolean {
    val context = LocalContext.current
    fun now() = InputDevice.getDeviceIds().any { id ->
        InputDevice.getDevice(id)?.let { isController(it.sources, it.isVirtual) } == true
    }
    var connected by remember { mutableStateOf(now()) }
    val removed by rememberUpdatedState(onRemoved)
    DisposableEffect(context) {
        val manager = context.getSystemService(InputManager::class.java)
        val listener = object : InputManager.InputDeviceListener {
            override fun onInputDeviceAdded(deviceId: Int) { connected = now() }
            override fun onInputDeviceRemoved(deviceId: Int) {
                connected = now()
                removed()
            }
            override fun onInputDeviceChanged(deviceId: Int) { connected = now() }
        }
        manager.registerInputDeviceListener(listener, Handler(Looper.getMainLooper()))
        connected = now()
        onDispose { manager.unregisterInputDeviceListener(listener) }
    }
    return connected
}

/**
 * What PD2 shows ([GamePicture]), read every [PICTURE_MS] while a [screen] is known and steadied
 * ([PictureDebounce]); unknown otherwise. A few sampled pixels, off the main thread: of the game's
 * own buffer on the screen while it presents there ([PresentServer.readShown]; the X screen then
 * misses its frames), else of the framebuffer, mapped once. A Game view made again for the same X
 * screen (back from Settings) starts from the picture steadied last ([SteadiedPicture]), so its
 * size does not change for a moment.
 */
@Composable
private fun gamePicture(framebuffer: Framebuffer?, screen: XwdHeader?): GamePicture {
    var picture by remember { mutableStateOf(SteadiedPicture.of(framebuffer, screen)) }
    LaunchedEffect(framebuffer, screen) {
        val start = SteadiedPicture.of(framebuffer, screen)
        picture = start
        if (framebuffer == null || screen == null || screen.bitsPerPixel != 32) return@LaunchedEffect
        withContext(Dispatchers.IO) {
            val pixels = framebuffer.map()?.takeIf { it.header == screen } ?: return@withContext
            val steady = PictureDebounce(start)
            while (isActive) {
                val now = PresentServer.current.value?.readShown { GamePicture.of(it.width, it.height, it::pixel) }
                    ?: GamePicture.of(screen.width, screen.height, pixels::pixel)
                val next = steady.sample(now)
                SteadiedPicture.keep(framebuffer, screen, next)
                if (next != picture) withContext(Dispatchers.Main) { picture = next }
                delay(PICTURE_MS)
            }
        }
    }
    return picture
}

/** The picture [gamePicture] steadied last, for the X screen of one framebuffer and header. */
private object SteadiedPicture {
    private var framebuffer: Framebuffer? = null
    private var screen: XwdHeader? = null
    private var picture = GamePicture.UNKNOWN

    /** The picture kept for this X screen; unknown for another one. */
    @Synchronized
    fun of(framebuffer: Framebuffer?, screen: XwdHeader?): GamePicture =
        if (framebuffer != null && framebuffer === this.framebuffer && screen == this.screen) picture else GamePicture.UNKNOWN

    @Synchronized
    fun keep(framebuffer: Framebuffer, screen: XwdHeader, picture: GamePicture) {
        this.framebuffer = framebuffer
        this.screen = screen
        this.picture = picture
    }
}

/** How often the Game view looks at the shape of PD2's picture. */
private const val PICTURE_MS = 250L

/** Debug builds: one line per new geometry of the Game view, for checks without screen captures. */
private const val GEOMETRY_TAG = "pd2-view-geometry"

/** Debug builds: one line per change of the controller's mode, from the chord or the Controls screen. */
private const val PAD_MODE_TAG = "pd2-pad-mode"

/**
 * The X screen and the input into it. While attached it sees every key and controller motion of
 * the window first (it wraps the window's callback, so the activity's own mapping of A and B
 * never applies here), and it is the editor Android's keyboard types into. While [suspended]
 * (an overlay layer shows, [GameLayer.suspends]) it lets go of the controller, touch and the mouse and
 * passes keys on, so the controller drives the native screens; the on-screen controls still
 * reach the game ([virtual], [send]). [frameRate]: the player's frame rate, the app's clock's
 * ([FrameClock]) and, where it is below the screen's full rate, the display's; Full
 * ([AppSettings.FULL_FRAME_RATE]): the screen's full rate, which it measures while it shows
 * ([ScreenRate]), and the display's highest mode ([FrameRateVote]).
 */
private class GameSurface(
    context: Context,
    framebuffer: Framebuffer,
    presentCounter: File?,
    private val screen: XwdHeader,
) : SurfaceView(context) {
    /**
     * Settings → Slower game in the background, followed live: while the view does not show (the
     * app in the background, the screen off, the app's own screens over the game) and while an
     * overlay shows ([suspended]: the in-game menu, the Controls screen, their inputs, the editor),
     * the game's window loses the focus, so the game uses its own background frame rate
     * ([RelayEvent.Focus]).
     */
    var tellBackground = AppSettings().lowFrameRateInBackground
        set(value) {
            if (value == field) return
            field = value
            overlayFocus()
        }

    /** The game's window lost the focus for an overlay ([overlayFocus]). */
    private var overlayAway = false

    /**
     * An overlay shows ([suspended]) with [tellBackground] on: the game's window loses the focus,
     * as off the screen; it has it again as the overlay goes or the setting goes off. Off the
     * screen (no copy) nothing is sent: the window is away there, and the surface's return
     * ([surfaceChanged]) gives the focus back only with no overlay.
     */
    private fun overlayFocus() {
        // The same rule as GameLayer.padInert (the on-screen controls dimmed and inert meanwhile).
        val away = suspended && tellBackground
        if (away == overlayAway) return
        overlayAway = away
        if (copy == null) return
        if (away) releaseHeld()
        send(listOf(RelayEvent.Focus(!away)))
    }

    /**
     * Android's keyboard stays up while an overlay shows ([GameLayer.keepsKeyboard]: the menu, whose
     * Keyboard tile shows and hides it); else it closes as the overlay opens, so that has the whole height.
     */
    var keepsKeyboard = true
        set(value) {
            if (value == field) return
            field = value
            if (!value && suspended && keyboard) showKeyboard(false)
        }

    /**
     * The player's frame rate ([AppSettings.frameRate]), for the next time the surface comes and
     * for a change of the screen's full rate ([screenRate]).
     */
    var frameRate = AppSettings().frameRate

    /**
     * The display's real rate while the surface shows ([ScreenRate]): the screen's full rate
     * follows it, and the clock and both layers' vote follow the full rate at once. It counts the
     * buffers that go to the compositor ([FrameCopy.posted]).
     */
    private val screenRate = ScreenRateWatch(context, { display }, { copy?.posted() }) { applyFrameRate(frameRate) }

    /**
     * The player changed the frame rate to [pick] while the game shows, or the screen's full rate
     * changed ([screenRate]): the clock follows at once, and both layers ask the display for the
     * new rate at once (not only with the next surface).
     */
    fun applyFrameRate(pick: Int) {
        frameRate = pick
        val (vote, capped) = clockAndVote()
        copy?.setFrameRate(vote)
        PresentServer.current.value?.setFrameRate(vote, capped)
    }

    /**
     * Sets the clock for [frameRate] on the screen's full rate ([ScreenRate.full]; on a display other
     * than the built-in one its highest mode) and returns the rate the layers ask the display for
     * (the highest mode while [screenRate] measures, else [FrameRateVote.vote]) and whether the
     * player's rate is below the full rate ([FrameRateVote.cap]). It tells [screenRate] whether the
     * layers ask for the highest mode, which starts or stops its checks.
     */
    private fun clockAndVote(): Pair<Float, Boolean> {
        val display = display
        val highest = FrameRateVote.highest(display)
        val full = if (display == null || display.displayId == Display.DEFAULT_DISPLAY) ScreenRate.full.value else highest
        val capped = FrameRateVote.cap(full, frameRate) != null
        FrameClock.setRate(FrameRateVote.rate(full, frameRate))
        val top = screenRate.measuring || !capped
        screenRate.vote(top)
        return (if (top) highest else FrameRateVote.vote(highest, full, frameRate)) to capped
    }

    private val window: Window? = context.activity()?.window
    private var callback: Window.Callback? = null
    private var sends = Executors.newSingleThreadExecutor { Thread(it, "pd2-input") }
    private val handler = Handler(Looper.getMainLooper())
    /** The token of a tap's click still to come on [handler] ([TouchGestures.later]). */
    private val pendingClick = Any()
    private var pad: PadInput? = null
    private var touch: TouchGestures? = null
    private var touchId = -1
    private val mouse = Mouse()
    /** A mouse is over the game, not over an overlay ([onResolvePointerIcon]). */
    private var mouseOver = false
    /** The keyboard keys sent to X as pressed and not yet released (keysyms). */
    private val keysDown = HashSet<Int>()
    private var lastFrameNanos = 0L
    /** The controller's clock ([frame]) is posted. */
    private var clockRunning = false
    private var keyboard = false
    private val longPress = Runnable { touch?.longPressDue(SystemClock.uptimeMillis()) }
    private var copy: FrameCopy? = null
    private var copyShape: List<Int>? = null

    /** The copy's counts so far ([FrameCopy.counts]); null while not copying. Main thread. */
    fun counts(): FrameCounts? = copy?.counts()

    /** The chord was held: open the overlay menu. */
    var onMenu: () -> Unit = {}

    /** A key while an overlay shows ([suspended]), before its screens have it; true: it was used. */
    var onOverlayKey: (KeyEvent) -> Boolean = { false }

    /** The on-screen controls' custom buttons, let go when the view leaves its window. */
    var customButtons: CustomButtons? = null

    /** The on-screen virtual controller, let go with what is held ([releaseHeld]). */
    var virtualPad: VirtualPad? = null

    var controls: Controls = Controls()
        set(value) {
            field = value
            pad?.controls = value
        }

    var input: GameInput? = null
        set(value) {
            if (value === field) return
            field = value
            pad = value?.let { game ->
                PadInput(
                    pad = { game.gamepad.write(it) }, relay = ::send, onMenu = { onMenu() }, controls = controls,
                    pointerScale = screen.width / GameResolution.OWN_SIZE.first.toFloat(),
                    onMode = { onMouseMode(it) },
                )
            }
            // A new pad starts in pad mode.
            onMouseMode(pad?.mouseMode ?: false)
            touch = null
            // An overlay that took the focus before the relay came ([overlayFocus]): the relay hears it now.
            if (value != null && overlayAway && copy != null) send(listOf(RelayEvent.Focus(false)))
        }

    var suspended = false
        set(value) {
            if (value == field) return
            field = value
            if (value) {
                pad?.releaseAll()
                // Keys go to the native screens from now on, their releases too: X lets go now.
                releaseKeys()
                handler.removeCallbacks(longPress)
                touch?.cancel()
                send(mouse.release())
                // Android's keyboard types into the game: it closes too, unless the overlay keeps it.
                if (keyboard && !keepsKeyboard) showKeyboard(false)
            } else if (keyboard && keyboardShown()) {
                // Back from the menu with the keyboard up: it types into the game again.
                requestFocus()
                context.getSystemService(InputMethodManager::class.java).restartInput(this)
            }
            // After the releases above, which the game's window takes while it has the focus.
            overlayFocus()
        }

    /** The controller's mode changed: mouse mode on or off. */
    var onMouseMode: (Boolean) -> Unit = {}

    /** Mouse mode on or off, as the chord switches it ([PadInput.useMouseMode]). */
    fun useMouseMode(on: Boolean) {
        pad?.useMouseMode(on)
    }

    /** A device went: what the controller held is let go (Android sends no release for it). */
    fun releaseController() {
        pad?.releaseAll()
    }

    /** Lets go of the keyboard keys X holds down from here. */
    private fun releaseKeys() {
        if (keysDown.isEmpty()) return
        send(keysDown.map { RelayEvent.Key(it, false) })
        keysDown.clear()
    }

    override fun onWindowFocusChanged(hasWindowFocus: Boolean) {
        super.onWindowFocusChanged(hasWindowFocus)
        // The game runs on in the background: nothing stays held there (a running character).
        if (!hasWindowFocus) {
            pad?.releaseAll()
            releaseKeys()
            send(mouse.release())
        }
    }

    private val frame = object : Choreographer.FrameCallback {
        override fun doFrame(frameTimeNanos: Long) {
            // Choreographer may run a callback it took before the view left or hid its window: no new loop.
            if (windowVisibility != VISIBLE) {
                clockRunning = false
                return
            }
            val p = pad
            if (lastFrameNanos != 0L) p?.tick((frameTimeNanos - lastFrameNanos) / 1_000_000)
            lastFrameNanos = frameTimeNanos
            if (p?.ticking == true) {
                Choreographer.getInstance().postFrameCallback(this)
            } else {
                clockRunning = false
                lastFrameNanos = 0L
            }
        }
    }

    /**
     * The controller's clock runs while the pad has timed work ([PadInput.ticking]: a chord's
     * timing, the pointer in mouse mode) and the window shows; it starts with the controller's key
     * or stick input that gives it work (the on-screen controls give it none) and counts from that
     * moment.
     */
    private fun clock() {
        if (clockRunning || windowVisibility != VISIBLE || pad?.ticking != true) return
        clockRunning = true
        lastFrameNanos = System.nanoTime()
        Choreographer.getInstance().postFrameCallback(frame)
    }

    init {
        isFocusable = true
        isFocusableInTouchMode = true
        // Android draws a translucent white over a focused view once a key leaves touch mode (a
        // controller button, a typed letter); over the game's hole it washes out the whole picture.
        defaultFocusHighlightEnabled = false
        holder.addCallback(object : SurfaceHolder.Callback {
            override fun surfaceCreated(holder: SurfaceHolder) {
                copyShape = null
            }
            // Called after surfaceCreated and whenever the size changes. The copy starts again when
            // the display turned (the surface is often created before the game's landscape): the frames
            // are stored turned for the rotation. A new size alone (Display, PD2's menu and game) keeps
            // the copy and the game's layer, which take only the new geometry.
            override fun surfaceChanged(holder: SurfaceHolder, format: Int, width: Int, height: Int) {
                val shape = listOf(display?.rotation ?: Surface.ROTATION_0, width, height)
                if (shape == copyShape) return
                // Only a new size, the display not turned (Display, PD2's menus): both layers show at it
                // in place, with no new copy or layer, so no older X screen shows meanwhile.
                val running = copy
                if (running != null && copyShape?.get(0) == shape[0]) {
                    copyShape = shape
                    running.resize(width, height)
                    PresentServer.current.value?.resize(holder.surface, width, height)
                    return
                }
                val returning = copy == null
                copy?.stop()
                copyShape = shape
                // The first surface starts a measurement of the display's rate; a return goes on with what it knows.
                screenRate.start()
                val (rate, capped) = clockAndVote()
                // Back on the screen: the game's window has the focus again, also after the
                // setting was turned off meanwhile (x-input-relay does nothing when it did not take
                // the focus away); not while an overlay shows that keeps it away ([overlayFocus]).
                if (returning) send(listOf(RelayEvent.Focus(!overlayAway)))
                // The game's own buffers, when it presents them here, on a layer above the copy's.
                // On a return first: a frame the game drew while away shows at once, so the copy,
                // which rests under the layer, never posts the X screen's older picture. On a new
                // shape the copy first: it shows the game while the old layer's frames come back.
                val server = PresentServer.current.value
                if (returning) server?.attach(holder.surface, rate, capped)
                copy = FrameCopy.start(holder.surface, framebuffer, screen, shape[0], presentCounter, rate)
                if (!returning) server?.attach(holder.surface, rate, capped)
            }
            override fun surfaceDestroyed(holder: SurfaceHolder) {
                leaveScreen()
                screenRate.stop()
                PresentServer.current.value?.detach(holder.surface)
                copy?.stop()
                copy = null
            }
        })
    }

    override fun onAttachedToWindow() {
        super.onAttachedToWindow()
        val window = window ?: return
        val base = window.callback
        val wrapper = object : Window.Callback by base {
            override fun dispatchKeyEvent(event: KeyEvent): Boolean =
                onGameKey(event) || (suspended && onOverlayKey(event)) || base.dispatchKeyEvent(event)
            override fun dispatchGenericMotionEvent(event: MotionEvent): Boolean =
                onGameMotion(event) || base.dispatchGenericMotionEvent(event)
        }
        window.callback = wrapper
        callback = base
        if (sends.isShutdown) sends = Executors.newSingleThreadExecutor { Thread(it, "pd2-input") }
    }

    // The controller's clock ([frame]) runs only while the window shows (clock): in the background
    // it would wake the main thread on every display frame for nothing (the controller is let go
    // when the window loses focus).
    override fun onWindowVisibilityChanged(visibility: Int) {
        super.onWindowVisibilityChanged(visibility)
        Choreographer.getInstance().removeFrameCallback(frame)
        clockRunning = false
        lastFrameNanos = 0L
        if (visibility == VISIBLE) clock()
    }

    /**
     * The game is no longer on the screen: what is held goes up first, while the game's window
     * still has the focus that its keys need, then the window loses the focus ([tellBackground]).
     */
    private fun leaveScreen() {
        if (copy == null || !tellBackground) return
        releaseHeld()
        send(listOf(RelayEvent.Focus(false)))
    }

    /**
     * What is held goes up (the mouse, the controller, keys, the on-screen custom buttons and
     * virtual controller), before the game's window loses the focus that its keys need.
     */
    private fun releaseHeld() {
        send(mouse.release())
        pad?.releaseAll()
        releaseKeys()
        customButtons?.releaseAll()
        virtualPad?.releaseAll()
    }

    override fun onDetachedFromWindow() {
        Choreographer.getInstance().removeFrameCallback(frame)
        clockRunning = false
        callback?.let { window?.callback = it }
        callback = null
        handler.removeCallbacksAndMessages(null)
        touch?.cancel()
        send(mouse.release())
        pad?.releaseAll()
        releaseKeys()
        // The on-screen custom buttons' keys go through [sends] too: released while it still takes them.
        customButtons?.releaseAll()
        // Before the sends stop: the surface goes after this.
        leaveScreen()
        sends.shutdown()
        super.onDetachedFromWindow()
    }

    /** Android's keyboard opens for the game ([on]), or closes and the view is no editor again. */
    fun showKeyboard(on: Boolean) {
        val manager = context.getSystemService(InputMethodManager::class.java)
        keyboard = on
        if (on) {
            requestFocus()
            manager.restartInput(this)
            manager.showSoftInput(this, 0)
        } else {
            manager.hideSoftInputFromWindow(windowToken, 0)
            manager.restartInput(this)
        }
    }

    private fun keyboardShown() = ViewCompat.getRootWindowInsets(this)?.isVisible(WindowInsetsCompat.Type.ime()) == true

    /** Controller keys go to the pad (always consumed), keyboard keys to X as keysyms. */
    private fun onGameKey(event: KeyEvent): Boolean {
        if (suspended) return false
        // Android's keyboard closed by its own Back: no editor again, or it reopens on every key.
        if (keyboard && !keyboardShown()) {
            keyboard = false
            context.getSystemService(InputMethodManager::class.java).restartInput(this)
        }
        val code = event.keyCode
        if (code == KeyEvent.KEYCODE_VOLUME_UP || code == KeyEvent.KEYCODE_VOLUME_DOWN || code == KeyEvent.KEYCODE_VOLUME_MUTE) {
            return false
        }
        val down = event.action == KeyEvent.ACTION_DOWN
        if (KeyEvent.isGamepadButton(code) || event.isFromSource(InputDevice.SOURCE_GAMEPAD) ||
            event.isFromSource(InputDevice.SOURCE_JOYSTICK)
        ) {
            // A cancelled key (focus lost, the device reset) was not let go by the player: it
            // must neither act as a release (a cancelled Select + Start would switch mouse
            // mode) nor leave anything held.
            if (event.isCanceled) {
                pad?.releaseAll()
                return true
            }
            if (event.repeatCount == 0) {
                pad?.onKey(code, down)
                clock()
            }
            return true
        }
        val sym = Keys.keysym(code) ?: return false
        if (event.repeatCount == 0) {
            if (down) keysDown += sym else keysDown -= sym
            send(listOf(RelayEvent.Key(sym, down)))
        }
        return true
    }

    /** A controller's sticks, triggers and hat. */
    private fun onGameMotion(event: MotionEvent): Boolean {
        if (suspended || !event.isFromSource(InputDevice.SOURCE_JOYSTICK)) return false
        if (event.action == MotionEvent.ACTION_CANCEL) {
            // Android ends the motion (focus lost, the device reset or removed): the sticks,
            // triggers and hat rest, or the game would keep the last values.
            pad?.onAxes(PadAxes())
            return true
        }
        if (event.action != MotionEvent.ACTION_MOVE) return false
        pad?.onAxes(
            PadAxes(
                lx = event.getAxisValue(MotionEvent.AXIS_X),
                ly = event.getAxisValue(MotionEvent.AXIS_Y),
                rx = event.getAxisValue(MotionEvent.AXIS_Z),
                ry = event.getAxisValue(MotionEvent.AXIS_RZ),
                leftTrigger = maxOf(event.getAxisValue(MotionEvent.AXIS_LTRIGGER), event.getAxisValue(MotionEvent.AXIS_BRAKE)),
                rightTrigger = maxOf(event.getAxisValue(MotionEvent.AXIS_RTRIGGER), event.getAxisValue(MotionEvent.AXIS_GAS)),
                hatX = event.getAxisValue(MotionEvent.AXIS_HAT_X),
                hatY = event.getAxisValue(MotionEvent.AXIS_HAT_Y),
            ),
        )
        clock()
        return true
    }

    /** Touch, mapped from this view (the whole X screen, as Display places it) to X screen pixels; a mouse's events go to [onMouse]. */
    override fun onTouchEvent(event: MotionEvent): Boolean {
        if (event.isFromSource(InputDevice.SOURCE_MOUSE)) return onMouse(event)
        // Compose's own cancel (another device's press, a device gone) has no source: the mouse lets go too.
        if (event.actionMasked == MotionEvent.ACTION_CANCEL) send(mouse.release())
        if (suspended) return true
        // A finger may wander this far, in X screen pixels, and still tap.
        val slop = ViewConfiguration.get(context).scaledTouchSlop.toFloat() * screen.width / width.coerceAtLeast(1)
        val gestures = touch ?: input?.let {
            TouchGestures(
                slop, ::send,
                later = { delayMs, click -> handler.postDelayed({ click() }, pendingClick, delayMs) },
                dropLater = { handler.removeCallbacksAndMessages(pendingClick) },
            )
        }?.also { touch = it } ?: return true
        val time = event.eventTime
        when (event.actionMasked) {
            MotionEvent.ACTION_DOWN -> {
                touchId = event.getPointerId(0)
                handler.removeCallbacks(longPress)
                gestures.down(screenX(event.x), screenY(event.y), time)
                handler.postDelayed(longPress, TouchGestures.LONG_PRESS_MS)
            }
            MotionEvent.ACTION_POINTER_DOWN -> gestures.secondDown(time)
            // The followed finger lifts while another stays: its gesture ends (the other
            // finger's moves are not followed).
            MotionEvent.ACTION_POINTER_UP -> if (event.getPointerId(event.actionIndex) == touchId) {
                handler.removeCallbacks(longPress)
                gestures.primaryUp(time)
            }
            MotionEvent.ACTION_MOVE -> {
                val index = event.findPointerIndex(touchId)
                if (index >= 0) gestures.move(screenX(event.getX(index)), screenY(event.getY(index)), time)
            }
            MotionEvent.ACTION_UP -> {
                handler.removeCallbacks(longPress)
                gestures.up(time)
            }
            MotionEvent.ACTION_CANCEL -> {
                handler.removeCallbacks(longPress)
                gestures.cancel()
            }
        }
        return true
    }

    /** A mouse's moves with no button held and its wheel. */
    override fun onGenericMotionEvent(event: MotionEvent): Boolean = when (event.actionMasked) {
        MotionEvent.ACTION_HOVER_ENTER, MotionEvent.ACTION_HOVER_MOVE, MotionEvent.ACTION_HOVER_EXIT, MotionEvent.ACTION_SCROLL ->
            event.isFromSource(InputDevice.SOURCE_MOUSE) && onMouse(event)
        else -> super.onGenericMotionEvent(event)
    }

    /**
     * A mouse, 1:1 into X ([Mouse]) and never through the touch gestures: a click is a click at
     * once. Its buttons come with the moves (Android's button state), so `ACTION_BUTTON_PRESS`,
     * which Android sends here even under an overlay, is not used.
     */
    private fun onMouse(event: MotionEvent): Boolean {
        mouseOver = event.actionMasked != MotionEvent.ACTION_HOVER_EXIT
        if (suspended) return true
        send(
            when (event.actionMasked) {
                MotionEvent.ACTION_CANCEL -> mouse.release()
                // Out of the view into the black around it: the pointer stops at the X screen's edge, however fast.
                MotionEvent.ACTION_HOVER_EXIT -> listOf(RelayEvent.Motion(screenX(event.x), screenY(event.y)))
                // The wheel only from a scroll: Compose copies a scroll's axes into a hover it makes up.
                MotionEvent.ACTION_SCROLL ->
                    mouse.events(screenX(event.x), screenY(event.y), event.buttonState, event.getAxisValue(MotionEvent.AXIS_VSCROLL))
                else -> mouse.events(screenX(event.x), screenY(event.y), event.buttonState)
            },
        )
        return true
    }

    // Over the game only the game's own cursor shows; Android's arrow shows over an overlay.
    override fun onResolvePointerIcon(event: MotionEvent, pointerIndex: Int): PointerIcon? =
        if (!suspended && mouseOver && event.isFromSource(InputDevice.SOURCE_MOUSE)) {
            PointerIcon.getSystemIcon(context, PointerIcon.TYPE_NULL)
        } else {
            super.onResolvePointerIcon(event, pointerIndex)
        }

    // Only while the player asked for Android's keyboard; a tap on the game never opens it.
    override fun onCheckIsTextEditor(): Boolean = keyboard

    /** Android's keyboard: committed text is typed into X, deletions are BackSpace and Delete. */
    override fun onCreateInputConnection(outAttrs: EditorInfo): InputConnection? {
        // No editor otherwise: with one, Android's keyboard pops up for every hardware key.
        if (!keyboard) return null
        outAttrs.inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_VARIATION_VISIBLE_PASSWORD or
            InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS
        outAttrs.imeOptions = EditorInfo.IME_FLAG_NO_EXTRACT_UI or EditorInfo.IME_FLAG_NO_FULLSCREEN or EditorInfo.IME_ACTION_NONE
        return object : BaseInputConnection(this, false) {
            override fun commitText(text: CharSequence, newCursorPosition: Int): Boolean {
                send(Keys.typeText(text))
                return true
            }

            override fun deleteSurroundingText(beforeLength: Int, afterLength: Int): Boolean {
                send(Keys.deleteText(beforeLength, afterLength))
                return true
            }

            override fun performEditorAction(editorAction: Int): Boolean {
                send(tap(Keys.XK_RETURN))
                return true
            }
        }
    }

    private fun tap(sym: Int) = listOf(RelayEvent.Key(sym, true), RelayEvent.Key(sym, false))

    /** The on-screen virtual controller, merged into the game's pad. */
    fun virtual(state: PadState) {
        pad?.onVirtual(state)
    }

    /** Events into X through the relay, in order, off the main thread. */
    fun send(events: List<RelayEvent>) {
        val relay = input?.relay ?: return
        if (events.isEmpty() || sends.isShutdown) return
        sends.execute {
            try {
                relay.send(events)
            } catch (_: IOException) {
                // The relay is gone with the game environment.
            }
        }
    }

    private fun screenX(x: Float) = (x * screen.width / width.coerceAtLeast(1)).toInt().coerceIn(0, screen.width - 1)

    private fun screenY(y: Float) = (y * screen.height / height.coerceAtLeast(1)).toInt().coerceIn(0, screen.height - 1)
}

private fun Context.activity(): Activity? = when (this) {
    is Activity -> this
    is ContextWrapper -> baseContext.activity()
    else -> null
}
