package com.pd2launcher.app.control

import java.io.EOFException
import java.io.IOException
import java.io.InputStream
import java.io.OutputStream
import java.net.SocketTimeoutException

/** A frame over [Frames.MAX_FRAME_BYTES]: a protocol error. */
class FrameTooLargeException(val length: Long) :
    IOException("a $length-byte frame is over the ${Frames.MAX_FRAME_BYTES}-byte limit")

/**
 * The Control channel's framing (as the engine's `read_frame` and `write_frame`):
 * a 4-byte big-endian length, then that many bytes of UTF-8 JSON.
 */
object Frames {
    /** The largest frame either side may send: 1 MiB of JSON. */
    const val MAX_FRAME_BYTES: Int = 1024 * 1024

    /**
     * One frame's body, or null at the end of the connection between frames. A
     * frame over the limit is refused before its body is read. A read that returns nothing
     * is not progress but a timeout ([SocketTimeoutException]): Android's FileInputStream
     * returns 0 when a socket's receive timeout runs out, and looping on it never ends.
     */
    fun read(input: InputStream): ByteArray? {
        val header = ByteArray(4)
        var filled = 0
        while (filled < header.size) {
            val read = input.read(header, filled, header.size - filled)
            if (read < 0) {
                if (filled == 0) return null
                throw EOFException("the connection ended inside a frame header")
            }
            if (read == 0) throw SocketTimeoutException("a read returned nothing")
            filled += read
        }
        val length = ((header[0].toLong() and 0xff) shl 24) or
            ((header[1].toLong() and 0xff) shl 16) or
            ((header[2].toLong() and 0xff) shl 8) or
            (header[3].toLong() and 0xff)
        if (length > MAX_FRAME_BYTES) throw FrameTooLargeException(length)
        val body = ByteArray(length.toInt())
        var got = 0
        while (got < body.size) {
            val read = input.read(body, got, body.size - got)
            if (read < 0) throw EOFException("the connection ended inside a frame")
            if (read == 0) throw SocketTimeoutException("a read returned nothing")
            got += read
        }
        return body
    }

    /**
     * Write [body] as one frame; one over the limit is never written. [body] is not copied (it
     * may hold CD keys its caller clears); callers serialize their writes.
     */
    fun write(output: OutputStream, body: ByteArray) {
        if (body.size > MAX_FRAME_BYTES) throw FrameTooLargeException(body.size.toLong())
        val header = ByteArray(4)
        header[0] = (body.size ushr 24).toByte()
        header[1] = (body.size ushr 16).toByte()
        header[2] = (body.size ushr 8).toByte()
        header[3] = body.size.toByte()
        output.write(header)
        output.write(body)
        output.flush()
    }
}
