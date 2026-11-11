// Runs at most one host or join session for the whole process, so it outlives the activity while
// the player is in Minecraft. Mirrors `src/ui/session.rs`.

package com.mcrtc.android

import android.content.Context
import android.os.Handler
import android.os.Looper
import android.util.Log
import com.mcrtc.android.net.DEFAULT_STUN
import com.mcrtc.android.net.Event
import com.mcrtc.android.net.HostOptions
import com.mcrtc.android.net.IceConfig
import com.mcrtc.android.net.JAVA_PORT
import com.mcrtc.android.net.JoinOptions
import com.mcrtc.android.net.Rtc
import com.mcrtc.android.net.TurnServer
import com.mcrtc.android.net.errorText
import com.mcrtc.android.net.runHost
import com.mcrtc.android.net.runJoin
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.launch

const val DEFAULT_ADDRESS = "127.0.0.1:$JAVA_PORT"

private const val PREFERENCES = "forms"

object Sessions {
    val state = State()
    private val main = Handler(Looper.getMainLooper())
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
    private val listeners = mutableListOf<() -> Unit>()
    private var running: Running? = null
    private var loaded = false

    private class Running(val input: Channel<String>, val stop: MutableStateFlow<Boolean>)

    val isRunning: Boolean get() = running != null

    /** Main thread only, like everything else here that is not marked otherwise. */
    fun listen(listener: () -> Unit) {
        listeners += listener
    }

    fun unlisten(listener: () -> Unit) {
        listeners -= listener
    }

    fun changed() = listeners.toList().forEach { it() }

    fun load(context: Context) {
        if (loaded) return
        loaded = true
        val saved = context.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE)
        with(state.forms) {
            server = saved.getString("server", "")!!
            listen = saved.getString("listen", "")!!
            stun = saved.getString("stun", "")!!
            turnUrl = saved.getString("turn_url", "")!!
            turnUser = saved.getString("turn_user", "")!!
            turnPass = saved.getString("turn_pass", "")!!
        }
    }

    fun save(context: Context) {
        with(state.forms) {
            context.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE).edit()
                .putString("server", server)
                .putString("listen", listen)
                .putString("stun", stun)
                .putString("turn_url", turnUrl)
                .putString("turn_user", turnUser)
                .putString("turn_pass", turnPass)
                .apply()
        }
    }

    private fun ice(): IceConfig {
        val forms = state.forms
        val stun = forms.stun.split(Regex("[\\s,]+")).filter { it.isNotBlank() }
        val turn = forms.turnUrl.trim().takeIf { it.isNotEmpty() }
            ?.let { listOf(TurnServer(it, forms.turnUser, forms.turnPass)) }
            .orEmpty()
        return IceConfig(stun.ifEmpty { DEFAULT_STUN }, turn)
    }

    fun start(context: Context, mode: Mode): String? {
        if (running != null) return "a session is already running"
        val app = context.applicationContext
        val factory = try {
            Rtc.factory(app)
        } catch (error: Throwable) {
            return errorText(error)
        }
        // Read on this thread: the forms belong to the screens.
        val forms = state.forms
        val ice = ice()
        val server = forms.server.trim().ifEmpty { DEFAULT_ADDRESS }
        val listen = forms.listen.trim().ifEmpty { DEFAULT_ADDRESS }
        val maxPeers = positive(forms.maxPeers)
        val maxConnections = positive(forms.maxConnections)
        val input = Channel<String>(16)
        val stop = MutableStateFlow(false)
        val session = Running(input, stop)
        running = session
        TunnelService.start(app, mode)

        val report: (Event) -> Unit = { event -> main.post { apply(event) } }
        scope.launch {
            val error = try {
                when (mode) {
                    Mode.Host -> runHost(
                        HostOptions(factory, ice, server, maxPeers, maxConnections, report),
                        input, stop,
                    )
                    Mode.Join -> runJoin(JoinOptions(factory, ice, listen, report), input, stop)
                }
                null
            } catch (error: CancellationException) {
                null
            } catch (error: Throwable) {
                errorText(error)
            }
            main.post {
                // Cleared before reporting, so the screens can start again as soon as they hear this.
                if (running === session) running = null
                TunnelService.stop(app)
                apply(Event.SessionEnded(error))
            }
        }
        return null
    }

    private fun apply(event: Event) {
        // Lets scripted tests on an emulator pick the strings up. Release builds never log them.
        if (BuildConfig.DEBUG && (event is Event.Invite || event is Event.Reply)) Log.d("mcrtc", event.toString())
        state.apply(event)?.let { send(it) }
        if (event is Event.Joined || event is Event.Hosting) TunnelService.update(event)
        changed()
    }

    /** Never waits, because the screens call this from the main thread. */
    fun send(line: String) {
        val session = running
        if (session == null || session.input.trySend(line).isFailure) {
            state.fail("Something Went Wrong", "mcrtc is not listening right now")
        }
    }

    fun stop() {
        running?.stop?.value = true
    }
}
