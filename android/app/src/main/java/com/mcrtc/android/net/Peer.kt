// PeerConnection setup, non-trickle offer/answer helpers and connection monitoring. A port of
// `src/peer.rs` onto libwebrtc.

package com.mcrtc.android.net

import android.content.Context
import kotlin.coroutines.resume
import kotlin.coroutines.resumeWithException
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlinx.coroutines.withTimeoutOrNull
import org.webrtc.DataChannel
import com.mcrtc.android.BuildConfig
import org.webrtc.IceCandidate
import org.webrtc.Logging
import org.webrtc.MediaConstraints
import org.webrtc.MediaStream
import org.webrtc.PeerConnection
import org.webrtc.PeerConnection.IceGatheringState
import org.webrtc.PeerConnection.PeerConnectionState
import org.webrtc.PeerConnectionFactory
import org.webrtc.RTCStatsReport
import org.webrtc.SdpObserver
import org.webrtc.SessionDescription

/** How long to wait for candidates. A dead STUN server would otherwise hold up the invite for
 *  its full retransmission schedule, and the candidates found so far are usually enough. */
private const val GATHER_TIMEOUT_MS = 10_000L

const val FAILURE_HINT =
    "Connection failed. Probably strict NAT. Add a TURN server in Options."

val DEFAULT_STUN = listOf("stun:stun.l.google.com:19302", "stun:stun.cloudflare.com:3478")

data class TurnServer(val url: String, val username: String, val password: String)

data class IceConfig(val stun: List<String> = DEFAULT_STUN, val turn: List<TurnServer> = emptyList())

object Rtc {
    private var factory: PeerConnectionFactory? = null

    @Synchronized
    fun factory(context: Context): PeerConnectionFactory = factory ?: run {
        PeerConnectionFactory.initialize(
            PeerConnectionFactory.InitializationOptions.builder(context.applicationContext)
                .createInitializationOptions()
        )
        if (BuildConfig.DEBUG) Logging.enableLogToDebugOutput(Logging.Severity.LS_INFO)
        PeerConnectionFactory.builder().createPeerConnectionFactory().also { factory = it }
    }
}

/** One remote friend. `onChannel` runs on a libwebrtc thread, so it must return immediately. */
class Peer(factory: PeerConnectionFactory, ice: IceConfig, onChannel: (RtcChannel) -> Unit) {
    val state = MutableStateFlow(PeerConnectionState.NEW)
    private val gathered = MutableStateFlow(false)
    private val channels = mutableListOf<RtcChannel>()
    private var disposed = false

    private val pc: PeerConnection

    init {
        val servers = buildList {
            if (ice.stun.isNotEmpty()) add(PeerConnection.IceServer.builder(ice.stun).createIceServer())
            for (turn in ice.turn) {
                add(
                    PeerConnection.IceServer.builder(turn.url)
                        .setUsername(turn.username)
                        .setPassword(turn.password)
                        .createIceServer()
                )
            }
        }
        val configuration = PeerConnection.RTCConfiguration(servers).apply {
            sdpSemantics = PeerConnection.SdpSemantics.UNIFIED_PLAN
            // The desktop side only gathers UDP, and gathering once lets the state reach complete.
            tcpCandidatePolicy = PeerConnection.TcpCandidatePolicy.DISABLED
            continualGatheringPolicy = PeerConnection.ContinualGatheringPolicy.GATHER_ONCE
        }
        val observer = object : PeerConnection.Observer {
            override fun onIceGatheringChange(state: IceGatheringState) {
                if (state == IceGatheringState.COMPLETE) gathered.value = true
            }

            override fun onConnectionChange(newState: PeerConnectionState) {
                state.value = newState
            }

            override fun onDataChannel(channel: DataChannel) {
                onChannel(track(RtcChannel(channel)))
            }

            override fun onSignalingChange(state: PeerConnection.SignalingState) = Unit
            override fun onIceConnectionChange(state: PeerConnection.IceConnectionState) = Unit
            override fun onIceConnectionReceivingChange(receiving: Boolean) = Unit
            override fun onIceCandidate(candidate: IceCandidate) = Unit
            override fun onIceCandidatesRemoved(candidates: Array<out IceCandidate>) = Unit
            override fun onAddStream(stream: MediaStream) = Unit
            override fun onRemoveStream(stream: MediaStream) = Unit
            override fun onRenegotiationNeeded() = Unit
        }
        pc = factory.createPeerConnection(configuration, observer)
            ?: fail("could not create the peer connection")
    }

    private fun track(channel: RtcChannel): RtcChannel {
        synchronized(channels) { channels += channel }
        return channel
    }

    fun createChannel(label: String): RtcChannel {
        val channel = pc.createDataChannel(label, DataChannel.Init())
            ?: fail("could not create data channel $label")
        return track(RtcChannel(channel))
    }

    /** A channel both sides create themselves with the same stream ID. */
    fun createNegotiatedChannel(label: String, streamId: Int): RtcChannel {
        val init = DataChannel.Init().apply {
            negotiated = true
            id = streamId
        }
        val channel = pc.createDataChannel(label, init)
            ?: fail("could not create data channel $label")
        return track(RtcChannel(channel))
    }

    /** Creates the offer and returns its SDP once gathering finished, candidates included. */
    suspend fun createOffer(): String {
        val offer = create { observer -> pc.createOffer(observer, MediaConstraints()) }
            ?: fail("could not create the offer")
        apply("could not apply the offer") { observer -> pc.setLocalDescription(observer, offer) }
        return gatheredSdp()
    }

    /** Applies a remote offer and returns the answer SDP once gathering finished. */
    suspend fun acceptOffer(sdp: String): String {
        val offer = SessionDescription(SessionDescription.Type.OFFER, sdp)
        apply("could not apply the invite") { observer -> pc.setRemoteDescription(observer, offer) }
        val answer = create { observer -> pc.createAnswer(observer, MediaConstraints()) }
            ?: fail("could not create the reply")
        apply("could not apply the reply") { observer -> pc.setLocalDescription(observer, answer) }
        return gatheredSdp()
    }

    suspend fun acceptAnswer(sdp: String) {
        val answer = SessionDescription(SessionDescription.Type.ANSWER, sdp)
        apply("could not apply the reply") { observer -> pc.setRemoteDescription(observer, answer) }
    }

    private suspend fun gatheredSdp(): String {
        withTimeoutOrNull(GATHER_TIMEOUT_MS) { gathered.first { it } }
        val description = pc.localDescription ?: fail("no local description after gathering")
        return trimCandidates(description.description)
    }

    /**
     * Waits for the connection to come up, reporting failure with a NAT hint. libwebrtc can call
     * a connection failed while the other side has not pasted its string yet, and recovers once
     * that side starts checking, so only a close or the deadline ends the wait.
     */
    suspend fun waitConnected(limitMs: Long) {
        val reached = withTimeoutOrNull(limitMs) {
            state.first { it == PeerConnectionState.CONNECTED || it == PeerConnectionState.CLOSED }
        }
        when (reached) {
            PeerConnectionState.CONNECTED -> Unit
            PeerConnectionState.CLOSED -> fail("the connection was closed before it was established")
            else -> fail(FAILURE_HINT)
        }
    }

    /** Resolves once the connection is gone for good. */
    suspend fun waitLost(): PeerConnectionState =
        state.first { it == PeerConnectionState.FAILED || it == PeerConnectionState.CLOSED }

    /** How the connection is routed, like `host <-> srflx (direct)`, available once connected. */
    suspend fun route(): String? {
        val report = suspendCancellableCoroutine<RTCStatsReport> { continuation ->
            pc.getStats { report -> continuation.resume(report) }
        }
        val stats = report.statsMap
        val pairId = stats.values.firstOrNull { it.type == "transport" }
            ?.members?.get("selectedCandidatePairId") as? String
        val pair = stats[pairId] ?: stats.values.firstOrNull {
            it.type == "candidate-pair" && it.members["nominated"] == true && it.members["state"] == "succeeded"
        } ?: return null
        val kind = { id: Any? -> stats[id as? String]?.members?.get("candidateType") as? String }
        val local = kind(pair.members["localCandidateId"]) ?: return null
        val remote = kind(pair.members["remoteCandidateId"]) ?: return null
        val path = if (local == "relay" || remote == "relay") "relayed through TURN" else "direct"
        return "$local <-> $remote ($path)"
    }

    fun close() {
        synchronized(this) {
            if (disposed) return
            pc.close()
        }
        // A local close stops the observer before it can report this itself.
        state.value = PeerConnectionState.CLOSED
    }

    /** Frees the native connection. Nothing may use this peer or its channels afterwards. */
    fun dispose() {
        close()
        synchronized(this) {
            if (disposed) return
            disposed = true
            synchronized(channels) { channels.forEach { it.release() } }
            pc.dispose()
        }
    }

    private suspend fun create(start: (SdpObserver) -> Unit): SessionDescription? =
        suspendCancellableCoroutine { continuation ->
            start(object : SdpObserver {
                override fun onCreateSuccess(description: SessionDescription) =
                    continuation.resume(description)

                override fun onCreateFailure(error: String?) =
                    continuation.resumeWithException(MessageException(error ?: "unknown error"))

                override fun onSetSuccess() = Unit
                override fun onSetFailure(error: String?) = Unit
            })
        }

    private suspend fun apply(context: String, start: (SdpObserver) -> Unit) =
        suspendCancellableCoroutine { continuation ->
            start(object : SdpObserver {
                override fun onSetSuccess() = continuation.resume(Unit)
                override fun onSetFailure(error: String?) =
                    continuation.resumeWithException(MessageException("$context: $error"))

                override fun onCreateSuccess(description: SessionDescription) = Unit
                override fun onCreateFailure(error: String?) = Unit
            })
        }
}
