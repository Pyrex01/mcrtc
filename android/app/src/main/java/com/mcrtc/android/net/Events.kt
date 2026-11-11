// What the host and join flows have to say, independent of how it reaches a person. Mirrors
// `src/events.rs`.

package com.mcrtc.android.net

import kotlinx.coroutines.async
import kotlinx.coroutines.channels.ReceiveChannel
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.selects.select

sealed interface Event {
    data class Hosting(val server: String) : Event
    data class AskName(val friend: Int, val default: String) : Event
    data class WaitingForSlot(val limit: Int) : Event
    data object Gathering : Event
    data class Invite(val label: String, val text: String, val minutes: Long) : Event
    data class AskReply(val label: String) : Event
    data class BadPaste(val error: String) : Event
    data class Connecting(val label: String) : Event
    data class FriendConnected(val label: String, val route: String?) : Event
    data class FriendLeft(val label: String, val reason: String) : Event
    data class Stats(val label: String, val connections: Int, val pingMs: Long?) : Event
    data object Cancelled : Event
    data object InviteExpired : Event
    data class ConnectFailed(val error: String) : Event
    data object AskNext : Event
    data object ShuttingDown : Event

    data object AskInvite : Event
    data class Reply(val text: String) : Event
    data object WaitingForHost : Event
    data class Joined(val address: String, val route: String?) : Event
    data class Left(val reason: String) : Event

    data class SessionEnded(val error: String?) : Event
}

typealias Reporter = (Event) -> Unit

/** Lines from the person, the way the desktop flows read stdin. Null once it is closed. */
suspend fun ReceiveChannel<String>.readLine(): String? = receiveCatching().getOrNull()

/** Runs `block` until it finishes or `shutdown` turns true, whichever comes first. */
suspend fun <T> untilStopped(shutdown: StateFlow<Boolean>, block: suspend () -> T): T? =
    coroutineScope {
        val work = async { block() }
        val stop = async { shutdown.first { it } }
        select {
            work.onAwait { stop.cancel(); it }
            stop.onAwait { work.cancel(); null }
        }
    }

/** The value of whichever of `first` and `second` finishes first; the other is cancelled. */
suspend fun <T> race(first: suspend () -> T, second: suspend () -> T): T = coroutineScope {
    val a = async { first() }
    val b = async { second() }
    select {
        a.onAwait { b.cancel(); it }
        b.onAwait { a.cancel(); it }
    }
}

/** Splits `host:port`, with brackets around an IPv6 host, or takes `fallbackPort` when none. */
fun splitAddress(address: String, fallbackPort: Int): Pair<String, Int> {
    val text = address.trim()
    val (host, port) = when {
        text.startsWith("[") -> text.substringAfter('[').substringBefore(']') to
            text.substringAfter("]", "").removePrefix(":")
        text.count { it == ':' } == 1 -> text.substringBefore(':') to text.substringAfter(':')
        else -> text to ""
    }
    if (port.isEmpty()) return host to fallbackPort
    val number = port.toIntOrNull()?.takeIf { it in 1..65535 } ?: fail("`$address` has an invalid port")
    return host to number
}

fun errorText(error: Throwable): String = error.message ?: error.toString()
