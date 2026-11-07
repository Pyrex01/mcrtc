package dev.mcrtc

import dev.mcrtc.core.MobileEvent

enum class Screen { Join, Working, Reply, Connected, Ended }

/** Everything the UI and the notification show. Changed only through [reduce] and the helpers below. */
data class UiState(
    val screen: Screen = Screen.Join,
    val myId: String = "",
    /** The invite as typed or pasted. */
    val invite: String = "",
    /** Why the last invite was rejected, shown under the field. */
    val pasteError: String? = null,
    /** A join session is alive in the Rust core. */
    val running: Boolean = false,
    /** The session is waiting for an invite, so Connect sends instead of starting. */
    val awaitingInvite: Boolean = false,
    val inviteSent: Boolean = false,
    val working: String = "",
    val reply: String = "",
    val waitingForHost: Boolean = false,
    val address: String = "",
    val route: String? = null,
    /** The last reason the session gave for ending; shown once it really ends. */
    val pendingFailure: Failure? = null,
    val failure: Failure? = null,
)

data class Failure(val title: String, val message: String)

/** The new state, and an invite to hand to the session if it asked for one we already have. */
data class Step(val state: UiState, val send: String? = null)

/** Invites survive chat apps that wrap or indent them: whitespace is never part of one. */
fun cleanPaste(text: String): String = text.filterNot { it.isWhitespace() }

fun describeRoute(route: String?): String? = route?.let {
    if ("relayed" in it) "Relayed through TURN" else "Direct connection"
}

/** The user pressed Connect with no session running. */
fun UiState.starting(): UiState = copy(
    screen = Screen.Working,
    working = "Starting",
    running = true,
    awaitingInvite = false,
    inviteSent = false,
    pasteError = null,
    reply = "",
    waitingForHost = false,
    pendingFailure = null,
    failure = null,
)

/** The user pressed Connect while the session waits for an invite; returns the line to send. */
fun UiState.sendingInvite(): Step = Step(
    copy(
        screen = Screen.Working,
        working = "Checking the invite",
        awaitingInvite = false,
        inviteSent = true,
        pasteError = null,
    ),
    cleanPaste(invite),
)

/** The session could not even start. */
fun UiState.startFailed(reason: String): UiState = copy(
    screen = Screen.Ended,
    running = false,
    awaitingInvite = false,
    failure = Failure("Could not start", reason),
)

/** Back to an empty Join screen. */
fun UiState.startOver(): UiState = UiState(myId = myId)

/** Folds one event from the Rust core into the state. Pure, so it can be tested on the JVM. */
fun reduce(state: UiState, event: MobileEvent): Step = when (event) {
    is MobileEvent.YourId -> Step(state.copy(myId = event.id))
    MobileEvent.AskInvite -> askInvite(state.copy(awaitingInvite = true))
    is MobileEvent.BadPaste -> Step(state.copy(pasteError = event.error, inviteSent = false))
    MobileEvent.Gathering -> Step(
        state.copy(screen = Screen.Working, working = "Gathering network candidates"),
    )
    is MobileEvent.Reply -> Step(state.copy(screen = Screen.Reply, reply = event.text))
    MobileEvent.WaitingForHost -> Step(state.copy(waitingForHost = true))
    is MobileEvent.Joined -> Step(
        state.copy(screen = Screen.Connected, address = event.address, route = event.route),
    )
    is MobileEvent.Left -> Step(
        // A plain "Disconnected." is what the user asked for, not something to explain.
        if (event.reason.isBlank() || event.reason == "Disconnected.") {
            state
        } else {
            state.copy(pendingFailure = Failure("Disconnected", event.reason))
        },
    )
    is MobileEvent.ConnectFailed -> Step(
        state.copy(pendingFailure = Failure("Could not connect", event.error)),
    )
    MobileEvent.InviteExpired -> Step(
        state.copy(
            pendingFailure = Failure(
                "Invite expired",
                "The invite expired before the host got your reply. Ask them for a new one.",
            ),
        ),
    )
    MobileEvent.ShuttingDown -> Step(state.copy(screen = Screen.Working, working = "Disconnecting"))
    is MobileEvent.SessionEnded -> Step(sessionEnded(state, event.error))
    MobileEvent.Cancelled, is MobileEvent.Other -> Step(state)
}

private fun askInvite(state: UiState): Step = when {
    // The last paste was rejected: let the user fix it.
    state.pasteError != null -> Step(state.copy(screen = Screen.Join))
    !state.inviteSent && state.invite.isNotBlank() -> state.sendingInvite()
    else -> Step(state)
}

private fun sessionEnded(state: UiState, error: String?): UiState {
    val failure = error?.let { Failure("Connection lost", it) } ?: state.pendingFailure
    val fresh = state.startOver()
    return if (failure == null) fresh else fresh.copy(screen = Screen.Ended, failure = failure)
}
