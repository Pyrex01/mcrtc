package com.mcrtc.android

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.Service
import android.content.Intent
import android.os.IBinder
import android.app.Notification.Builder

class TunnelService : Service() {
    private var handle = 0L
    private val channelId = "mcrtc-tunnel"

    override fun onCreate() {
        super.onCreate()
        val manager = getSystemService(NotificationManager::class.java)
        manager.createNotificationChannel(NotificationChannel(channelId, "mcrtc tunnel", NotificationManager.IMPORTANCE_LOW))
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        val action = intent?.getStringExtra("command")
        // Android requires foreground promotion before doing work for a foreground service.
        startForeground(1, notification("Starting"))
        if (!NativeSession.isAvailable()) {
            broadcast("error: the Rust tunnel library is not packaged for this device ABI")
            stopSelf()
            return START_NOT_STICKY
        }
        if (handle == 0L) handle = NativeSession.create()
        if (action == "stop") {
            NativeSession.command(handle, "stop", "")
            stopSelf()
        } else if (action != null) {
            val result = NativeSession.command(handle, action, intent.getStringExtra("value") ?: "")
            if (result.startsWith("error:")) broadcast(result)
        }
        if (action != "stop") broadcast(if (action == "start_host") "Hosting" else "Joining")
        return START_STICKY
    }

    private fun broadcast(message: String) {
        sendBroadcast(Intent(ACTION_STATUS).putExtra("status", message))
    }

    private fun notification(text: String): Notification = Builder(this, channelId)
        .setContentTitle("mcrtc")
        .setContentText(text)
        .setSmallIcon(android.R.drawable.stat_sys_upload)
        .setOngoing(true)
        .build()

    override fun onDestroy() {
        if (handle != 0L) NativeSession.destroy(handle)
        handle = 0L
        super.onDestroy()
    }
    override fun onBind(intent: Intent?): IBinder? = null
}

const val ACTION_STATUS = "com.mcrtc.android.TUNNEL_STATUS"
