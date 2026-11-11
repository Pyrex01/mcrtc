// A libwebrtc data channel with coroutine-friendly events.

package com.mcrtc.android.net

import java.nio.ByteBuffer
import java.util.concurrent.atomic.AtomicLong
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.withTimeoutOrNull
import org.webrtc.DataChannel

/**
 * Messages are handed over on a libwebrtc thread, which must never wait, so they queue here
 * without a bound. A local socket that stops reading for long is cut off instead.
 */
private const val MAX_QUEUED_BYTES = 32L * 1024 * 1024

class RtcChannel(private val channel: DataChannel) : DataChannel.Observer {
    /** Everything received, in order. Ends when the channel closes. */
    private val messages = Channel<ByteArray>(Channel.UNLIMITED)
    private val queued = AtomicLong()
    private val bufferChanged = Channel<Unit>(Channel.CONFLATED)
    val open = MutableStateFlow(false)
    private var released = false

    init {
        channel.registerObserver(this)
        onStateChange()
    }

    override fun onStateChange() {
        when (channel.state()) {
            DataChannel.State.OPEN -> open.value = true
            DataChannel.State.CLOSED -> messages.close()
            else -> Unit
        }
    }

    override fun onMessage(buffer: DataChannel.Buffer) {
        // The buffer is only valid during this call.
        val data = ByteArray(buffer.data.remaining())
        buffer.data.get(data)
        if (queued.addAndGet(data.size.toLong()) > MAX_QUEUED_BYTES) {
            messages.close()
            return
        }
        messages.trySend(data)
    }

    override fun onBufferedAmountChange(previousAmount: Long) {
        bufferChanged.trySend(Unit)
    }

    /** The next message, or null once the channel is closed. */
    suspend fun receive(): ByteArray? {
        val data = messages.receiveCatching().getOrNull() ?: return null
        queued.addAndGet(-data.size.toLong())
        return data
    }

    suspend fun awaitOpen() {
        open.first { it }
    }

    /** Waits until the amount buffered changes, or `limitMs` passes. */
    suspend fun awaitBufferChange(limitMs: Long) {
        withTimeoutOrNull(limitMs) { bufferChanged.receive() }
    }

    val label: String get() = guarded("") { channel.label() }

    fun bufferedAmount(): Long = guarded(0L) { channel.bufferedAmount() }

    fun send(data: ByteArray, binary: Boolean = true): Boolean =
        guarded(false) { channel.send(DataChannel.Buffer(ByteBuffer.wrap(data), binary)) }

    fun sendText(text: String): Boolean = send(text.toByteArray(), binary = false)

    fun close() = guarded(Unit) { channel.close() }

    /** Frees the native channel. Called by [Peer.dispose]. */
    fun release() {
        synchronized(this) {
            if (released) return
            released = true
            channel.unregisterObserver()
            channel.dispose()
        }
        messages.close()
    }

    private inline fun <T> guarded(default: T, action: () -> T): T =
        synchronized(this) { if (released) default else action() }
}
