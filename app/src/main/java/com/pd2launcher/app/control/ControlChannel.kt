package com.pd2launcher.app.control

import android.os.ParcelFileDescriptor
import android.system.ErrnoException
import android.system.Os
import android.system.OsConstants
import android.system.StructPollfd
import java.io.Closeable
import java.io.FileInputStream
import java.io.FileOutputStream
import java.io.IOException
import java.io.InputStream
import java.io.OutputStream
import java.net.SocketTimeoutException
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

/** The engine answered `refused`: it speaks only [supported]. */
class ProtocolRefusedException(val supported: List<Long>) :
    IOException("the engine refused protocol $PROTOCOL; it speaks $supported")

/** The engine sent something the protocol does not allow at that point. */
class ProtocolException(message: String, cause: Throwable? = null) : IOException(message, cause)

/**
 * The app's end of the Control channel: the app speaks first with [handshake], then sends
 * [send]s and follows [snapshot]. Closing it is what tells the engine to stop safely and exit.
 *
 * [closer] must unblock a read in progress on [input] (for a socket: shut it down
 * before closing it, as [overSocket] does). [readable] waits up to the given milliseconds for
 * [input] to have data or its end and says whether it has (false also when the wait ended early);
 * only a [handshake] with a deadline uses it, so the reader thread's reads never time out.
 */
class ControlChannel(
    private val input: InputStream,
    private val output: OutputStream,
    private val closer: Closeable,
    private val readable: ((timeoutMs: Int) -> Boolean)? = null,
    private val onCommandError: (EngineMessage.CommandError) -> Unit = {},
) : Closeable {

    sealed interface State {
        data object Idle : State
        data object Handshaking : State
        data class Connected(val welcome: EngineMessage.Welcome) : State
        data class Refused(val supported: List<Long>) : State
        /** [cause] is null for an orderly end (either side closed). */
        data class Closed(val cause: Throwable?) : State
    }

    /** Guards [state] changes. */
    private val lock = Any()
    /** Orders writes, so commands go out in `seq` order; never held by [close]. */
    private val writeLock = Any()
    private var nextSeq = 1L
    private val _state = MutableStateFlow<State>(State.Idle)
    private val _snapshot = MutableStateFlow<EngineMessage.SnapshotMessage?>(null)

    val state: StateFlow<State> = _state.asStateFlow()

    /** The newest snapshot the engine sent; null until the first. */
    val snapshot: StateFlow<EngineMessage.SnapshotMessage?> = _snapshot.asStateFlow()

    /**
     * Send `hello` and wait for the engine's answer (blocking), for at most [timeoutMs] when
     * given (then [SocketTimeoutException], and the channel closes). On `welcome` the
     * reader thread starts and the welcome is returned; on `refused` the channel
     * closes and [ProtocolRefusedException] is thrown.
     */
    fun handshake(appVersion: String, hostFacts: HostFacts, timeoutMs: Long? = null): EngineMessage.Welcome {
        synchronized(lock) {
            check(_state.value == State.Idle) { "hello is the first message, and only the first" }
            _state.value = State.Handshaking
        }
        try {
            write(AppMessage.Hello(PROTOCOL, appVersion, hostFacts))
            val answerInput = timeoutMs?.let { Deadline(input, checkNotNull(readable) { "a deadline needs readable" }, it) } ?: input
            val frame = Frames.read(answerInput) ?: throw ProtocolException("the engine closed before welcome")
            when (val answer = decode(frame)) {
                is EngineMessage.Welcome -> {
                    // An engine that welcomes another protocol than the one hello named would
                    // read and write messages this app does not.
                    if (answer.protocol != PROTOCOL) throw ProtocolException("the engine welcomed protocol ${answer.protocol}, not $PROTOCOL")
                    synchronized(lock) {
                        if (_state.value != State.Handshaking) throw ProtocolException("closed during the handshake")
                        _state.value = State.Connected(answer)
                    }
                    Thread(::readLoop, "pd2-control-reader").apply { isDaemon = true }.start()
                    return answer
                }
                is EngineMessage.Refused -> {
                    _state.value = State.Refused(answer.supported)
                    closeTransport()
                    throw ProtocolRefusedException(answer.supported)
                }
                else -> throw ProtocolException("expected welcome, got ${answer::class.simpleName}")
            }
        } catch (error: IOException) {
            if (error !is ProtocolRefusedException) finish(error)
            throw error
        }
    }

    /** Send one command after the handshake; returns its `seq`. */
    fun send(command: Command): Long = synchronized(writeLock) {
        check(_state.value is State.Connected) { "commands are sent only after welcome" }
        val seq = nextSeq++
        try {
            write(AppMessage.CommandMessage(seq, command))
        } catch (error: IOException) {
            finish(error)
            throw error
        }
        seq
    }

    /**
     * Send `authorize` with both CD keys after the handshake; returns its `seq`. The keys never
     * become a String: [d2Key], [lodKey] and the message's bytes are cleared afterwards, sent or not.
     */
    fun sendAuthorize(d2Key: CharArray, lodKey: CharArray): Long = try {
        synchronized(writeLock) {
            check(_state.value is State.Connected) { "commands are sent only after welcome" }
            val seq = nextSeq++
            val body = authorizeCommandJson(seq, d2Key, lodKey)
            try {
                Frames.write(output, body)
            } catch (error: IOException) {
                finish(error)
                throw error
            } finally {
                body.fill(0)
            }
            seq
        }
    } finally {
        d2Key.fill('\u0000')
        lodKey.fill('\u0000')
    }

    override fun close() = finish(null)

    private fun readLoop() {
        try {
            while (true) {
                val frame = Frames.read(input) ?: break
                when (val message = decode(frame)) {
                    is EngineMessage.SnapshotMessage -> _snapshot.value = message
                    is EngineMessage.CommandError -> onCommandError(message)
                    else -> throw ProtocolException("unexpected ${message::class.simpleName} after welcome")
                }
            }
            finish(null)
        } catch (error: IOException) {
            finish(error)
        }
    }

    private fun write(message: AppMessage) {
        val body = ControlJson.encodeToString(AppMessage.serializer(), message).encodeToByteArray()
        synchronized(writeLock) { Frames.write(output, body) }
    }

    private fun decode(frame: ByteArray): EngineMessage = try {
        ControlJson.decodeFromString(EngineMessage.serializer(), frame.decodeToString(throwOnInvalidSequence = true))
    } catch (error: CharacterCodingException) {
        throw ProtocolException("an engine message is not UTF-8", error)
    } catch (error: IllegalArgumentException) {
        // Also kotlinx.serialization's SerializationException.
        throw ProtocolException("an engine message could not be decoded", error)
    }

    /** Close once; a closed channel keeps the first reason it closed for. */
    private fun finish(cause: Throwable?) {
        synchronized(lock) {
            when (_state.value) {
                is State.Closed, is State.Refused -> return
                else -> _state.value = State.Closed(cause)
            }
        }
        closeTransport()
    }

    private fun closeTransport() {
        try {
            closer.close()
        } catch (_: IOException) {
        }
    }

    /**
     * [input] read only once [readable] says it has something, until [timeoutMs] from now: the
     * handshake's deadline without a socket receive timeout, which the kernel would still apply
     * to the reader thread's first read after it was lifted.
     */
    private class Deadline(
        private val input: InputStream,
        private val readable: (timeoutMs: Int) -> Boolean,
        private val timeoutMs: Long,
    ) : InputStream() {
        private val end = System.nanoTime() / 1_000_000 + timeoutMs

        override fun read(): Int {
            val one = ByteArray(1)
            return if (read(one, 0, 1) < 0) -1 else one[0].toInt() and 0xff
        }

        override fun read(b: ByteArray, off: Int, len: Int): Int {
            while (true) {
                val left = end - System.nanoTime() / 1_000_000
                if (left <= 0) throw SocketTimeoutException("no answer within $timeoutMs ms")
                if (readable(left.coerceAtMost(Int.MAX_VALUE.toLong()).toInt())) return input.read(b, off, len)
            }
        }
    }

    companion object {
        /** A channel over the app's end of the socket pair; it takes ownership of [socket]. */
        fun overSocket(
            socket: ParcelFileDescriptor,
            onCommandError: (EngineMessage.CommandError) -> Unit = {},
        ): ControlChannel {
            val descriptor = socket.fileDescriptor
            val readable = { timeoutMs: Int ->
                val wanted = StructPollfd().apply {
                    fd = descriptor
                    events = OsConstants.POLLIN.toShort()
                }
                try {
                    Os.poll(arrayOf(wanted), timeoutMs) > 0
                } catch (error: ErrnoException) {
                    // A signal ended the wait early: the deadline's loop waits again for what is left.
                    if (error.errno != OsConstants.EINTR) throw IOException(error)
                    false
                }
            }
            return ControlChannel(FileInputStream(descriptor), FileOutputStream(descriptor), {
                try {
                    Os.shutdown(descriptor, OsConstants.SHUT_RDWR)
                } catch (_: ErrnoException) {
                }
                socket.close()
            }, readable, onCommandError)
        }
    }
}
