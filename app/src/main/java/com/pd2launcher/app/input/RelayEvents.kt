package com.pd2launcher.app.input

import com.pd2launcher.app.GameResolution
import java.io.IOException
import java.io.InputStream
import java.io.OutputStream
import java.nio.ByteBuffer
import java.nio.ByteOrder

/**
 * One event for x-input-relay, the userland program that injects pointer and keys into the X
 * screen through XTEST. The format is defined in userland/x-input-relay.c
 * (`struct relay_event`): 12 bytes, little endian, `u32 type, i32 a, i32 b`.
 */
sealed class RelayEvent(val type: Int, val a: Int, val b: Int) {
    /** The pointer to ([x], [y]) on the X screen. */
    data class Motion(val x: Int, val y: Int) : RelayEvent(MOTION, x, y)

    /** The pointer by ([dx], [dy]), inside the pointer area ([Area]) where x-input-relay has one. */
    data class Relative(val dx: Int, val dy: Int) : RelayEvent(RELATIVE, dx, dy)

    /** X [button] (1 left, 2 middle, 3 right, 4/5 wheel up/down) pressed or released. */
    data class Button(val button: Int, val down: Boolean) : RelayEvent(BUTTON, button, if (down) 1 else 0)

    /** The key carrying X [keysym] pressed or released. */
    data class Key(val keysym: Int, val down: Boolean) : RelayEvent(KEY, keysym, if (down) 1 else 0)

    /**
     * The game is on the app's screen again ([onScreen]) or no longer: x-input-relay gives the game's
     * window the focus back or takes it away, as a desktop does, so the game uses its own background
     * frame rate meanwhile.
     */
    data class Focus(val onScreen: Boolean) : RelayEvent(FOCUS, if (onScreen) 1 else 0, 0)

    /**
     * The pointer area, [rect] on the X screen ([GameResolution.pointerArea]; each of its four numbers
     * 0 to 65535, a = left | top << 16, b = width | height << 16): x-input-relay stops each later
     * [Relative] at its edges, from where the pointer really is ([PointerBounds]).
     */
    data class Area(val rect: GameResolution.PictureRect) :
        RelayEvent(AREA, rect.left or (rect.top shl 16), rect.width or (rect.height shl 16))

    companion object {
        const val SIZE = 12
        const val MOTION = 1
        const val RELATIVE = 2
        const val BUTTON = 3
        const val KEY = 4
        const val FOCUS = 5
        const val AREA = 6

        const val LEFT = 1
        const val MIDDLE = 2
        const val RIGHT = 3
        const val WHEEL_UP = 4
        const val WHEEL_DOWN = 5
    }

    fun encode(): ByteArray =
        ByteBuffer.allocate(SIZE).order(ByteOrder.LITTLE_ENDIAN).putInt(type).putInt(a).putInt(b).array()
}

/**
 * The game's window while it is alone on the X screen: at [x], [y] and [width] x [height] on the
 * [screenWidth] x [screenHeight] X screen (parts of it may lie off the screen). A full-screen game's
 * covers the screen; a windowed game's sits where Wine put it, as if a window manager framed it.
 */
data class GameWindow(val x: Int, val y: Int, val width: Int, val height: Int, val screenWidth: Int, val screenHeight: Int)

/**
 * The X screen's pointer image, as x-input-relay tells it ('I'): [width] x [height] [pixels], rows
 * from the top, each 0xAARRGGBB with premultiplied alpha (XFixes' form), its hot spot at [hotX],
 * [hotY] in it.
 */
class PointerImage(val width: Int, val height: Int, val hotX: Int, val hotY: Int, val pixels: IntArray) {
    /** The pixels as Android's colours take them: 0xAARRGGBB with straight alpha. */
    fun colors(): IntArray = IntArray(pixels.size) { i ->
        val p = pixels[i]
        val a = p ushr 24
        if (a == 0 || a == 255) {
            p
        } else {
            fun straight(shift: Int) = ((p ushr shift and 0xff) * 255 + a / 2) / a
            (a shl 24) or (straight(16).coerceAtMost(255) shl 16) or (straight(8).coerceAtMost(255) shl 8) or
                straight(0).coerceAtMost(255)
        }
    }

    /** Where it covers the X screen while its hot spot is at ([x], [y]), in X screen pixels. */
    fun rect(x: Int, y: Int) = GameResolution.PictureRect(x - hotX, y - hotY, width, height)
}

/** One of x-input-relay's words, read from the app's end of its socket ([RelayWords]). */
sealed interface RelayWord {
    /** 'C' ([window] null: the game's window is not alone on the X screen, or none is named yet), or 'F': it is alone, there. */
    data class Windows(val window: GameWindow?) : RelayWord

    /** 'I': the X screen's pointer shows, with this [image]. */
    class Pointer(val image: PointerImage) : RelayWord

    /** 'H': the pointer shows nothing (the game draws its own). */
    data object PointerHidden : RelayWord

    /** 'P': the pointer's hot spot is at [x], [y] on the X screen, while it shows. */
    data class PointerAt(val x: Int, val y: Int) : RelayWord
}

/**
 * x-input-relay's words, read from the app's end of its socket (the format is defined in
 * userland/x-input-relay.c): on the X screen's windows, 'C', the game's window is not alone on
 * the X screen (or none is named yet), and 'F' and six little-endian i32, the game's window alone
 * and where it is ([GameWindow]); on the X screen's pointer, 'I' its image ([PointerImage]: four
 * little-endian i32, then its pixels as little-endian u32), 'H' it shows nothing, and 'P' and two
 * little-endian i32, where it is.
 */
object RelayWords {
    const val NOT_ALONE = 'C'.code
    const val ALONE = 'F'.code
    const val POINTER = 'I'.code
    const val POINTER_HIDDEN = 'H'.code
    const val POINTER_AT = 'P'.code

    /** The largest pointer image each way (POINTER_MAX in x-input-relay.c). */
    const val POINTER_MAX = 128

    /**
     * The next word on [input]. A byte that starts no word is skipped. Throws [IOException] at the
     * end of the stream (the relay has gone), and for a pointer image of a size the relay never sends.
     */
    fun read(input: InputStream): RelayWord {
        while (true) {
            when (input.read()) {
                -1 -> throw IOException("x-input-relay's socket closed")
                NOT_ALONE -> return RelayWord.Windows(null)
                ALONE -> {
                    val b = numbers(input, 6)
                    return RelayWord.Windows(GameWindow(b.int, b.int, b.int, b.int, b.int, b.int))
                }
                POINTER -> {
                    val head = numbers(input, 4)
                    val width = head.int
                    val height = head.int
                    if (width !in 1..POINTER_MAX || height !in 1..POINTER_MAX) {
                        throw IOException("x-input-relay's pointer image of ${width}x$height is not 1 to $POINTER_MAX each way")
                    }
                    val hotX = head.int
                    val hotY = head.int
                    val pixels = numbers(input, width * height)
                    return RelayWord.Pointer(PointerImage(width, height, hotX, hotY, IntArray(width * height) { pixels.int }))
                }
                POINTER_HIDDEN -> return RelayWord.PointerHidden
                POINTER_AT -> {
                    val b = numbers(input, 2)
                    return RelayWord.PointerAt(b.int, b.int)
                }
            }
        }
    }

    /** The next [count] little-endian 32-bit numbers on [input], whole. */
    private fun numbers(input: InputStream, count: Int): ByteBuffer {
        val bytes = ByteArray(count * 4)
        var have = 0
        while (have < bytes.size) {
            val n = input.read(bytes, have, bytes.size - have)
            if (n < 0) throw IOException("x-input-relay's socket closed in a word")
            have += n
        }
        return ByteBuffer.wrap(bytes).order(ByteOrder.LITTLE_ENDIAN)
    }
}

/** Writes events to the relay's socket (the app's end of the pair whose other end is its stdin). */
class RelaySender(private val out: OutputStream) {
    @Synchronized
    fun send(event: RelayEvent) {
        out.write(event.encode())
    }

    @Synchronized
    fun send(events: List<RelayEvent>) {
        if (events.isEmpty()) return
        val bytes = ByteArray(events.size * RelayEvent.SIZE)
        events.forEachIndexed { i, e -> e.encode().copyInto(bytes, i * RelayEvent.SIZE) }
        out.write(bytes)
    }
}
