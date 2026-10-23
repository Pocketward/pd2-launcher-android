package com.pd2launcher.app.input

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

    /** The pointer by ([dx], [dy]). */
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

    companion object {
        const val SIZE = 12
        const val MOTION = 1
        const val RELATIVE = 2
        const val BUTTON = 3
        const val KEY = 4
        const val FOCUS = 5

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
 * x-input-relay's words on the X screen's windows, read from the app's end of its socket
 * (the format is defined in userland/x-input-relay.c): 'C', the game's window is not alone on
 * the X screen (or none is named yet); 'F' and six little-endian i32, the game's window alone
 * and where it is ([GameWindow]).
 */
object RelayWindows {
    const val NOT_ALONE = 'C'.code
    const val ALONE = 'F'.code
    private const val PLACE_SIZE = 6 * 4

    /**
     * The next word on [input]: the game's window while alone, or null while not. A byte that
     * starts no word is skipped. Throws [IOException] at the end of the stream (the relay has gone).
     */
    fun read(input: InputStream): GameWindow? {
        while (true) {
            when (input.read()) {
                -1 -> throw IOException("x-input-relay's socket closed")
                NOT_ALONE -> return null
                ALONE -> {
                    val bytes = ByteArray(PLACE_SIZE)
                    var have = 0
                    while (have < PLACE_SIZE) {
                        val n = input.read(bytes, have, PLACE_SIZE - have)
                        if (n < 0) throw IOException("x-input-relay's socket closed in a word")
                        have += n
                    }
                    val b = ByteBuffer.wrap(bytes).order(ByteOrder.LITTLE_ENDIAN)
                    return GameWindow(b.int, b.int, b.int, b.int, b.int, b.int)
                }
            }
        }
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
