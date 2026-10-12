package com.pd2launcher.app.control

import android.os.ParcelFileDescriptor
import android.system.ErrnoException
import android.system.Os
import android.system.OsConstants
import java.io.Closeable
import java.io.FileInputStream
import java.io.FileOutputStream
import java.io.IOException
import java.io.InputStream
import java.io.OutputStream
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
 * before closing it, as [overSocket] does).
 */
class ControlChannel(
    private val input: InputStream,
    private val output: OutputStream,
    private val closer: Closeable,
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
     * Send `hello` and wait for the engine's answer (blocking). On `welcome` the
     * reader thread starts and the welcome is returned; on `refused` the channel
     * closes and [ProtocolRefusedException] is thrown.
     */
    fun handshake(appVersion: String, hostFacts: HostFacts): EngineMessage.Welcome {
        synchronized(lock) {
            check(_state.value == State.Idle) { "hello is the first message, and only the first" }
            _state.value = State.Handshaking
        }
        try {
            write(AppMessage.Hello(PROTOCOL, appVersion, hostFacts))
            val frame = Frames.read(input) ?: throw ProtocolException("the engine closed before welcome")
            when (val answer = decode(frame)) {
                is EngineMessage.Welcome -> {
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

    companion object {
        /** A channel over the app's end of the socket pair; it takes ownership of [socket]. */
        fun overSocket(
            socket: ParcelFileDescriptor,
            onCommandError: (EngineMessage.CommandError) -> Unit = {},
        ): ControlChannel {
            val descriptor = socket.fileDescriptor
            return ControlChannel(FileInputStream(descriptor), FileOutputStream(descriptor), {
                try {
                    Os.shutdown(descriptor, OsConstants.SHUT_RDWR)
                } catch (_: ErrnoException) {
                }
                socket.close()
            }, onCommandError)
        }
    }
}
