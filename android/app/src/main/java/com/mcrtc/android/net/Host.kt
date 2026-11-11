// Host mode: the invite loop, per-friend supervision and reaching the Minecraft server. A port
// of `src/host.rs`, Java Edition only.

package com.mcrtc.android.net

import java.net.InetSocketAddress
import java.net.Socket
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancelAndJoin
import kotlinx.coroutines.channels.ReceiveChannel
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.job
import kotlinx.coroutines.joinAll
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeoutOrNull
import org.webrtc.PeerConnectionFactory

private const val CONNECT_TIMEOUT_MS = 30_000L
private const val DIAL_TIMEOUT_MS = 5_000
private const val SHUTDOWN_TIMEOUT_MS = 5_000L
const val JAVA_PORT = 25565

class HostOptions(
    val factory: PeerConnectionFactory,
    val ice: IceConfig,
    val server: String,
    val maxPeers: Int?,
    val maxConnections: Int?,
    val report: Reporter,
)

/** Turns the friend's `open` requests into connections to the Minecraft server. */
private class Tunnel(
    val peer: Peer,
    val control: RtcChannel,
    val server: String,
    val maxConnections: Int?,
    val label: String,
    val connections: Counter,
) {
    fun role(scope: CoroutineScope) = Role.Host { id, transport ->
        scope.launch { serve(id, transport) }
    }

    private suspend fun serve(id: Long, transport: Transport) {
        if (transport == Transport.Datagrams) {
            // Java has no datagrams.
            answerOpen(control, id, false)
            return
        }
        val slot = connections.tryAcquire(maxConnections)
        if (slot == null) {
            answerOpen(control, id, false)
            return
        }
        slot.use {
            val socket = dial()
            val channel = socket?.let {
                runCatching { peer.createNegotiatedChannel(channelLabel(id), streamId(id)) }.getOrNull()
            }
            if (socket == null || channel == null) {
                socket?.close()
                answerOpen(control, id, false)
                return
            }
            answerOpen(control, id, true)
            bridge(socket, channel)
        }
    }

    private suspend fun dial(): Socket? = withContext(Dispatchers.IO) {
        runCatching {
            val (host, port) = splitAddress(server, JAVA_PORT)
            Socket().apply { connect(InetSocketAddress(host, port), DIAL_TIMEOUT_MS) }
        }.getOrNull()
    }
}

private class Session(val tunnel: Tunnel, val slot: Slot)

/** Reports connection counts and ping until cancelled. */
suspend fun reportStats(label: String, connections: Counter, rtt: StateFlow<Long?>, report: Reporter) {
    combine(connections.count, rtt) { count, ping -> Event.Stats(label, count, ping) }
        .collect { report(it) }
}

/** Runs an established friend until they leave. */
private suspend fun supervise(session: Session, shutdown: StateFlow<Boolean>, report: Reporter) {
    val tunnel = session.tunnel
    val rtt = MutableStateFlow<Long?>(null)
    try {
        coroutineScope {
            val bridges = CoroutineScope(coroutineContext + SupervisorJob(coroutineContext.job))
            val stats = launch { reportStats(tunnel.label, tunnel.connections, rtt, report) }
            val reason = race(
                {
                    when (runControl(tunnel.control, tunnel.role(bridges), shutdown, rtt)) {
                        ControlEnd.PeerLeft -> "left"
                        ControlEnd.Closed -> "disconnected"
                        ControlEnd.TimedOut -> "timed out"
                        ControlEnd.Shutdown -> null
                    }
                },
                { "disconnected (${tunnel.peer.waitLost().name.lowercase()})" },
            )
            stats.cancel()
            if (reason != null) report(Event.FriendLeft(tunnel.label, reason))
            tunnel.peer.close()
            // Closing the connection ends every bridge; give them a moment to be counted as gone.
            tunnel.connections.drain(SHUTDOWN_TIMEOUT_MS)
            bridges.coroutineContext.job.cancelAndJoin()
        }
    } finally {
        session.slot.close()
        tunnel.peer.dispose()
    }
}

/** What came of the attempt to bring in one friend. */
private sealed interface Attempt {
    class Connected(val session: Session, val route: String?) : Attempt
    data object Cancelled : Attempt
    data object InputClosed : Attempt
}

suspend fun runHost(options: HostOptions, input: ReceiveChannel<String>, shutdown: StateFlow<Boolean>) {
    val report = options.report
    report(Event.Hosting(options.server))
    val peers = Counter()
    val friends = mutableListOf<Job>()
    coroutineScope {
        var friend = 1
        while (true) {
            friends.removeAll { it.isCompleted }
            val open = untilStopped(shutdown) {
                nextFriend(options, peers, input, friend) { session ->
                    friends += launch { supervise(session, shutdown, report) }
                }
            } ?: break
            if (!open) {
                shutdown.first { it }
                break
            }
            friend += 1
        }

        report(Event.ShuttingDown)
        if (withTimeoutOrNull(SHUTDOWN_TIMEOUT_MS) { friends.joinAll() } == null) {
            friends.forEach { it.cancel() }
        }
    }
}

/** One round of the invite loop. Returns false when the input is gone. */
private suspend fun nextFriend(
    options: HostOptions,
    peers: Counter,
    input: ReceiveChannel<String>,
    friend: Int,
    start: (Session) -> Unit,
): Boolean {
    val report = options.report
    options.maxPeers?.let { limit ->
        if (peers.get() >= limit) {
            report(Event.WaitingForSlot(limit))
            peers.waitBelow(limit)
        }
    }

    val defaultLabel = "friend-$friend"
    report(Event.AskName(friend, defaultLabel))
    val name = input.readLine() ?: return false
    val label = name.trim().ifEmpty { defaultLabel }

    val attempt = try {
        invite(options, peers, input, label)
    } catch (error: CancellationException) {
        throw error
    } catch (error: Exception) {
        report(Event.ConnectFailed(errorText(error)))
        null
    }
    when (attempt) {
        is Attempt.Connected -> {
            report(Event.FriendConnected(attempt.session.tunnel.label, attempt.route))
            start(attempt.session)
        }
        Attempt.Cancelled -> report(Event.Cancelled)
        Attempt.InputClosed -> return false
        null -> Unit
    }

    report(Event.AskNext)
    return input.readLine() != null
}

private suspend fun invite(
    options: HostOptions,
    peers: Counter,
    input: ReceiveChannel<String>,
    label: String,
): Attempt {
    val report = options.report
    // Friends announce connections on the control channel, so nothing else should show up here.
    val peer = Peer(options.factory, options.ice) { channel -> channel.close() }
    var handedOver = false
    try {
        val control = peer.createChannel(CONTROL_LABEL)

        report(Event.Gathering)
        val sdp = peer.createOffer()
        val invite = Payload.create(PayloadType.Offer, sdp, Edition.Java).encode()
        report(Event.Invite(label, invite, DEFAULT_TTL_MS / 60_000))

        val answer = when (val reply = readReply(options, input, label)) {
            is Reply.Sdp -> reply.sdp
            Reply.Cancelled -> return Attempt.Cancelled
            Reply.InputClosed -> return Attempt.InputClosed
        }
        report(Event.Connecting(label))
        peer.acceptAnswer(answer)
        peer.waitConnected(CONNECT_TIMEOUT_MS)
        val route = peer.route()

        val tunnel = Tunnel(peer, control, options.server, options.maxConnections, label, Counter())
        handedOver = true
        return Attempt.Connected(Session(tunnel, peers.acquire()), route)
    } finally {
        if (!handedOver) peer.dispose()
    }
}

private sealed interface Reply {
    class Sdp(val sdp: String) : Reply
    data object Cancelled : Reply
    data object InputClosed : Reply
}

/** Asks until a valid reply arrives, so a bad paste does not cost the whole invite. */
private suspend fun readReply(options: HostOptions, input: ReceiveChannel<String>, label: String): Reply {
    val report = options.report
    val deadline = System.currentTimeMillis() + DEFAULT_TTL_MS
    while (true) {
        report(Event.AskReply(label))
        val remaining = deadline - System.currentTimeMillis()
        val paste = withTimeoutOrNull(remaining.coerceAtLeast(1)) { input.receiveCatching() }
        if (paste == null) {
            report(Event.InviteExpired)
            return Reply.Cancelled
        }
        val text = paste.getOrNull() ?: return Reply.InputClosed
        if (text.trim().equals("cancel", ignoreCase = true)) return Reply.Cancelled
        try {
            val payload = Payload.parse(text, PayloadType.Answer)
            payload.checkEdition(Edition.Java)
            return Reply.Sdp(payload.sdp)
        } catch (error: MessageException) {
            report(Event.BadPaste(errorText(error)))
        }
    }
}
