package dev.mcrtc.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.unit.dp
import dev.mcrtc.UiState

@Composable
fun JoinScreen(
    state: UiState,
    onInviteChange: (String) -> Unit,
    onConnect: () -> Unit,
    onCancel: () -> Unit,
) {
    val context = LocalContext.current
    Column(
        Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(16.dp),
        verticalArrangement = Arrangement.spacedBy(16.dp),
    ) {
        Card(Modifier.fillMaxWidth()) {
            Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                Text("Your mcrtc ID", style = MaterialTheme.typography.titleMedium)
                Row(verticalAlignment = Alignment.CenterVertically) {
                    SelectionContainer(Modifier.weight(1f)) {
                        Text(
                            state.myId.ifEmpty { "Loading…" },
                            fontFamily = FontFamily.Monospace,
                            style = MaterialTheme.typography.bodyMedium,
                        )
                    }
                    TextButton(
                        onClick = { copyText(context, "mcrtc ID", state.myId) },
                        enabled = state.myId.isNotEmpty() && !state.myId.startsWith("Unavailable"),
                    ) { Text("Copy") }
                }
                Text(
                    "If the host uses a whitelist, give them this ID so they can let you in.",
                    style = MaterialTheme.typography.bodySmall,
                )
            }
        }

        Text("Paste the invite your friend sent you", style = MaterialTheme.typography.titleMedium)
        OutlinedTextField(
            value = state.invite,
            onValueChange = onInviteChange,
            modifier = Modifier.fillMaxWidth().heightIn(min = 120.dp),
            label = { Text("Invite") },
            isError = state.pasteError != null,
            supportingText = state.pasteError?.let { error -> { Text(error) } },
            textStyle = MaterialTheme.typography.bodySmall.copy(fontFamily = FontFamily.Monospace),
            maxLines = 8,
        )
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            OutlinedButton(onClick = { pasteText(context)?.let { onInviteChange(it.trim()) } }) {
                Text("Paste")
            }
            Button(
                onClick = onConnect,
                enabled = state.invite.isNotBlank() && (!state.running || state.awaitingInvite),
                modifier = Modifier.weight(1f),
            ) { Text("Connect") }
        }
        if (state.running) {
            TextButton(onClick = onCancel) { Text("Cancel") }
        }

        Text(
            "Invites and replies contain your IP addresses. Only share them with people you trust.",
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.error,
        )
    }
}
