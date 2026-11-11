// The shared TCP <-> data channel pump. A port of `src/bridge.rs`.
//
// A data channel here is a reliable, ordered byte stream, so chunks go out in the order they were
// read and the receiver writes them to TCP as they arrive.

package com.mcrtc.android.net

import java.io.Closeable
import java.net.Socket
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicLong
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch
import kotlinx.coroutines.withTimeoutOrNull

const val MAX_CHUNK = 16 * 1024
const val BUFFERED_LOW = 256L * 1024
const val BUFFERED_HIGH = 1024L * 1024

/** A stalled TCP peer has to be cut off, not waited for: its messages would pile up in memory. */
private const val WRITE_STALL_TIMEOUT_MS = 30_000L

/** How long to wait for the remote side to take the last bytes before closing. */
private const val FLUSH_TIMEOUT_MS = 15_000L

/** The buffered-amount event can lag, so capacity is also rechecked on a timer. */
private const val CAPACITY_RECHECK_MS = 20L

private const val FLUSH_RECHECK_MS = 5L

/** Counts something in use, like connections or friends. */
class Counter {
    val count = MutableStateFlow(0)

    fun get() = count.value

    fun acquire(): Slot = tryAcquire(null)!!

    /** Takes a slot unless `limit` are already in use. */
    fun tryAcquire(limit: Int?): Slot? {
        while (true) {
            val now = count.value
            if (limit != null && now >= limit) return null
            if (count.compareAndSet(now, now + 1)) return Slot(this)
        }
    }

    /** Resolves once fewer than `limit` slots are in use. */
    suspend fun waitBelow(limit: Int) {
        count.first { it < limit }
    }

    /** Waits up to `limitMs` for every slot to be given back. */
    suspend fun drain(limitMs: Long) {
        withTimeoutOrNull(limitMs) { count.first { it == 0 } }
    }
}

/** Holds one slot of a [Counter] until closed. */
class Slot(private val counter: Counter) : Closeable {
    private val released = AtomicBoolean()

    override fun close() {
        if (released.compareAndSet(false, true)) {
            while (true) {
                val now = counter.count.value
                if (counter.count.compareAndSet(now, now - 1)) return
            }
        }
    }
}

/** Pumps bytes both ways until either side closes, then closes the other. */
suspend fun bridge(socket: Socket, channel: RtcChannel) {
    try {
        socket.tcpNoDelay = true
        coroutineScope {
            val done = CompletableDeferred<Unit>()
            val writing = AtomicLong(0)

            // Data channel to TCP. Ends when the channel closes or the socket fails.
            val inbound = launch(Dispatchers.IO) {
                runCatching {
                    val output = socket.getOutputStream()
                    while (true) {
                        val data = channel.receive() ?: break
                        writing.set(System.nanoTime())
                        output.write(data)
                        writing.set(0)
                    }
                    output.flush()
                    socket.shutdownOutput()
                }
                done.complete(Unit)
            }
            // Blocking writes cannot time out by themselves.
            val watchdog = launch {
                while (true) {
                    delay(1_000)
                    val since = writing.get()
                    if (since != 0L && System.nanoTime() - since > WRITE_STALL_TIMEOUT_MS * 1_000_000) {
                        done.complete(Unit)
                        break
                    }
                }
            }
            // TCP to data channel. Ends at end of stream or when the channel stops taking data.
            val outbound = launch(Dispatchers.IO) {
                runCatching {
                    channel.awaitOpen()
                    val input = socket.getInputStream()
                    val buffer = ByteArray(MAX_CHUNK)
                    while (true) {
                        waitForCapacity(channel)
                        val read = input.read(buffer)
                        if (read < 0) break
                        if (!channel.send(buffer.copyOf(read))) break
                    }
                    flush(channel)
                }
                done.complete(Unit)
            }

            done.await()
            // A blocked read or write only lets go once the socket is closed.
            runCatching { socket.close() }
            inbound.cancel()
            outbound.cancel()
            watchdog.cancel()
        }
    } finally {
        runCatching { socket.close() }
        channel.close()
    }
}

/** Holds TCP reads back while too much is unacknowledged. */
private suspend fun waitForCapacity(channel: RtcChannel) {
    if (channel.bufferedAmount() <= BUFFERED_HIGH) return
    while (channel.bufferedAmount() > BUFFERED_LOW) {
        channel.awaitBufferChange(CAPACITY_RECHECK_MS)
    }
}

/** Waits until everything was handed on, because closing may abandon what is queued. The last
 *  bytes matter: a server's disconnect message is usually followed directly by its FIN. */
private suspend fun flush(channel: RtcChannel) {
    withTimeoutOrNull(FLUSH_TIMEOUT_MS) {
        while (channel.bufferedAmount() > 0) delay(FLUSH_RECHECK_MS)
    }
}
