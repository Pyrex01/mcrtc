package dev.mcrtc.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.unit.dp
import dev.mcrtc.AppSettings
import dev.mcrtc.SettingsStore
import kotlinx.coroutines.launch

@Composable
fun SettingsScreen(store: SettingsStore, enabled: Boolean) {
    val saved by store.settings.collectAsState(initial = null)
    // Edits stay local and are written through, so typing never fights the stored value.
    var edited by remember { mutableStateOf<AppSettings?>(null) }
    val settings = edited ?: saved ?: return
    val scope = rememberCoroutineScope()
    fun change(next: AppSettings) {
        edited = next
        scope.launch { store.save(next) }
    }

    Column(
        Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(16.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        if (!enabled) {
            Text(
                "Settings apply to the next connection. Disconnect to change them.",
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.error,
            )
        }
        Row(verticalAlignment = Alignment.CenterVertically) {
            Column(Modifier.weight(1f)) {
                Text("Show in LAN Games", style = MaterialTheme.typography.titleMedium)
                Text(
                    "The world also appears under Friends, LAN Games, for this phone and others " +
                        "on the same Wi-Fi.",
                    style = MaterialTheme.typography.bodySmall,
                )
            }
            Switch(
                checked = settings.lanGames,
                onCheckedChange = { change(settings.copy(lanGames = it)) },
                enabled = enabled,
            )
        }
        Row(verticalAlignment = Alignment.CenterVertically) {
            Column(Modifier.weight(1f)) {
                Text("RakNet only (UDP)", style = MaterialTheme.typography.titleMedium)
                Text(
                    "Leave off for Geyser and Bedrock servers using NetherNet. Turn on only if " +
                        "a RakNet server also accepts TCP on its port.",
                    style = MaterialTheme.typography.bodySmall,
                )
            }
            Switch(
                checked = settings.udpOnly,
                onCheckedChange = { change(settings.copy(udpOnly = it)) },
                enabled = enabled,
            )
        }
        Text("Network", style = MaterialTheme.typography.titleMedium)
        OutlinedTextField(
            value = settings.stun,
            onValueChange = { change(settings.copy(stun = it)) },
            label = { Text("STUN servers") },
            supportingText = { Text("Separated by spaces. Leave empty for the defaults.") },
            enabled = enabled,
            keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Uri),
            modifier = Modifier.fillMaxWidth(),
        )
        OutlinedTextField(
            value = settings.turnUrl,
            onValueChange = { change(settings.copy(turnUrl = it)) },
            label = { Text("TURN server") },
            placeholder = { Text("turn:example.com:3478") },
            supportingText = { Text("Only needed when a direct connection fails.") },
            singleLine = true,
            enabled = enabled,
            keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Uri),
            modifier = Modifier.fillMaxWidth(),
        )
        OutlinedTextField(
            value = settings.turnUser,
            onValueChange = { change(settings.copy(turnUser = it)) },
            label = { Text("TURN username") },
            singleLine = true,
            enabled = enabled,
            modifier = Modifier.fillMaxWidth(),
        )
        OutlinedTextField(
            value = settings.turnPass,
            onValueChange = { change(settings.copy(turnPass = it)) },
            label = { Text("TURN password") },
            singleLine = true,
            enabled = enabled,
            visualTransformation = PasswordVisualTransformation(),
            keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Password),
            modifier = Modifier.fillMaxWidth(),
        )
    }
}
