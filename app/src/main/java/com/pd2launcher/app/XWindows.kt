package com.pd2launcher.app

import android.net.LocalSocket
import android.net.LocalSocketAddress
import java.io.ByteArrayOutputStream
import java.io.EOFException
import java.io.IOException
import java.io.InputStream
import java.io.OutputStream

/**
 * The top-level windows that show on the game environment's X screen, asked of Xvfb itself: the
 * X protocol on its socket, the way x-input-relay looks at the windows (userland/x-input-relay.c,
 * shows()). A window shows when it is mapped and viewable, an InputOutput window more than one
 * pixel wide and high (Wine's helper windows are 1x1), and part of it is on the X screen.
 * [LauncherHang] uses it to see the official PD2 launcher's windows.
 */
object XWindows {
    /** How long Xvfb may take for one answer. */
    private const val TIMEOUT_MS = 2_000

    private const val GET_WINDOW_ATTRIBUTES = 3
    private const val GET_GEOMETRY = 14
    private const val QUERY_TREE = 15
    private const val INPUT_OUTPUT = 1
    private const val VIEWABLE = 2
    private const val GENERIC_EVENT = 35
    /** No answer of these few requests is this long: a longer length is a broken stream. */
    private const val MAX_REPLY_WORDS = 1 shl 18

    /** The root window and the X screen's size. */
    data class Screen(val root: Int, val width: Int, val height: Int)

    /** A window that shows: its outer size, border included. */
    data class Window(val width: Int, val height: Int)

    /**
     * The windows that show on the X screen of [display] (":0", Xvfb's abstract socket
     * `@/tmp/.X11-unix/X0`), bottom to top, or null when Xvfb does not answer, refuses the
     * connection or the answer is broken.
     */
    fun showing(display: String): List<Window>? {
        val number = display.removePrefix(":").substringBefore('.').toIntOrNull() ?: return null
        val socket = LocalSocket()
        return try {
            socket.connect(LocalSocketAddress("/tmp/.X11-unix/X$number", LocalSocketAddress.Namespace.ABSTRACT))
            socket.soTimeout = TIMEOUT_MS
            windows(socket.inputStream, socket.outputStream)
        } catch (_: IOException) {
            null
        } finally {
            try {
                socket.close()
            } catch (_: IOException) {
            }
        }
    }

    /**
     * The conversation of [showing] on a new connection's two streams: the connection setup, the
     * root window's children (bottom to top), and the attributes and geometry of each. Null when
     * Xvfb refuses the connection or answers the list of children with an X error; IOException
     * when the stream breaks or ends.
     */
    internal fun windows(input: InputStream, output: OutputStream): List<Window>? {
        output.write(setupRequest())
        output.flush()
        val screen = screen(readSetup(input) ?: return null) ?: return null
        output.write(request(QUERY_TREE, screen.root))
        output.flush()
        val children = children(readReply(input) ?: return null)
        if (children.isEmpty()) return emptyList()
        // All requests at once; Xvfb answers them in order.
        val requests = ByteArrayOutputStream()
        for (child in children) {
            requests.write(request(GET_WINDOW_ATTRIBUTES, child))
            requests.write(request(GET_GEOMETRY, child))
        }
        output.write(requests.toByteArray())
        output.flush()
        return children.mapNotNull {
            // An X error instead: the window went between the list and the question.
            val attributes = readReply(input)
            val geometry = readReply(input)
            if (attributes != null && geometry != null) shows(attributes, geometry, screen) else null
        }
    }

    /** The connection setup: little-endian, protocol 11.0, no authorization. */
    internal fun setupRequest(): ByteArray = byteArrayOf('l'.code.toByte(), 0, 11, 0, 0, 0, 0, 0, 0, 0, 0, 0)

    /** A request of [opcode] with one window as its only argument (QueryTree, GetWindowAttributes, GetGeometry). */
    internal fun request(opcode: Int, window: Int): ByteArray =
        byteArrayOf(opcode.toByte(), 0, 2, 0) + le32(window)

    /** The whole setup answer, or null when Xvfb refused the connection (Failed or Authenticate). */
    internal fun readSetup(input: InputStream): ByteArray? {
        val head = readFully(input, 8)
        val rest = readFully(input, u16(head, 6) * 4)
        return if (head[0].toInt() == 1) head + rest else null
    }

    /** The first screen of a successful setup answer [setup]; null when it has none. */
    internal fun screen(setup: ByteArray): Screen? {
        if (setup.size < 40 || setup[28].toInt() == 0) return null
        val vendor = u16(setup, 24)
        val formats = setup[29].toInt() and 0xff
        val at = 40 + (vendor + 3) / 4 * 4 + 8 * formats
        if (setup.size < at + 24) return null
        return Screen(u32(setup, at), u16(setup, at + 20), u16(setup, at + 22))
    }

    /**
     * The next answer: a reply's bytes, or null for an X error. Events (Xvfb sends MappingNotify to
     * every client when x-input-relay changes the keymap) are skipped.
     */
    internal fun readReply(input: InputStream): ByteArray? {
        while (true) {
            val head = readFully(input, 32)
            when (head[0].toInt() and 0x7f) {
                0 -> return null
                1 -> return head + readFully(input, words(head) * 4)
                GENERIC_EVENT -> readFully(input, words(head) * 4)
            }
        }
    }

    /** The children of a QueryTree reply, bottom to top. */
    internal fun children(tree: ByteArray): List<Int> {
        val count = u16(tree, 16)
        if (tree.size < 32 + 4 * count) throw IOException("QueryTree reply too short for $count children")
        return List(count) { u32(tree, 32 + 4 * it) }
    }

    /** The window of a GetWindowAttributes reply and a GetGeometry reply when it shows on [screen]; else null. */
    internal fun shows(attributes: ByteArray, geometry: ByteArray, screen: Screen): Window? {
        if (attributes.size < 44 || geometry.size < 32) return null
        if (u16(attributes, 12) != INPUT_OUTPUT || attributes[26].toInt() != VIEWABLE) return null
        val x = s16(geometry, 12)
        val y = s16(geometry, 14)
        val width = u16(geometry, 16)
        val height = u16(geometry, 18)
        val border = u16(geometry, 20)
        if (width <= 1 || height <= 1) return null
        // Its outer box, border included, against the screen.
        val outer = Window(width + 2 * border, height + 2 * border)
        return outer.takeIf { x < screen.width && y < screen.height && x + it.width > 0 && y + it.height > 0 }
    }

    private fun words(head: ByteArray): Int {
        val words = u32(head, 4)
        if (words < 0 || words > MAX_REPLY_WORDS) throw IOException("X answer of $words words")
        return words
    }

    private fun readFully(input: InputStream, length: Int): ByteArray {
        val bytes = ByteArray(length)
        var have = 0
        while (have < length) {
            val n = input.read(bytes, have, length - have)
            if (n < 0) throw EOFException("the X connection ended")
            have += n
        }
        return bytes
    }

    private fun le32(value: Int) = byteArrayOf(value.toByte(), (value shr 8).toByte(), (value shr 16).toByte(), (value shr 24).toByte())

    private fun u16(bytes: ByteArray, at: Int) = (bytes[at].toInt() and 0xff) or ((bytes[at + 1].toInt() and 0xff) shl 8)

    private fun s16(bytes: ByteArray, at: Int) = u16(bytes, at).toShort().toInt()

    private fun u32(bytes: ByteArray, at: Int) = u16(bytes, at) or (u16(bytes, at + 2) shl 16)
}
