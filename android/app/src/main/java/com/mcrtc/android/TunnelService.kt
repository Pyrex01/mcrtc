// Keeps the process, the CPU and Wi-Fi awake while a session runs, because the player spends it
// in Minecraft with mcrtc in the background, or hosts with the screen off.

package com.mcrtc.android

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.net.wifi.WifiManager
import android.os.Build
import android.os.IBinder
import android.os.PowerManager
import com.mcrtc.android.net.Event

private const val CHANNEL_ID = "mcrtc-tunnel"
private const val NOTIFICATION_ID = 1
private const val EXTRA_TEXT = "text"

class TunnelService : Service() {
    private var wakeLock: PowerManager.WakeLock? = null
    private var wifiLock: WifiManager.WifiLock? = null

    override fun onCreate() {
        super.onCreate()
        val manager = getSystemService(NotificationManager::class.java)
        manager.createNotificationChannel(
            NotificationChannel(CHANNEL_ID, "Tunnel", NotificationManager.IMPORTANCE_LOW)
        )
        wakeLock = getSystemService(PowerManager::class.java)
            .newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "mcrtc:tunnel")
            .apply { acquire() }
        @Suppress("DEPRECATION")
        val mode = if (Build.VERSION.SDK_INT >= 29) {
            WifiManager.WIFI_MODE_FULL_LOW_LATENCY
        } else {
            WifiManager.WIFI_MODE_FULL_HIGH_PERF
        }
        wifiLock = applicationContext.getSystemService(WifiManager::class.java)
            ?.createWifiLock(mode, "mcrtc:tunnel")
            ?.apply { acquire() }
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        val text = intent?.getStringExtra(EXTRA_TEXT) ?: "Running"
        // Android requires foreground promotion before doing work for a foreground service.
        if (Build.VERSION.SDK_INT >= 29) {
            startForeground(NOTIFICATION_ID, notification(this, text), ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC)
        } else {
            startForeground(NOTIFICATION_ID, notification(this, text))
        }
        if (!Sessions.isRunning) stopSelf()
        return START_NOT_STICKY
    }

    override fun onDestroy() {
        wakeLock?.takeIf { it.isHeld }?.release()
        wifiLock?.takeIf { it.isHeld }?.release()
        super.onDestroy()
    }

    override fun onBind(intent: Intent?): IBinder? = null

    companion object {
        @android.annotation.SuppressLint("StaticFieldLeak") // the application context
        private var context: Context? = null

        /** Called from the screens, while the app is in the foreground as Android requires. */
        fun start(context: Context, mode: Mode) {
            this.context = context.applicationContext
            val text = if (mode == Mode.Host) "Starting to host" else "Joining a friend"
            val intent = Intent(context, TunnelService::class.java).putExtra(EXTRA_TEXT, text)
            context.startForegroundService(intent)
        }

        /** Rewrites the notification, which works from the background too. */
        fun update(event: Event) {
            val context = context ?: return
            val text = when (event) {
                is Event.Hosting -> "Hosting ${event.server}"
                is Event.Joined -> "Connected. Minecraft can join ${event.address}"
                else -> return
            }
            context.getSystemService(NotificationManager::class.java)
                .notify(NOTIFICATION_ID, notification(context, text))
        }

        fun stop(context: Context) {
            context.stopService(Intent(context, TunnelService::class.java))
        }

        private fun notification(context: Context, text: String): Notification {
            val open = PendingIntent.getActivity(
                context, 0, Intent(context, MainActivity::class.java),
                PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
            )
            return Notification.Builder(context, CHANNEL_ID)
                .setContentTitle("mcrtc")
                .setContentText(text)
                .setSmallIcon(android.R.drawable.stat_sys_upload)
                .setContentIntent(open)
                .setOngoing(true)
                .build()
        }
    }
}
