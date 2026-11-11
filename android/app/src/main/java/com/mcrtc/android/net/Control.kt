// The `control` data channel: heartbeat, a clean goodbye, and setting up tunnel channels. A port
// of `src/control.rs`; both ends must speak exactly the same words.

package com.mcrtc.android.net

import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.atomic.AtomicLong
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch
import kotlinx.coroutines.withTimeoutOrNull

const val CONTROL_LABEL = "control"

private const val PING = "ping"
private const val PONG = "pong"
private const val BYE = "bye"
private const val OPEN = "open"
private const val OPEN_DATAGRAMS = "open-datagrams"
private const val READY = "ready"
private const val REFUSED = "refused"

private const val PING_INTERVAL_MS = 5_000L

/** Longer than the bridge's write stall timeout, since a stalled stream delays this channel too. */
private const val PEER_TIMEOUT_MS = 45_000L

private const val BYE_FLUSH_TIMEOUT_MS = 1_000L

/** Covers the host dialing the Minecraft server. */
private const val OPEN_TIMEOUT_MS = 15_000L

/** How many stream IDs tunnel channels rotate through: even IDs from 2 up, all below libwebrtc's
 *  limit of 1024 streams. Must match `STREAM_ID_COUNT` in `src/control.rs`. */
private const val STREAM_ID_COUNT = 500L

/** The stream ID of tunnel connection `id`. */
fun streamId(id: Long): Int = (2 + 2 * (id % STREAM_ID_COUNT)).toInt()

fun channelLabel(id: Long) = "mc-$id"

/** What a tunnel connection carries, which decides how the host reaches the server. */
enum class Transport(val word: String) {
    Stream(OPEN),
    Datagrams(OPEN_DATAGRAMS),
}

/** The joiner's open requests still waiting for an answer. */
class PendingOpens {
    private val waiting = ConcurrentHashMap<Long, CompletableDeferred<Boolean>>()

    fun resolve(id: Long, accepted: Boolean) {
        waiting.remove(id)?.complete(accepted)
    }

    /** Asks the host to open its end of tunnel connection `id`. False means no. */
    suspend fun request(control: RtcChannel, id: Long, transport: Transport): Boolean {
        val answer = CompletableDeferred<Boolean>()
        waiting[id] = answer
        try {
            if (!control.sendText("${transport.word} $id")) return false
            return withTimeoutOrNull(OPEN_TIMEOUT_MS) { answer.await() } ?: false
        } finally {
            waiting.remove(id)
        }
    }
}

/** The host's answer to an `open`. */
fun answerOpen(control: RtcChannel, id: Long, accepted: Boolean) {
    control.sendText("${if (accepted) READY else REFUSED} $id")
}

enum class ControlEnd {
    /** The other side said goodbye. */
    PeerLeft,

    /** The channel closed without a goodbye. */
    Closed,

    /** The other side went silent. */
    TimedOut,

    /** We are shutting down and said goodbye. */
    Shutdown,
}

/** What this side does with tunnel messages. */
sealed interface Role {
    /** Called for each `open`. Must not block. */
    class Host(val onOpen: (Long, Transport) -> Unit) : Role
    class Joiner(val pending: PendingOpens) : Role

    fun handle(message: String) {
        val word = message.substringBefore(' ', "")
        val id = message.substringAfter(' ', "").toLongOrNull() ?: return
        when {
            this is Host && word == OPEN -> onOpen(id, Transport.Stream)
            this is Host && word == OPEN_DATAGRAMS -> onOpen(id, Transport.Datagrams)
            this is Joiner && word == READY -> pending.resolve(id, true)
            this is Joiner && word == REFUSED -> pending.resolve(id, false)
        }
    }
}

/** Serves the control channel until the session ends. Says goodbye when `shutdown` turns true. */
suspend fun runControl(
    channel: RtcChannel,
    role: Role,
    shutdown: StateFlow<Boolean>,
    rtt: MutableStateFlow<Long?>,
): ControlEnd = coroutineScope {
    val end = CompletableDeferred<ControlEnd>()
    val lastHeard = AtomicLong(System.nanoTime())
    val pingSent = AtomicLong(0)

    val stopper = launch {
        shutdown.first { it }
        sayGoodbye(channel)
        end.complete(ControlEnd.Shutdown)
    }
    val pinger = launch {
        while (true) {
            if (System.nanoTime() - lastHeard.get() > PEER_TIMEOUT_MS * 1_000_000) {
                end.complete(ControlEnd.TimedOut)
                break
            }
            // Failing here only means the channel is not open yet.
            if (channel.sendText(PING)) pingSent.set(System.nanoTime())
            delay(PING_INTERVAL_MS)
        }
    }
    val reader = launch {
        while (true) {
            val data = channel.receive() ?: break
            lastHeard.set(System.nanoTime())
            when (val message = String(data)) {
                PING -> channel.sendText(PONG)
                BYE -> {
                    end.complete(ControlEnd.PeerLeft)
                    return@launch
                }
                PONG -> {
                    val sent = pingSent.getAndSet(0)
                    if (sent != 0L) rtt.value = (System.nanoTime() - sent) / 1_000_000
                }
                else -> role.handle(message)
            }
        }
        end.complete(ControlEnd.Closed)
    }

    val result = end.await()
    stopper.cancel()
    pinger.cancel()
    reader.cancel()
    result
}

private suspend fun sayGoodbye(channel: RtcChannel) {
    if (!channel.sendText(BYE)) return
    withTimeoutOrNull(BYE_FLUSH_TIMEOUT_MS) {
        while (channel.bufferedAmount() > 0) delay(10)
    }
}
