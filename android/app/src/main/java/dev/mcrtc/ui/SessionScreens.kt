package dev.mcrtc.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedCard
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.unit.dp
import dev.mcrtc.UiState
import dev.mcrtc.describeRoute
import dev.mcrtc.minecraftAddress

@Composable
fun WorkingScreen(text: String, onCancel: () -> Unit) {
    Column(
        Modifier.fillMaxSize().padding(16.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.spacedBy(24.dp, Alignment.CenterVertically),
    ) {
        CircularProgressIndicator(Modifier.size(48.dp))
        Text("$text…", style = MaterialTheme.typography.titleMedium)
        TextButton(onClick = onCancel) { Text("Cancel") }
    }
}

@Composable
fun ReplyScreen(state: UiState, onCancel: () -> Unit) {
    val context = LocalContext.current
    Column(
        Modifier.fillMaxSize().padding(16.dp),
        verticalArrangement = Arrangement.spacedBy(16.dp),
    ) {
        Text("Send this reply to the host", style = MaterialTheme.typography.titleLarge)
        Text(
            "They paste it in mcrtc to let you in.",
            style = MaterialTheme.typography.bodyMedium,
        )
        OutlinedCard(Modifier.fillMaxWidth().weight(1f, fill = false).heightIn(min = 80.dp)) {
            SelectionContainer {
                Text(
                    state.reply,
                    modifier = Modifier.verticalScroll(rememberScrollState()).padding(12.dp),
                    fontFamily = FontFamily.Monospace,
                    style = MaterialTheme.typography.bodySmall,
                )
            }
        }
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            Button(
                onClick = { copyText(context, "mcrtc reply", state.reply) },
                modifier = Modifier.weight(1f).heightIn(min = 56.dp),
            ) { Text("Copy") }
            Button(
                onClick = { shareText(context, state.reply) },
                modifier = Modifier.weight(1f).heightIn(min = 56.dp),
            ) { Text("Share") }
        }
        Row(verticalAlignment = Alignment.CenterVertically) {
            if (state.waitingForHost) {
                CircularProgressIndicator(Modifier.size(20.dp), strokeWidth = 2.dp)
                Text("  Waiting for host…", style = MaterialTheme.typography.bodyMedium)
            }
        }
        OutlinedButton(onClick = onCancel, modifier = Modifier.fillMaxWidth()) { Text("Cancel") }
    }
}

@Composable
fun ConnectedScreen(state: UiState, onDisconnect: () -> Unit) {
    val context = LocalContext.current
    val minecraft = remember { minecraftIntent(context) }
    val address = minecraftAddress(state.address)
    Column(
        Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(16.dp),
        verticalArrangement = Arrangement.spacedBy(16.dp),
    ) {
        Text("Connected", style = MaterialTheme.typography.headlineMedium)
        describeRoute(state.route)?.let { Text(it, style = MaterialTheme.typography.bodyMedium) }
        Text("Keep mcrtc running, then in Minecraft:", style = MaterialTheme.typography.titleMedium)
        val steps = listOf(
            "Open Minecraft and tap Play.",
            "Open the Servers tab and tap Add Server.",
            "Enter any name, address ${address.substringBeforeLast(':')}, " +
                "port ${address.substringAfterLast(':')}.",
            "Save, then tap the server to join your friend's world.",
        )
        steps.forEachIndexed { i, step ->
            Text("${i + 1}. $step", style = MaterialTheme.typography.bodyLarge)
        }
        if (minecraft != null) {
            Button(
                onClick = { context.startActivity(minecraft) },
                modifier = Modifier.fillMaxWidth().heightIn(min = 56.dp),
            ) { Text("Open Minecraft") }
        }
        OutlinedButton(onClick = onDisconnect, modifier = Modifier.fillMaxWidth()) {
            Text("Disconnect")
        }
    }
}

@Composable
fun EndedScreen(state: UiState, onStartOver: () -> Unit) {
    val failure = state.failure
    Column(
        Modifier.fillMaxSize().padding(16.dp),
        verticalArrangement = Arrangement.spacedBy(16.dp),
    ) {
        Text(failure?.title ?: "Session ended", style = MaterialTheme.typography.headlineSmall)
        failure?.let {
            SelectionContainer { Text(it.message, style = MaterialTheme.typography.bodyLarge) }
        }
        Button(onClick = onStartOver, modifier = Modifier.fillMaxWidth()) { Text("Start over") }
    }
}
