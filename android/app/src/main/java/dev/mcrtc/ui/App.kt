package dev.mcrtc.ui

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.Settings
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import dev.mcrtc.Screen
import dev.mcrtc.SettingsStore
import dev.mcrtc.Tunnel

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun App(settings: SettingsStore, onConnect: () -> Unit) {
    val state by Tunnel.state.collectAsStateWithLifecycle()
    var showSettings by rememberSaveable { mutableStateOf(false) }
    BackHandler(enabled = showSettings) { showSettings = false }

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(if (showSettings) "Settings" else "mcrtc") },
                navigationIcon = {
                    if (showSettings) {
                        IconButton(onClick = { showSettings = false }) {
                            Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = "Back")
                        }
                    }
                },
                actions = {
                    if (!showSettings) {
                        IconButton(onClick = { showSettings = true }) {
                            Icon(Icons.Filled.Settings, contentDescription = "Settings")
                        }
                    }
                },
            )
        },
    ) { padding ->
        Box(Modifier.fillMaxSize().padding(padding)) {
            if (showSettings) {
                SettingsScreen(settings, enabled = !state.running)
            } else {
                when (state.screen) {
                    Screen.Join -> JoinScreen(
                        state = state,
                        onInviteChange = Tunnel::setInvite,
                        onConnect = onConnect,
                        onCancel = Tunnel::stop,
                    )
                    Screen.Working -> WorkingScreen(state.working, onCancel = Tunnel::stop)
                    Screen.Reply -> ReplyScreen(state, onCancel = Tunnel::stop)
                    Screen.Connected -> ConnectedScreen(state, onDisconnect = Tunnel::stop)
                    Screen.Ended -> EndedScreen(state, onStartOver = Tunnel::startOver)
                }
            }
        }
    }
}
