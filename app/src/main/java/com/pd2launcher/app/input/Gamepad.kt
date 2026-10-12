package com.pd2launcher.app.input

import java.io.File
import java.io.RandomAccessFile
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.nio.channels.FileChannel

/**
 * The gamepad file the game's `xinput1_4.dll` (the engine's XInput bridge) maps, named to the
 * guest in `PD2_GAMEPAD_FILE`. Little endian, 32 bytes:
 *
 *     0  u32 magic 'PD2G'      4  u32 packet (seqlock: odd while the fields change)
 *     8  u32 connected (0/1)   12 u16 XInput buttons
 *     14 u8  left trigger      15 u8  right trigger
 *     16 i16 lx, ly, rx, ry (XInput signs: up is positive)
 *     24..31 reserved
 */
object GamepadLayout {
    const val SIZE = 32
    const val MAGIC = 0x47324450 // "PD2G"
    const val MAGIC_OFFSET = 0
    const val PACKET_OFFSET = 4
    const val CONNECTED_OFFSET = 8
    const val BUTTONS_OFFSET = 12
    const val LEFT_TRIGGER_OFFSET = 14
    const val RIGHT_TRIGGER_OFFSET = 15
    const val LX_OFFSET = 16
    const val LY_OFFSET = 18
    const val RX_OFFSET = 20
    const val RY_OFFSET = 22

    // XInput's wButtons bits.
    const val DPAD_UP = 0x0001
    const val DPAD_DOWN = 0x0002
    const val DPAD_LEFT = 0x0004
    const val DPAD_RIGHT = 0x0008
    const val START = 0x0010
    const val BACK = 0x0020
    const val LEFT_THUMB = 0x0040
    const val RIGHT_THUMB = 0x0080
    const val LEFT_SHOULDER = 0x0100
    const val RIGHT_SHOULDER = 0x0200
    const val A = 0x1000
    const val B = 0x2000
    const val X = 0x4000
    const val Y = 0x8000
}

/** One XInput pad state: [buttons] bits, triggers 0..255, thumbs -32768..32767 (up positive). */
data class PadState(
    val buttons: Int = 0,
    val leftTrigger: Int = 0,
    val rightTrigger: Int = 0,
    val lx: Int = 0,
    val ly: Int = 0,
    val rx: Int = 0,
    val ry: Int = 0,
) {
    companion object {
        /** Connected, nothing pressed, sticks centred: what the game sees in mouse mode. */
        val NEUTRAL = PadState()
    }
}

/**
 * Writes pad states into [buffer] (the mapped gamepad file) under the bridge's seqlock: the
 * packet number goes odd, the fields change, the packet number goes even again, with [fence] (a
 * full memory barrier) between, so the game never takes a half-written state for a whole one.
 * The packet number grows by two per state, which also tells the game the state changed.
 *
 * One writer at a time: the UI thread writes states while the session's worker may [close] the
 * pad, so both are synchronized, and once closed (the game told the pad is gone) later writes
 * change nothing.
 */
class GamepadWriter(private val buffer: ByteBuffer, private val fence: () -> Unit) {
    private var packet: Int

    init {
        buffer.order(ByteOrder.LITTLE_ENDIAN)
        packet = buffer.getInt(GamepadLayout.PACKET_OFFSET) and 1.inv()
        buffer.putInt(GamepadLayout.PACKET_OFFSET, packet)
        fence()
        buffer.putInt(GamepadLayout.MAGIC_OFFSET, GamepadLayout.MAGIC)
        fence()
    }

    private var closed = false

    @Synchronized
    fun write(state: PadState) {
        if (!closed) put(state, connected = true)
    }

    /** Tells the game the pad is gone (neutral, disconnected); every later [write] is a no-op. */
    @Synchronized
    fun close() {
        if (closed) return
        put(PadState.NEUTRAL, connected = false)
        closed = true
    }

    private fun put(state: PadState, connected: Boolean) {
        buffer.putInt(GamepadLayout.PACKET_OFFSET, packet + 1)
        fence()
        buffer.putInt(GamepadLayout.CONNECTED_OFFSET, if (connected) 1 else 0)
        buffer.putShort(GamepadLayout.BUTTONS_OFFSET, state.buttons.toShort())
        buffer.put(GamepadLayout.LEFT_TRIGGER_OFFSET, state.leftTrigger.coerceIn(0, 255).toByte())
        buffer.put(GamepadLayout.RIGHT_TRIGGER_OFFSET, state.rightTrigger.coerceIn(0, 255).toByte())
        buffer.putShort(GamepadLayout.LX_OFFSET, thumb(state.lx))
        buffer.putShort(GamepadLayout.LY_OFFSET, thumb(state.ly))
        buffer.putShort(GamepadLayout.RX_OFFSET, thumb(state.rx))
        buffer.putShort(GamepadLayout.RY_OFFSET, thumb(state.ry))
        fence()
        packet += 2
        buffer.putInt(GamepadLayout.PACKET_OFFSET, packet)
    }

    private fun thumb(value: Int): Short = value.coerceIn(Short.MIN_VALUE.toInt(), Short.MAX_VALUE.toInt()).toShort()
}

/**
 * The gamepad file itself, mapped shared so the game's mapping sees every write. The fence is a
 * real full barrier (libinputfence.so): the reader is another process on another core.
 */
class GamepadFile private constructor(private val file: RandomAccessFile, buffer: ByteBuffer) : AutoCloseable {
    private val writer = GamepadWriter(buffer) { nativeFence() }

    init {
        writer.write(PadState.NEUTRAL)
    }

    fun write(state: PadState) = writer.write(state)

    /**
     * Tells the game the pad is gone and closes the file; a write racing in from the UI thread
     * after this is a no-op. Android has no API to unmap a MappedByteBuffer: the mapping goes
     * with the buffer once this object is unreachable.
     */
    override fun close() {
        writer.close()
        file.close()
    }

    companion object {
        init {
            System.loadLibrary("inputfence")
        }

        /** Creates (or reuses) [path] as a 32-byte gamepad file: a connected, neutral pad. */
        fun open(path: File): GamepadFile {
            val file = RandomAccessFile(path, "rw")
            try {
                file.setLength(GamepadLayout.SIZE.toLong())
                val buffer = file.channel.map(FileChannel.MapMode.READ_WRITE, 0, GamepadLayout.SIZE.toLong())
                return GamepadFile(file, buffer)
            } catch (e: Exception) {
                file.close()
                throw e
            }
        }

        @JvmStatic
        private external fun nativeFence()
    }
}
