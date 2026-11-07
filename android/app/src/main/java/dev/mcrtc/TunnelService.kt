package dev.mcrtc

import android.app.Notification
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.net.wifi.WifiManager
import android.os.Build
import android.os.IBinder
import android.os.PowerManager
import android.util.Log
import androidx.core.app.NotificationChannelCompat
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat
import androidx.core.app.ServiceCompat
import androidx.core.content.ContextCompat
import dev.mcrtc.core.MobileException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.launch
import kotlin.concurrent.thread

/**
 * Keeps the process, Wi-Fi and CPU awake while a join session runs, with an ongoing notification.
 * It starts the session on [Tunnel]'s core and stops itself once the session has ended.
 */
class TunnelService : Service() {
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
    private var wifiLock: WifiManager.WifiLock? = null
    private var wakeLock: PowerManager.WakeLock? = null
    private var started = false
    /** The coroutines watching the current session; a new session gets fresh ones. */
    private var session: Job? = null

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (intent?.action == ACTION_STOP) {
            Tunnel.stop()
            if (!started) stopSelf()
            return START_NOT_STICKY
        }
        if (started) return START_NOT_STICKY
        started = true

        createChannel()
        ServiceCompat.startForeground(
            this,
            NOTIFICATION_ID,
            notification(statusText(Tunnel.state.value)),
            if (Build.VERSION.SDK_INT >= 34) ServiceInfo.FOREGROUND_SERVICE_TYPE_SPECIAL_USE else 0,
        )
        acquireLocks()

        // A Connect right after the last session ended can reach this same instance before it is
        // destroyed, so everything below is per session, not per instance.
        session?.cancel()
        val session = Job(scope.coroutineContext[Job]).also { session = it }
        scope.launch(session + Dispatchers.IO) {
            val settings = SettingsStore(this@TunnelService).settings.first()
            try {
                Tunnel.core(this@TunnelService).start(settings.toMobile())
            } catch (e: MobileException) {
                Log.w(TAG, "Could not start a session: ${e.reason()}")
                Tunnel.startFailed(e.reason())
            } catch (e: UnsatisfiedLinkError) {
                Log.e(TAG, "Could not load the mcrtc core", e)
                Tunnel.startFailed(e.message ?: "Could not load the mcrtc core")
            }
        }
        scope.launch(session) {
            Tunnel.state.collect { state ->
                if (!state.running) finish()
            }
        }
        scope.launch(session) {
            Tunnel.state.map(::statusText).distinctUntilChanged().collect { text ->
                if (NotificationManagerCompat.from(this@TunnelService).areNotificationsEnabled()) {
                    @Suppress("MissingPermission") // Checked just above.
                    NotificationManagerCompat.from(this@TunnelService)
                        .notify(NOTIFICATION_ID, notification(text))
                }
            }
        }
        return START_NOT_STICKY
    }

    private fun finish() {
        session?.cancel()
        session = null
        started = false
        releaseLocks()
        ServiceCompat.stopForeground(this, ServiceCompat.STOP_FOREGROUND_REMOVE)
        stopSelf()
    }

    override fun onDestroy() {
        scope.cancel()
        releaseLocks()
        // Lets a running session say goodbye to the host; it blocks, so not on the main thread.
        thread(name = "mcrtc-shutdown") { Tunnel.shutDown() }
        super.onDestroy()
    }

    private fun acquireLocks() {
        val wifi = applicationContext.getSystemService(WifiManager::class.java)
        @Suppress("DEPRECATION") // FULL_HIGH_PERF is the best there is before Android 10.
        val mode = if (Build.VERSION.SDK_INT >= 29) {
            WifiManager.WIFI_MODE_FULL_LOW_LATENCY
        } else {
            WifiManager.WIFI_MODE_FULL_HIGH_PERF
        }
        wifiLock = wifi?.createWifiLock(mode, "mcrtc:tunnel")?.apply {
            setReferenceCounted(false)
            acquire()
        }
        val power = getSystemService(PowerManager::class.java)
        wakeLock = power?.newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "mcrtc:tunnel")?.apply {
            setReferenceCounted(false)
            // The session may run for a whole evening of play; it is released when it ends.
            @Suppress("WakelockTimeout")
            acquire()
        }
    }

    private fun releaseLocks() {
        wifiLock?.takeIf { it.isHeld }?.release()
        wifiLock = null
        wakeLock?.takeIf { it.isHeld }?.release()
        wakeLock = null
    }

    private fun createChannel() {
        val channel = NotificationChannelCompat.Builder(CHANNEL_ID, NotificationManagerCompat.IMPORTANCE_LOW)
            .setName("Tunnel")
            .setDescription("Shows while mcrtc is connected to a friend")
            .setShowBadge(false)
            .build()
        NotificationManagerCompat.from(this).createNotificationChannel(channel)
    }

    private fun notification(text: String): Notification {
        val open = PendingIntent.getActivity(
            this,
            0,
            Intent(this, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP),
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )
        val stop = PendingIntent.getService(
            this,
            1,
            Intent(this, TunnelService::class.java).setAction(ACTION_STOP),
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )
        return NotificationCompat.Builder(this, CHANNEL_ID)
            .setSmallIcon(R.drawable.ic_stat_tunnel)
            .setContentTitle("mcrtc")
            .setContentText(text)
            .setContentIntent(open)
            .setOngoing(true)
            .setOnlyAlertOnce(true)
            .setSilent(true)
            .setCategory(NotificationCompat.CATEGORY_SERVICE)
            .setForegroundServiceBehavior(NotificationCompat.FOREGROUND_SERVICE_IMMEDIATE)
            .addAction(0, "Stop", stop)
            .build()
    }

    companion object {
        private const val TAG = "mcrtc-app"
        private const val CHANNEL_ID = "tunnel"
        private const val NOTIFICATION_ID = 1
        private const val ACTION_START = "dev.mcrtc.START"
        private const val ACTION_STOP = "dev.mcrtc.STOP"

        fun start(context: Context) {
            ContextCompat.startForegroundService(
                context,
                Intent(context, TunnelService::class.java).setAction(ACTION_START),
            )
        }

        fun statusText(state: UiState): String = when (state.screen) {
            Screen.Join -> "Waiting for an invite"
            Screen.Working -> "${state.working}…"
            Screen.Reply -> if (state.waitingForHost) "Waiting for host…" else "Send your reply to the host"
            Screen.Connected -> "Connected — Minecraft: ${minecraftAddress(state.address)}"
            Screen.Ended -> "Ended"
        }
    }
}

/** Where Minecraft should connect: a wildcard listen address is reached through loopback. */
fun minecraftAddress(address: String): String {
    val port = address.substringAfterLast(':', "19132")
    val host = address.substringBeforeLast(':', "127.0.0.1")
    return if (host == "0.0.0.0" || host == "[::]" || host.isEmpty()) "127.0.0.1:$port" else "$host:$port"
}
