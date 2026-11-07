package com.mcrtc.android

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.content.Intent
import android.os.Bundle
import android.widget.*
import android.app.Activity
import android.os.Build
import android.view.View
import android.content.BroadcastReceiver
import android.content.IntentFilter

class MainActivity : Activity() {
    private lateinit var value: EditText
    private lateinit var status: TextView
    private val statusReceiver = object : BroadcastReceiver() {
        override fun onReceive(context: Context?, intent: Intent?) {
            intent?.getStringExtra("status")?.let { status.text = it }
        }
    }

    override fun onCreate(state: Bundle?) {
        super.onCreate(state)
        setContentView(R.layout.activity_main)
        value = findViewById(R.id.value); status = findViewById(R.id.status)
        findViewById<Button>(R.id.paste).setOnClickListener { paste() }
        findViewById<Spinner>(R.id.mode).setOnItemSelectedListener(object : android.widget.AdapterView.OnItemSelectedListener {
            override fun onNothingSelected(parent: android.widget.AdapterView<*>?) = Unit
            override fun onItemSelected(parent: android.widget.AdapterView<*>?, view: View?, position: Int, id: Long) {
                findViewById<Button>(R.id.start).text = if (position == 0) "Host Server" else "Join Server"
                value.hint = if (position == 0) "Minecraft server address (127.0.0.1:25565)" else "Paste invite from host"
            }
        })
        findViewById<Button>(R.id.start).setOnClickListener {
            if (!NativeSession.isAvailable()) {
                status.text = "Rust library missing for this device"
                return@setOnClickListener
            }
            val host = findViewById<Spinner>(R.id.mode).selectedItemPosition == 0
            val command = if (host) "start_host" else "start_join"
            val raw = value.text.toString().trim()
            if (!host && raw.isEmpty()) {
                status.text = "Paste an invite first"
                return@setOnClickListener
            }
            val intent = Intent(this, TunnelService::class.java).putExtra("command", command).putExtra("value", raw)
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) startForegroundService(intent) else startService(intent)
            status.text = if (host) "Starting host…" else "Joining…"
        }
        findViewById<Button>(R.id.stop).setOnClickListener {
            val intent = Intent(this, TunnelService::class.java).putExtra("command", "stop")
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) startForegroundService(intent) else startService(intent)
            status.text = "Disconnected"
        }
    }

    override fun onResume() {
        super.onResume()
        if (Build.VERSION.SDK_INT >= 33) {
            registerReceiver(statusReceiver, IntentFilter(ACTION_STATUS), RECEIVER_NOT_EXPORTED)
        } else {
            @Suppress("DEPRECATION")
            registerReceiver(statusReceiver, IntentFilter(ACTION_STATUS))
        }
    }

    override fun onPause() {
        unregisterReceiver(statusReceiver)
        super.onPause()
    }

    private fun paste() {
        val clipboard = getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
        value.setText(clipboard.primaryClip?.getItemAt(0)?.coerceToText(this) ?: "")
    }
}
