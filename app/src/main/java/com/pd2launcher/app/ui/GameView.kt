package com.pd2launcher.app.ui

import android.app.Activity
import android.content.Context
import android.content.ContextWrapper
import android.hardware.input.InputManager
import android.os.Handler
import android.os.Looper
import android.os.SystemClock
import android.text.InputType
import android.view.Choreographer
import android.view.InputDevice
import android.view.KeyEvent
import android.view.MotionEvent
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
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.isImeVisible
import androidx.compose.foundation.layout.safeDrawing
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.viewinterop.AndroidView
import androidx.core.view.ViewCompat
import androidx.core.view.WindowInsetsCompat
import com.pd2launcher.app.AppSettingsStore
import com.pd2launcher.app.FpsReading
import com.pd2launcher.app.FrameCopy
import com.pd2launcher.app.FrameCounts
import com.pd2launcher.app.FrameRateVote
import com.pd2launcher.app.Framebuffer
import com.pd2launcher.app.GameEnvironment
import com.pd2launcher.app.GameFrameRate
import com.pd2launcher.app.GameInput
import com.pd2launcher.app.PresentServer
import com.pd2launcher.app.R
import com.pd2launcher.app.SessionService
import com.pd2launcher.app.XwdHeader
import com.pd2launcher.app.input.Controls
import com.pd2launcher.app.input.ControlsStore
import com.pd2launcher.app.input.Keys
import com.pd2launcher.app.input.CustomButtons
import com.pd2launcher.app.input.GamePicture
import com.pd2launcher.app.input.PictureDebounce
import com.pd2launcher.app.input.isController
import com.pd2launcher.app.input.LayoutStore
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

/** What shows over the game: nothing, the menu rail, the Controls panel, or the on-screen controls editor. */
enum class GameLayer {
    NONE,
    MENU,
    CONTROLS,
    EDIT,
    ;

    /**
     * The on-screen controls show and play (when [Controls.onScreenShown] says so): over the game
     * and beside the menu rail, which draws over the pieces it overlaps and takes their touches,
     * so its Touch pad switch shows at once. The rail [asking] whether to stop, the Controls panel
     * and the editor (which has its own sample) hide them.
     */
    fun padShown(asking: Boolean = false): Boolean = this == NONE || (this == MENU && !asking)

    /**
     * The game lets go of the controller and of touch: the controller drives the native screens
     * and the on-screen controls are the only way into the game (beside the rail).
     */
    val suspends: Boolean get() = this != NONE

    /** A tap on the game picture, outside the rail and the on-screen controls, closes the rail and never reaches the game. */
    val tapCloses: Boolean get() = this == MENU

    /** The fps counter shows, when [showFps] is on, over the game PD2 draws (never over the official [launcher]) and beside the rail, so its switch shows at once. */
    fun fpsShown(showFps: Boolean, launcher: Boolean): Boolean = showFps && !launcher && (this == NONE || this == MENU)
}

/**
 * The Game view: the X screen ([framebuffer]) copied into a SurfaceView every display frame,
 * letterboxed to the X screen's shape, or the game's own buffers on a layer above it while it
 * presents them there ([PresentServer]), the display asked for its full rate or the player's
 * reduced frame rate, which also holds the game to it ([AppSettings.frameRate]), and the player's
 * input into the game: touch as the X screen's
 * mouse, keyboard keys and Android's keyboard as X keys, the controller as the game's XInput pad
 * (the chord pressed briefly: mouse mode), and the on-screen controls as game keys. The menu rail
 * opens from the glyph, the chord held, or Back; the game keeps running and showing beside it, the on-screen controls stay shown and playable
 * where the rail does not cover them ([GameLayer.padShown]), the controller lets go of the game
 * and drives the rail, and a tap elsewhere on the game closes it. Android's keyboard never pushes
 * the game up; it closes as the rail or another overlay opens, so they have the whole height. The
 * screen stays on as Settings → Keep the screen on says (MainActivity sets the window's flag; a
 * view of its own would keep it on whatever the setting).
 *
 * [presentCounter]: the game's present counter, for the fps counter ([SessionPaths.presentCounter]).
 * [onSettings] opens the app's settings at a [SettingsNav] tab (null: the menu offers none).
 * [preview] (debug builds): the overlay over an empty screen, as if the game ran, at that layer;
 * [previewLauncher]: as if the official PD2 launcher ran.
 */
@OptIn(ExperimentalLayoutApi::class)
@Composable
fun GameView(
    framebuffer: Framebuffer?,
    running: Boolean,
    presentCounter: File? = null,
    onSettings: ((tab: Int) -> Unit)? = null,
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
    // The rail asks whether to stop; any change of layer forgets the question.
    var asking by remember { mutableStateOf(false) }
    LaunchedEffect(layer) { asking = false }
    val controller = controllerConnected(onRemoved = { surface?.releaseController() })
    // The launcher's own window; a game it started is the game's ([Play.officialLauncherShown]).
    val launcher = previewLauncher || Play.officialLauncherShown(SessionService.status.collectAsState().value)
    // The launcher is a mouse/touch window: no menu/world picture to read.
    val picture = gamePicture(framebuffer, if (running && !launcher) header else null)
    // Android's keyboard as it is, closed by its own controls too: here it opens only for the game.
    val keyboard = WindowInsets.isImeVisible
    val buttons = remember { CustomButtons { events -> surface?.send(events) } }
    val virtualPad = remember { VirtualPad { state -> surface?.virtual(state) } }
    val live = running || preview != null
    val app by AppSettingsStore.current.collectAsState()
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
        layer = when (layer) {
            GameLayer.NONE -> GameLayer.MENU
            GameLayer.MENU -> GameLayer.NONE
            GameLayer.CONTROLS -> GameLayer.MENU
            GameLayer.EDIT -> GameLayer.NONE
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
                AndroidView(
                    modifier = Modifier.aspectRatio(screen.width.toFloat() / screen.height),
                    factory = { context ->
                        GameSurface(context, framebuffer, presentCounter, screen, app.frameRate).also { surface = it }
                    },
                    update = {
                        it.onMenu = { layer = GameLayer.MENU }
                        it.controls = controls
                        it.input = input
                        it.suspended = layer.suspends
                        it.customButtons = buttons
                    },
                )
            }
        }
        if (live) {
            val onScreen = controls.onScreenOn(controller)
            // Under the on-screen controls and the rail, over the game: what they leave of the screen.
            if (layer.tapCloses) TapToClose { layer = GameLayer.NONE }
            if (layer.padShown(asking) && controls.onScreenShown(controller, picture, launcher)) {
                OnScreenControls(layout, controls.onScreenOpacity, virtualPad, buttons)
            }
            if (layer == GameLayer.NONE) MenuGlyph(Modifier.align(Alignment.TopEnd).windowInsetsPadding(WindowInsets.safeDrawing)) { layer = GameLayer.MENU }
            // Top centre, clear of the default controls and of the rail; it takes no touch.
            if (layer.fpsShown(app.showFps, launcher)) FpsCounter(fps, Modifier.align(Alignment.TopCenter).windowInsetsPadding(WindowInsets.safeDrawing))
            if (layer == GameLayer.EDIT) ControlsEditor(sample = preview != null, onClose = { layer = GameLayer.NONE })
            GameRail(
                visible = layer == GameLayer.MENU,
                asking = asking,
                onAsking = { asking = it },
                launcher = launcher,
                keyboard = keyboard,
                touchPad = onScreen,
                showFps = app.showFps,
                onClose = { layer = GameLayer.NONE },
                onKeyboard = { on ->
                    // Typing needs the game: the rail closes as the keyboard opens.
                    if (on) layer = GameLayer.NONE
                    surface?.showKeyboard(on)
                },
                onTouchPad = { ControlsStore.save(context, controls.withTouchPad(it, controller)) },
                onShowFps = { AppSettingsStore.save(context, app.copy(showFps = it)) },
                onControls = { layer = GameLayer.CONTROLS },
                onEditControls = { layer = GameLayer.EDIT },
                onSettings = onSettings?.let { open -> { layer = GameLayer.NONE; open(SettingsNav.GAME) } },
                onStop = {
                    layer = GameLayer.NONE
                    SessionService.send(context, SessionService.ACTION_GAME_STOP)
                },
            )
            ControlsPanel(
                visible = layer == GameLayer.CONTROLS,
                controls = controls,
                save = { ControlsStore.save(context, it) },
                onBack = { layer = GameLayer.MENU },
                onChangeButtons = onSettings?.let { open -> { layer = GameLayer.NONE; open(SettingsNav.CONTROLLER) } },
                onEditOnScreen = { layer = GameLayer.EDIT },
            )
        }
    }
}

/**
 * A controller (a gamepad or joystick device, built in or attached) is there, followed live: a
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
 * What PD2 shows ([GamePicture]), read from the X screen's frames every [PICTURE_MS] while a
 * [screen] is known and steadied ([PictureDebounce]); unknown otherwise. A few sampled pixels
 * of the framebuffer, mapped once, off the main thread. Only the X screen: while the game
 * presents its own buffers the X screen misses their frames, but [Controls.onScreenShown] does
 * not use the picture, so the game's buffer ([PresentServer.readShown]) is not locked for it
 * four times a second.
 */
@Composable
private fun gamePicture(framebuffer: Framebuffer?, screen: XwdHeader?): GamePicture {
    var picture by remember { mutableStateOf(GamePicture.UNKNOWN) }
    LaunchedEffect(framebuffer, screen) {
        picture = GamePicture.UNKNOWN
        if (framebuffer == null || screen == null || screen.bitsPerPixel != 32) return@LaunchedEffect
        withContext(Dispatchers.IO) {
            val pixels = framebuffer.map()?.takeIf { it.header == screen } ?: return@withContext
            val steady = PictureDebounce()
            while (isActive) {
                val next = steady.sample(GamePicture.of(screen.width, screen.height, pixels::pixel))
                if (next != picture) withContext(Dispatchers.Main) { picture = next }
                delay(PICTURE_MS)
            }
        }
    }
    return picture
}

/** How often the Game view looks at the shape of PD2's picture. */
private const val PICTURE_MS = 250L

/**
 * The X screen and the input into it. While attached it sees every key and controller motion of
 * the window first (it wraps the window's callback, so the activity's own mapping of A and B
 * never applies here), and it is the editor Android's keyboard types into. While [suspended]
 * (an overlay layer shows, [GameLayer.suspends]) it lets go of the controller and touch and
 * passes keys on, so the controller drives the native screens; the on-screen controls still
 * reach the game ([virtual], [send]). [frameRate]: the player's reduced frame rate, which the
 * display is asked for and the game held to where the screen is faster; null: the screen's full
 * rate ([FrameRateVote]).
 */
private class GameSurface(
    context: Context,
    framebuffer: Framebuffer,
    presentCounter: File?,
    private val screen: XwdHeader,
    frameRate: Int?,
) : SurfaceView(context) {
    private val window: Window? = context.activity()?.window
    private var callback: Window.Callback? = null
    private var sends = Executors.newSingleThreadExecutor { Thread(it, "pd2-input") }
    private val handler = Handler(Looper.getMainLooper())
    /** The token of a tap's click still to come on [handler] ([TouchGestures.later]). */
    private val pendingClick = Any()
    private var pad: PadInput? = null
    private var touch: TouchGestures? = null
    private var touchId = -1
    /** The keyboard keys sent to X as pressed and not yet released (keysyms). */
    private val keysDown = HashSet<Int>()
    private var lastFrameNanos = 0L
    private var keyboard = false
    private val longPress = Runnable { touch?.longPressDue(SystemClock.uptimeMillis()) }
    private var copy: FrameCopy? = null
    private var copyShape: List<Int>? = null

    /** The copy's counts so far ([FrameCopy.counts]); null while not copying. Main thread. */
    fun counts(): FrameCounts? = copy?.counts()

    /** The chord was held: open the overlay menu. */
    var onMenu: () -> Unit = {}

    /** The on-screen controls' custom buttons, let go when the view leaves its window. */
    var customButtons: CustomButtons? = null

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
                PadInput(pad = { game.gamepad.write(it) }, relay = ::send, onMenu = { onMenu() }, controls = controls)
            }
            touch = null
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
                // Android's keyboard types into the game, so it closes too, and the overlay gets the
                // whole height.
                if (keyboard) showKeyboard(false)
            }
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
        }
    }

    private val frame = object : Choreographer.FrameCallback {
        override fun doFrame(frameTimeNanos: Long) {
            // Choreographer may run a callback it took before the view left or hid its window: no new loop.
            if (windowVisibility != VISIBLE) return
            if (lastFrameNanos != 0L) pad?.tick((frameTimeNanos - lastFrameNanos) / 1_000_000)
            lastFrameNanos = frameTimeNanos
            Choreographer.getInstance().postFrameCallback(this)
        }
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
            // the display turned (the surface is often created before the game's landscape) or the
            // size changed: the frames are stored turned for the rotation and scaled to the size.
            override fun surfaceChanged(holder: SurfaceHolder, format: Int, width: Int, height: Int) {
                val shape = listOf(display?.rotation ?: Surface.ROTATION_0, width, height)
                if (shape == copyShape) return
                val returning = copy == null
                copy?.stop()
                copyShape = shape
                val highest = FrameRateVote.highest(display)
                val rate = FrameRateVote.vote(highest, frameRate)
                val interval = FrameRateVote.intervalNanos(FrameRateVote.cap(highest, frameRate))
                // The game's own buffers, when it presents them here, on a layer above the copy's.
                // On a return first: a frame the game drew while away shows at once, so the copy,
                // which rests under the layer, never posts the X screen's older picture. On a new
                // shape the copy first: it shows the game while the old layer's frames come back.
                val server = PresentServer.current.value
                if (returning) server?.attach(holder.surface, rate, interval)
                copy = FrameCopy.start(holder.surface, framebuffer, screen, shape[0], presentCounter, rate, interval)
                if (!returning) server?.attach(holder.surface, rate, interval)
            }
            override fun surfaceDestroyed(holder: SurfaceHolder) {
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
            override fun dispatchKeyEvent(event: KeyEvent): Boolean = onGameKey(event) || base.dispatchKeyEvent(event)
            override fun dispatchGenericMotionEvent(event: MotionEvent): Boolean =
                onGameMotion(event) || base.dispatchGenericMotionEvent(event)
        }
        window.callback = wrapper
        callback = base
        if (sends.isShutdown) sends = Executors.newSingleThreadExecutor { Thread(it, "pd2-input") }
    }

    // The controller's clock ([frame]) runs only while the window shows: in the background it
    // would wake the main thread on every display frame for nothing (the controller is let go
    // when the window loses focus).
    override fun onWindowVisibilityChanged(visibility: Int) {
        super.onWindowVisibilityChanged(visibility)
        Choreographer.getInstance().removeFrameCallback(frame)
        if (visibility == VISIBLE) {
            lastFrameNanos = 0L
            Choreographer.getInstance().postFrameCallback(frame)
        }
    }

    override fun onDetachedFromWindow() {
        Choreographer.getInstance().removeFrameCallback(frame)
        callback?.let { window?.callback = it }
        callback = null
        handler.removeCallbacksAndMessages(null)
        touch?.cancel()
        pad?.releaseAll()
        releaseKeys()
        // The on-screen custom buttons' keys go through [sends] too: released while it still takes them.
        customButtons?.releaseAll()
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
            if (event.repeatCount == 0) pad?.onKey(code, down)
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
        return true
    }

    /** Touch, mapped from this view (the letterboxed X screen) to X screen pixels. */
    override fun onTouchEvent(event: MotionEvent): Boolean {
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
