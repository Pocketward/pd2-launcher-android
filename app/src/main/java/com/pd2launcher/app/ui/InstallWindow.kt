package com.pd2launcher.app.ui

import android.graphics.Bitmap
import androidx.compose.foundation.Image
import androidx.compose.foundation.LocalIndication
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.interaction.collectIsFocusedAsState
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.requiredSize
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.rotate
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import androidx.compose.ui.window.SecureFlagPolicy
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.lifecycle.repeatOnLifecycle
import com.pd2launcher.app.Framebuffer
import com.pd2launcher.app.R
import com.pd2launcher.app.ScreenPixels
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.withContext

/** A part of the X screen, in its pixels: [right] and [bottom] exclusive. */
data class ScreenArea(val left: Int, val top: Int, val right: Int, val bottom: Int) {
    val width: Int get() = right - left
    val height: Int get() = bottom - top
}

/**
 * Where the installer windows are on the X screen: the box around every pixel that is not the
 * black root, looked at every [STEP]th pixel, with a margin; null while nothing is drawn, or
 * nothing larger than [MIN_DRAWN_WIDTH] x [MIN_DRAWN_HEIGHT] (Xvfb draws the mouse cursor into
 * the screen, which alone would fill the thumbnail). The windows sit small on a large screen, so
 * the install screen shows only this part.
 */
fun drawnArea(width: Int, height: Int, pixel: (Int, Int) -> Int): ScreenArea? {
    var left = width
    var top = height
    var right = -1
    var bottom = -1
    for (y in 0 until height step STEP) {
        for (x in 0 until width step STEP) {
            if (pixel(x, y) and 0xFFFFFF == 0) continue
            if (x < left) left = x
            if (x > right) right = x
            if (y < top) top = y
            if (y > bottom) bottom = y
        }
    }
    if (right < 0 || right + STEP - left < MIN_DRAWN_WIDTH || bottom + STEP - top < MIN_DRAWN_HEIGHT) return null
    return ScreenArea(
        (left - MARGIN).coerceAtLeast(0), (top - MARGIN).coerceAtLeast(0),
        (right + STEP + MARGIN).coerceAtMost(width), (bottom + STEP + MARGIN).coerceAtMost(height),
    )
}

private const val STEP = 8
private const val MARGIN = 16

/** The least a window takes on the X screen, in its pixels: the cursor (16 to 32) is smaller. */
const val MIN_DRAWN_WIDTH = 64
const val MIN_DRAWN_HEIGHT = 48

/** The widest picture of the windows read from the screen; a larger area is sampled down to it. */
private const val PICTURE_WIDTH = 1200

/** How often the window's picture is read again. */
private const val WINDOW_MS = 500L

/** What [WindowReader.read] found on the X screen. */
sealed interface WindowFrame {
    /** Nothing drawn yet, or no framebuffer to read (not written yet, being replaced, short). */
    data object Blank : WindowFrame

    /** The same picture as the last read: the drawn area and its sampled pixels are unchanged. */
    data object Unchanged : WindowFrame

    /** A new picture of the drawn area: [width] x [height] opaque ARGB [colors]. */
    class Picture(val width: Int, val height: Int, val colors: IntArray) : WindowFrame
}

/**
 * Reads the installer windows' picture from the X screen's framebuffer ([map]: its pixels, or
 * null while Xvfb has not written them) for [InstallWindow]. The pixels are mapped once they are
 * there and then read in place (a game environment started again brings a new framebuffer, and
 * so a new reader); until then every read tries again. A read whose drawn area and pixel checksum
 * match the last one's builds nothing.
 */
class WindowReader(private val map: () -> ScreenPixels?) {
    constructor(framebuffer: Framebuffer) : this(framebuffer::map)

    private var screen: ScreenPixels? = null
    private var sample = IntArray(0)
    private var lastArea: ScreenArea? = null
    private var lastSum = 0L

    fun read(): WindowFrame {
        val screen = screen ?: map()?.also { screen = it } ?: return nothing()
        val header = screen.header
        // BGRX as a little-endian word: 0xXXRRGGBB.
        fun pixel(x: Int, y: Int) = screen.pixel(x, y)
        val area = drawnArea(header.width, header.height, ::pixel) ?: return nothing()
        val step = (area.width + PICTURE_WIDTH - 1) / PICTURE_WIDTH
        val w = area.width / step
        val h = area.height / step
        if (w <= 0 || h <= 0) return nothing()
        if (sample.size != w * h) sample = IntArray(w * h)
        var sum = 0L
        for (y in 0 until h) {
            for (x in 0 until w) {
                val color = pixel(area.left + x * step, area.top + y * step) or OPAQUE
                sample[y * w + x] = color
                sum = sum * 31 + color
            }
        }
        if (area == lastArea && sum == lastSum) return WindowFrame.Unchanged
        lastArea = area
        lastSum = sum
        return WindowFrame.Picture(w, h, sample.copyOf())
    }

    /** Nothing to show: the next picture is built whatever it holds. */
    private fun nothing(): WindowFrame {
        lastArea = null
        return WindowFrame.Blank
    }
}

/**
 * The small, read-only window onto the X screen during Install: the installer windows as they
 * work, cropped to where they are, read from the X screen's [framebuffer] twice a second while
 * the app is on screen (started), as a thumbnail beside the figure's percentage. Nothing shows
 * until something is drawn. A tap (or A) enlarges it (turned to fill a portrait screen); a tap
 * closes it again. It takes no input. Blizzard's installer may show a CD key in its windows, so
 * the screen stays out of screenshots while this is there ([KeepSecure]).
 */
@Composable
fun InstallWindow(framebuffer: Framebuffer) {
    KeepSecure()
    var picture by remember { mutableStateOf<ImageBitmap?>(null) }
    var enlarged by remember { mutableStateOf(false) }
    val interaction = remember { MutableInteractionSource() }
    val focused by interaction.collectIsFocusedAsState()
    val lifecycle = LocalLifecycleOwner.current.lifecycle
    LaunchedEffect(framebuffer, lifecycle) {
        picture = null
        val reader = WindowReader(framebuffer)
        lifecycle.repeatOnLifecycle(Lifecycle.State.STARTED) {
            while (isActive) {
                when (val frame = withContext(Dispatchers.IO) { reader.read() }) {
                    WindowFrame.Unchanged -> Unit
                    WindowFrame.Blank -> picture = null
                    is WindowFrame.Picture -> picture = withContext(Dispatchers.Default) {
                        Bitmap.createBitmap(frame.colors, frame.width, frame.height, Bitmap.Config.ARGB_8888).asImageBitmap()
                    }
                }
                delay(WINDOW_MS)
            }
        }
    }
    val shown = picture ?: return
    val name = stringResource(R.string.install_window)
    val hint = stringResource(R.string.install_window_hint)
    val shape = RoundedCornerShape(6.dp)
    Box(
        Modifier.padding(start = 12.dp).size(THUMB_WIDTH, THUMB_HEIGHT).clip(shape).background(Color.Black)
            .border(if (focused) 2.dp else 1.dp, if (focused) Palette.Teal else Palette.Rule, shape)
            .clickable(interaction, LocalIndication.current, onClickLabel = hint) { enlarged = true }
            .semantics { contentDescription = name },
    ) {
        Image(shown, null, Modifier.fillMaxSize(), contentScale = ContentScale.Fit)
        Text(
            hint.uppercase(), style = Type.Label, color = Palette.Text, maxLines = 1, overflow = TextOverflow.Ellipsis,
            modifier = Modifier.align(Alignment.BottomStart).fillMaxWidth()
                .background(Brush.verticalGradient(listOf(Color.Transparent, Color.Black.copy(alpha = 0.85f))))
                .padding(start = 6.dp, end = 6.dp, top = 12.dp, bottom = 4.dp),
        )
    }
    if (enlarged) {
        Dialog({ enlarged = false }, DialogProperties(usePlatformDefaultWidth = false, securePolicy = SecureFlagPolicy.SecureOn)) {
            BoxWithConstraints(Modifier.fillMaxSize().background(Color.Black).clickable { enlarged = false }, contentAlignment = Alignment.Center) {
                // A wide picture on a portrait screen is turned a quarter to use its length.
                val turn = maxHeight > maxWidth && shown.width > shown.height
                val image = if (turn) Modifier.requiredSize(maxHeight, maxWidth).rotate(90f) else Modifier.fillMaxSize()
                Image(shown, stringResource(R.string.install_window), image.padding(8.dp), contentScale = ContentScale.Fit)
                Text(
                    stringResource(R.string.install_window_close), style = Type.Hint, color = Palette.Muted,
                    modifier = Modifier.align(Alignment.BottomCenter).padding(16.dp),
                )
            }
        }
    }
}

private const val OPAQUE = 0xFF000000.toInt()

/** The thumbnail beside the figure: small enough for the five tasks to fit under it on a phone. */
private val THUMB_WIDTH = 132.dp
private val THUMB_HEIGHT = 99.dp
