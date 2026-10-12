package com.pd2launcher.app

import android.os.ParcelFileDescriptor
import java.io.Closeable
import java.io.File
import java.io.RandomAccessFile
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.nio.channels.FileChannel

/**
 * The X screen's framebuffer: Xvfb's screen 0 laid out as an XWD file ([XwdHeader], colormap,
 * pixels). It is a memfd the app makes and passes to Xvfb as its descriptor 3 (`-fbfd 3`,
 * `userland/patches/xserver/0003`), so the moving picture never reaches flash; when no memfd can
 * be made, it is Xvfb's `-fbdir` file. Each read opens it anew ([open]), so a closed framebuffer
 * reads as nothing, never as another file.
 */
class Framebuffer private constructor(
    private val open: () -> FileChannel,
    private val duplicate: () -> ParcelFileDescriptor,
    private val release: () -> Unit,
) : Closeable {
    /** Xvfb's header; null before Xvfb has sized the framebuffer and written a 32-bit header, or once closed. */
    fun header(): XwdHeader? = read { readHeader(it) }

    /** The pixels mapped ([ScreenPixels.map]); null when [header] is, or the pixels are not all there. */
    fun map(): ScreenPixels? = read { ScreenPixels.map(it) }

    /** A new descriptor of the framebuffer for native code, which owns it; -1 once closed. */
    fun descriptor(): Int = try {
        duplicate().detachFd()
    } catch (_: Exception) {
        -1
    }

    /** The owner's end: readers that have it mapped keep their mapping. */
    override fun close() = release()

    private fun <T> read(block: (FileChannel) -> T?): T? = try {
        open().use(block)
    } catch (_: Exception) {
        // Closed (the descriptor is gone), not there yet, or short: nothing to read now.
        null
    }

    companion object {
        /** Xvfb's descriptor for it: the third after standard input, output and error. */
        const val XVFB_DESCRIPTOR = 3

        /** A memfd [memory] (owned by the result, which [close] closes). */
        fun inMemory(memory: ParcelFileDescriptor): Framebuffer = Framebuffer(
            open = { ParcelFileDescriptor.AutoCloseInputStream(memory.dup()).channel },
            duplicate = { memory.dup() },
            release = { memory.close() },
        )

        /** Xvfb's `-fbdir` [file], opened anew for each read. */
        fun inFile(file: File): Framebuffer = Framebuffer(
            open = { RandomAccessFile(file, "r").channel },
            duplicate = { ParcelFileDescriptor.open(file, ParcelFileDescriptor.MODE_READ_ONLY) },
            release = {},
        )

        /** The header at the start of [channel] (a positional read), when it is a 32-bit screen's of sizes taken ([XwdHeader.plausible]). */
        fun readHeader(channel: FileChannel): XwdHeader? {
            val bytes = ByteBuffer.allocate(XwdHeader.SIZE)
            while (bytes.hasRemaining()) {
                if (channel.read(bytes, bytes.position().toLong()) < 0) break
            }
            return XwdHeader.parse(bytes.array().copyOf(bytes.position()))?.takeIf { it.plausible }
        }
    }
}

/**
 * The X screen's pixels mapped read-only: Xvfb's [header] and the [pixels] it describes, from the
 * first pixel, `bytesPerLine` x `height` bytes of little-endian BGRX words (0xXXRRGGBB). The
 * mapping shares Xvfb's memory, so it shows every later frame, and it stays valid after the
 * framebuffer is closed.
 */
class ScreenPixels(val header: XwdHeader, val pixels: ByteBuffer) {
    /** The pixel at ([x], [y]). */
    fun pixel(x: Int, y: Int): Int = pixels.getInt(y * header.bytesPerLine + x * 4)

    companion object {
        /** [channel]'s pixels, or null before its header is written or while it is shorter than the header says. */
        fun map(channel: FileChannel): ScreenPixels? {
            val header = Framebuffer.readHeader(channel) ?: return null
            val length = header.bytesPerLine.toLong() * header.height
            if (channel.size() < header.pixelOffset + length) return null
            val pixels = channel.map(FileChannel.MapMode.READ_ONLY, header.pixelOffset, length).order(ByteOrder.LITTLE_ENDIAN)
            return ScreenPixels(header, pixels)
        }
    }
}
