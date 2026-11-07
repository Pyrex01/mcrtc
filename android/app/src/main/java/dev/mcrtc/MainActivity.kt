package dev.mcrtc

import android.Manifest
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.result.contract.ActivityResultContracts
import androidx.core.content.ContextCompat
import dev.mcrtc.ui.App
import dev.mcrtc.ui.theme.McrtcTheme

class MainActivity : ComponentActivity() {
    // The notification is nice to have; the tunnel works without it.
    private val notificationPermission =
        registerForActivityResult(ActivityResultContracts.RequestPermission()) {}

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        Tunnel.preload(this)
        if (savedInstanceState == null) handleShare(intent)
        val settings = SettingsStore(this)
        setContent {
            McrtcTheme {
                App(settings = settings, onConnect = ::connect)
            }
        }
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        handleShare(intent)
    }

    private fun connect() {
        if (Build.VERSION.SDK_INT >= 33 &&
            ContextCompat.checkSelfPermission(this, Manifest.permission.POST_NOTIFICATIONS) !=
            PackageManager.PERMISSION_GRANTED
        ) {
            notificationPermission.launch(Manifest.permission.POST_NOTIFICATIONS)
        }
        Tunnel.connect(this)
    }

    /** An invite shared from a chat app lands in the invite field. */
    private fun handleShare(intent: Intent?) {
        if (intent?.action != Intent.ACTION_SEND || intent.type != "text/plain") return
        val text = intent.getStringExtra(Intent.EXTRA_TEXT)?.trim().orEmpty()
        if (text.isEmpty()) return
        val state = Tunnel.state.value
        // A session that is already past the invite has no use for another one.
        if (state.running && !state.awaitingInvite) return
        if (state.screen == Screen.Ended) Tunnel.startOver()
        Tunnel.setInvite(text)
    }
}
