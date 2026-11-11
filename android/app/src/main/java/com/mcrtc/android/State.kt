// The session as the screens see it, folded from its events. Nothing here draws or does I/O.
// Mirrors `src/ui/state.rs`.

package com.mcrtc.android

import com.mcrtc.android.net.Event

enum class Screen { Title, HostSetup, Friends, Name, Invite, JoinSetup, Reply, Joined, Options, Progress, Failed }

enum class Mode { Host, Join }

data class Friend(val name: String, val route: String?, val connections: Int, val ping: Long?)

data class Message(val title: String = "", val detail: String = "")

data class Joined(val address: String, val route: String?, val connections: Int, val ping: Long?)

/** The text of every form, kept while moving between screens. */
class Forms {
    var server = ""
    var maxPeers = ""
    var maxConnections = ""
    var stun = ""
    var turnUrl = ""
    var turnUser = ""
    var turnPass = ""
    var invite = ""
    var listen = ""
    var name = ""
    var reply = ""
}

fun positive(text: String): Int? = text.trim().toIntOrNull()?.takeIf { it > 0 }

/** Chat apps like to wrap long strings, and none of ours contain whitespace. */
fun cleanPaste(text: String): String = text.filterNot { it.isWhitespace() }

fun describeRoute(route: String?): String? = route?.let {
    if ("relayed" in it) "Relayed through TURN" else "Direct connection"
}

class State {
    var screen = Screen.Title
    var mode: Mode? = null
    val forms = Forms()
    val friends = mutableListOf<Friend>()
    var defaultName = ""
    var nameReady = false
    var waitingForSlot: Int? = null
    var invite = ""
    var reply = ""
    var label = ""
    var error = ""
    var progress = Message()
    var failure = Message()
    var joined: Joined? = null

    /** The flow is waiting for an invite to be pasted. */
    var awaitingInvite = false
    private var sentInvite = false
    private var leftReason = ""

    /** Set when there is something new to pop up in the corner. */
    var toast: String? = null

    fun fail(title: String, detail: String) {
        failure = Message(title, detail)
        screen = Screen.Failed
    }

    fun backFromFailure(): Screen = if (mode == Mode.Host) Screen.Friends else Screen.Title

    fun showProgress(title: String, detail: String) {
        progress = Message(title, detail)
        screen = Screen.Progress
    }

    fun beginJoin() {
        sentInvite = false
    }

    /** The invite to hand to the flow, marking it as sent. */
    fun takeInvite(): String {
        awaitingInvite = false
        sentInvite = true
        return cleanPaste(forms.invite)
    }

    /** Folds in one event. Returns a line the flow is waiting for, if the answer needs no person. */
    fun apply(event: Event): String? {
        when (event) {
            is Event.Hosting -> {
                mode = Mode.Host
                friends.clear()
                waitingForSlot = null
                screen = Screen.Friends
            }
            is Event.AskName -> {
                nameReady = true
                waitingForSlot = null
                defaultName = event.default
                if (screen != Screen.Failed) screen = Screen.Friends
            }
            is Event.WaitingForSlot -> {
                waitingForSlot = event.limit
                screen = Screen.Friends
            }
            Event.Gathering -> {
                val title = if (mode == Mode.Host) "Generating Invite" else "Joining"
                showProgress(title, "Gathering network candidates")
            }
            is Event.Invite -> {
                invite = event.text
                label = event.label
                error = ""
                forms.reply = ""
            }
            is Event.AskReply -> screen = Screen.Invite
            is Event.BadPaste -> {
                error = event.error
                if (mode == Mode.Join) sentInvite = false
            }
            is Event.Connecting -> showProgress("Connecting to ${event.label}", "Opening the tunnel")
            is Event.FriendConnected -> {
                toast = "${event.label} joined"
                val friend = Friend(event.label, event.route, 0, null)
                val index = friends.indexOfFirst { it.name == friend.name }
                if (index >= 0) friends[index] = friend else friends += friend
            }
            is Event.FriendLeft -> {
                friends.removeAll { it.name == event.label }
                toast = "${event.label} ${event.reason}"
            }
            is Event.Stats -> {
                val index = friends.indexOfFirst { it.name == event.label }
                if (index >= 0) {
                    friends[index] = friends[index].copy(connections = event.connections, ping = event.pingMs)
                } else {
                    joined = joined?.copy(connections = event.connections, ping = event.pingMs)
                }
            }
            Event.InviteExpired -> toast = "The invite expired"
            is Event.ConnectFailed -> fail("Could Not Connect", event.error)
            // The screens have their own Invite button, so the invite loop never has to wait here.
            Event.AskNext -> return ""
            Event.ShuttingDown -> showProgress("Stopping", "Saying goodbye")
            Event.AskInvite -> {
                mode = Mode.Join
                awaitingInvite = true
                if (error.isNotEmpty()) {
                    screen = Screen.JoinSetup
                } else if (!sentInvite && forms.invite.isNotBlank()) {
                    return takeInvite()
                }
            }
            is Event.Reply -> {
                reply = event.text
                screen = Screen.Reply
            }
            is Event.Joined -> {
                joined = Joined(event.address, event.route, 0, null)
                screen = Screen.Joined
            }
            is Event.Left -> leftReason = event.reason
            is Event.SessionEnded -> sessionEnded(event.error)
            Event.Cancelled, Event.WaitingForHost -> Unit
        }
        return null
    }

    private fun sessionEnded(error: String?) {
        val ended = mode
        val reason = leftReason
        mode = null
        leftReason = ""
        awaitingInvite = false
        nameReady = false
        joined = null
        friends.clear()
        screen = Screen.Title
        if (error != null) {
            fail("Connection Lost", error)
        } else if (ended == Mode.Join && reason.isNotEmpty() && reason != "Disconnected.") {
            fail("Disconnected", reason)
        }
    }
}
