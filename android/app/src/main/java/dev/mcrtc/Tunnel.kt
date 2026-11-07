package dev.mcrtc

import android.content.Context
import android.content.pm.ApplicationInfo
import android.util.Log
import dev.mcrtc.core.EventListener
import dev.mcrtc.core.MobileEvent
import dev.mcrtc.core.MobileException
import dev.mcrtc.core.MobileJoin
import dev.mcrtc.core.initLogging
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlin.concurrent.thread

/**
 * The app's single connection to the Rust core, and the state the Activity and [TunnelService]
 * both watch. The core lives as long as the process, because the Join screen needs its ID before
 * any session starts; only [TunnelService] starts sessions on it.
 */
object Tunnel {
    private const val TAG = "mcrtc-app"

    private val mutableState = MutableStateFlow(UiState())
    val state: StateFlow<UiState> = mutableState.asStateFlow()

    private val coreLock = Any()

    @Volatile
    private var core: MobileJoin? = null

    /** The core, created on first use. Loads the native library: never call on the main thread. */
    @Throws(MobileException::class)
    fun core(context: Context): MobileJoin = core ?: synchronized(coreLock) {
        core ?: run {
            val app = context.applicationContext
            initLogging(verbose = app.applicationInfo.flags and ApplicationInfo.FLAG_DEBUGGABLE != 0)
            MobileJoin(app.filesDir.path, Listener).also { created ->
                core = created
                update { it.copy(myId = created.myId()) }
            }
        }
    }

    /** Creates the core in the background so the Join screen can show the ID. */
    fun preload(context: Context) {
        if (core != null) return
        val app = context.applicationContext
        thread(name = "mcrtc-preload") {
            try {
                core(app)
            } catch (e: MobileException) {
                Log.e(TAG, "Could not start the mcrtc core", e)
                update { it.copy(myId = "Unavailable: ${e.reason()}") }
            } catch (e: UnsatisfiedLinkError) {
                Log.e(TAG, "Could not load the mcrtc core", e)
                update { it.copy(myId = "Unavailable: this device is not supported") }
            }
        }
    }

    fun setInvite(text: String) = update { it.copy(invite = text) }

    /** Connect: starts a session, or answers the running one that is waiting for an invite. */
    fun connect(context: Context) {
        val current = state.value
        when {
            !current.running -> {
                update { it.starting() }
                TunnelService.start(context)
            }
            current.awaitingInvite -> send(transition { it.sendingInvite() })
        }
    }

    /** Ends the session; SessionEnded follows. Never blocks. */
    fun stop() {
        core?.stop()
    }

    fun startOver() = update { it.startOver() }

    internal fun startFailed(reason: String) = update { it.startFailed(reason) }

    /** Stops any session and waits for it to say goodbye. Blocks for up to 8 seconds. */
    internal fun shutDown() {
        core?.shutDown()
    }

    private object Listener : EventListener {
        override fun onEvent(event: MobileEvent) {
            // Only the kind: invites and replies hold IP addresses.
            Log.d(TAG, "event: ${event::class.simpleName}")
            send(transition { reduce(it, event) })
        }
    }

    private fun send(line: String?) {
        if (line == null) return
        try {
            core?.send(line)
        } catch (e: MobileException) {
            update {
                it.copy(
                    screen = Screen.Join,
                    pasteError = e.reason(),
                    inviteSent = false,
                    awaitingInvite = true,
                )
            }
        }
    }

    private fun update(transform: (UiState) -> UiState) {
        transition { Step(transform(it)) }
    }

    /** Applies one step atomically, so events and button presses never overwrite each other. */
    private fun transition(step: (UiState) -> Step): String? = synchronized(mutableState) {
        val next = step(mutableState.value)
        mutableState.value = next.state
        next.send
    }
}

fun MobileException.reason(): String = when (this) {
    is MobileException.Failed -> reason
}
