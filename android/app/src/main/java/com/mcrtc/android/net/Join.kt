// Join mode: the local listener, one data channel per connection. A port of `src/join.rs`, Java
// Edition only.

package com.mcrtc.android.net

import java.io.IOException
import java.net.InetSocketAddress
import java.net.ServerSocket
import java.net.Socket
import java.util.concurrent.atomic.AtomicLong
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancelAndJoin
import kotlinx.coroutines.channels.ReceiveChannel
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.isActive
import kotlinx.coroutines.job
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeoutOrNull
import org.webrtc.PeerConnectionFactory

/** The host needs time to receive and paste the reply, so this is as long as a reply stays valid. */
private const val CONNECT_TIMEOUT_MS = DEFAULT_TTL_MS
private const val CONTROL_TIMEOUT_MS = 30_000L
private const val SHUTDOWN_TIMEOUT_MS = 5_000L

class JoinOptions(
    val factory: PeerConnectionFactory,
    val ice: IceConfig,
    val listen: String,
    val report: Reporter,
)

/** The join side of an established session. */
private class JoinTunnel(
    val peer: Peer,
    val control: RtcChannel,
    val pending: PendingOpens,
    val connections: Counter,
) {
    private val nextId = AtomicLong()

    /** Accepts connections until the listener is closed, bridging each over a new channel. The
     *  bridges live on until their connection or the peer connection ends. */
    suspend fun serve(listener: ServerSocket, scope: CoroutineScope) = withContext(Dispatchers.IO) {
        while (isActive && !listener.isClosed) {
            val socket = try {
                listener.accept()
            } catch (_: IOException) {
                if (listener.isClosed) break
                delay(100)
                continue
            }
            val id = nextId.getAndIncrement()
            scope.launch { connect(socket, id) }
        }
    }

    private suspend fun connect(socket: Socket, id: Long) {
        connections.acquire().use {
            val label = channelLabel(id)
            val channel = runCatching { peer.createNegotiatedChannel(label, streamId(id)) }.getOrNull()
            if (channel == null) {
                socket.close()
                return
            }
            if (pending.request(control, id, Transport.Stream)) {
                bridge(socket, channel)
            } else {
                socket.close()
                channel.close()
            }
        }
    }
}

suspend fun runJoin(options: JoinOptions, input: ReceiveChannel<String>, shutdown: StateFlow<Boolean>) {
    val report = options.report
    // Bind first: finding out the port is taken is better before the invite dance than after it.
    val listener = withContext(Dispatchers.IO) {
        try {
            val (host, port) = splitAddress(options.listen, JAVA_PORT)
            ServerSocket().apply {
                reuseAddress = true
                bind(InetSocketAddress(host, port))
            }
        } catch (_: IOException) {
            fail(
                "could not listen on ${options.listen}. Is something already using it? If the " +
                    "Minecraft server runs on this phone, pick another local address such as " +
                    "127.0.0.1:25566"
            )
        }
    }

    try {
        val sdp = readInvite(input, shutdown, report) ?: return

        val control = CompletableDeferred<RtcChannel>()
        val peer = Peer(options.factory, options.ice) { channel ->
            // Hands the host's `control` channel over and refuses anything else.
            if (channel.label == CONTROL_LABEL) control.complete(channel) else channel.close()
        }
        try {
            joined(options, peer, sdp, control, listener, shutdown)
        } finally {
            peer.dispose()
        }
    } finally {
        withContext(Dispatchers.IO) { runCatching { listener.close() } }
    }
}

private suspend fun joined(
    options: JoinOptions,
    peer: Peer,
    sdp: String,
    controlReady: CompletableDeferred<RtcChannel>,
    listener: ServerSocket,
    shutdown: StateFlow<Boolean>,
) {
    val report = options.report
    val established = untilStopped(shutdown) {
        report(Event.Gathering)
        val answer = peer.acceptOffer(sdp)
        val reply = Payload.create(PayloadType.Answer, answer, Edition.Java).encode()
        report(Event.Reply(reply))
        report(Event.WaitingForHost)

        peer.waitConnected(CONNECT_TIMEOUT_MS)
        val route = peer.route()
        val control = withTimeoutOrNull(CONTROL_TIMEOUT_MS) { controlReady.await() }
            ?: fail("the host never opened its control channel")
        control to route
    } ?: return
    val (control, route) = established

    val address = "${listener.inetAddress.hostAddress}:${listener.localPort}"
    report(Event.Joined(address, route))
    val connections = Counter()
    val pending = PendingOpens()
    val tunnel = JoinTunnel(peer, control, pending, connections)

    val end = coroutineScope {
        val bridges = CoroutineScope(coroutineContext + SupervisorJob(coroutineContext.job))
        val server = launch { tunnel.serve(listener, bridges) }
        val rtt = MutableStateFlow<Long?>(null)
        val stats = launch { reportStats("host", connections, rtt, report) }
        val end = race(
            { runControl(control, Role.Joiner(pending), shutdown, rtt) },
            { peer.waitLost(); ControlEnd.Closed },
        )

        stats.cancel()
        withContext(Dispatchers.IO) { runCatching { listener.close() } }
        server.cancelAndJoin()
        peer.close()
        connections.drain(SHUTDOWN_TIMEOUT_MS)
        bridges.coroutineContext.job.cancelAndJoin()
        end
    }

    when (end) {
        ControlEnd.PeerLeft -> report(Event.Left("The host closed the connection."))
        ControlEnd.Shutdown -> report(Event.Left("Disconnected."))
        ControlEnd.Closed -> fail("the connection to the host was lost")
        ControlEnd.TimedOut -> fail("the host stopped responding")
    }
}

/** Asks until a valid invite arrives. `null` means stop. */
private suspend fun readInvite(
    input: ReceiveChannel<String>,
    shutdown: StateFlow<Boolean>,
    report: Reporter,
): String? {
    while (true) {
        report(Event.AskInvite)
        val paste = untilStopped(shutdown) { input.readLine() } ?: return null
        try {
            val payload = Payload.parse(paste, PayloadType.Offer)
            payload.checkEdition(Edition.Java)
            return payload.sdp
        } catch (error: MessageException) {
            report(Event.BadPaste(errorText(error)))
        }
    }
}
