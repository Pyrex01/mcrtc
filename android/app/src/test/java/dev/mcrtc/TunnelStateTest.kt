package dev.mcrtc

import dev.mcrtc.core.MobileEvent
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class TunnelStateTest {
    /** Folds events in order, collecting every line the reducer asks to send. */
    private fun run(start: UiState, vararg events: MobileEvent): Pair<UiState, List<String>> {
        val sent = mutableListOf<String>()
        val end = events.fold(start) { state, event ->
            val step = reduce(state, event)
            step.send?.let(sent::add)
            step.state
        }
        return end to sent
    }

    private val connecting = UiState(invite = " mcrtc1:ab\n cd ").starting()

    @Test
    fun aNormalJoinEndsUpConnected() {
        val (state, sent) = run(
            connecting,
            MobileEvent.YourId("id-123"),
            MobileEvent.AskInvite,
            MobileEvent.Gathering,
            MobileEvent.Reply("mcrtc1:reply"),
            MobileEvent.WaitingForHost,
            MobileEvent.Joined("127.0.0.1:19132", "host/udp -> srflx/udp"),
        )
        assertEquals(listOf("mcrtc1:abcd"), sent)
        assertEquals(Screen.Connected, state.screen)
        assertEquals("id-123", state.myId)
        assertEquals("mcrtc1:reply", state.reply)
        assertTrue(state.waitingForHost)
        assertTrue(state.running)
        assertEquals("127.0.0.1:19132", state.address)
        assertEquals("Direct connection", describeRoute(state.route))
    }

    @Test
    fun theReplyScreenWaitsForTheHost() {
        val (state, _) = run(
            connecting,
            MobileEvent.AskInvite,
            MobileEvent.Gathering,
            MobileEvent.Reply("mcrtc1:reply"),
        )
        assertEquals(Screen.Reply, state.screen)
        assertFalse(state.waitingForHost)
        val waiting = reduce(state, MobileEvent.WaitingForHost).state
        assertEquals(Screen.Reply, waiting.screen)
        assertTrue(waiting.waitingForHost)
    }

    @Test
    fun aBadPasteReturnsToJoinAndTheFixGoesOutOnConnect() {
        val (rejected, sent) = run(
            connecting,
            MobileEvent.AskInvite,
            MobileEvent.BadPaste("not an mcrtc invite"),
            MobileEvent.AskInvite,
        )
        assertEquals(listOf("mcrtc1:abcd"), sent)
        assertEquals(Screen.Join, rejected.screen)
        assertEquals("not an mcrtc invite", rejected.pasteError)
        assertTrue(rejected.running)
        assertTrue(rejected.awaitingInvite)

        val fixed = rejected.copy(invite = "mcrtc1:good")
        val step = fixed.sendingInvite()
        assertEquals("mcrtc1:good", step.send)
        assertNull(step.state.pasteError)
        assertFalse(step.state.awaitingInvite)

        // The session asking again does not resend what is already on its way.
        val (state, again) = run(step.state, MobileEvent.Gathering)
        assertTrue(again.isEmpty())
        assertEquals(Screen.Working, state.screen)
    }

    @Test
    fun anEndedSessionWithAnErrorShowsIt() {
        val (state, _) = run(
            connecting,
            MobileEvent.AskInvite,
            MobileEvent.Gathering,
            MobileEvent.Reply("mcrtc1:reply"),
            MobileEvent.Joined("127.0.0.1:19132", null),
            MobileEvent.SessionEnded("the host went away"),
        )
        assertEquals(Screen.Ended, state.screen)
        assertEquals(Failure("Connection lost", "the host went away"), state.failure)
        assertFalse(state.running)
        assertEquals("", state.invite)
    }

    @Test
    fun anExpiredInviteSaysSo() {
        val (state, _) = run(
            connecting,
            MobileEvent.AskInvite,
            MobileEvent.Gathering,
            MobileEvent.Reply("mcrtc1:reply"),
            MobileEvent.WaitingForHost,
            MobileEvent.InviteExpired,
            MobileEvent.SessionEnded(null),
        )
        assertEquals(Screen.Ended, state.screen)
        assertEquals("Invite expired", state.failure?.title)
        assertFalse(state.running)
    }

    @Test
    fun aLeftReasonIsShownButAPlainDisconnectIsNot() {
        val joined = run(
            connecting,
            MobileEvent.AskInvite,
            MobileEvent.Joined("127.0.0.1:19132", "relayed"),
        ).first
        val kicked = run(joined, MobileEvent.Left("kicked by the host"), MobileEvent.SessionEnded(null)).first
        assertEquals(Failure("Disconnected", "kicked by the host"), kicked.failure)

        val (quit, _) = run(joined, MobileEvent.Left("Disconnected."), MobileEvent.SessionEnded(null))
        assertEquals(Screen.Join, quit.screen)
        assertNull(quit.failure)
        assertEquals("", quit.invite)
    }

    @Test
    fun startOverKeepsTheId() {
        val failed = UiState(myId = "id", invite = "x").startFailed("bad STUN url")
        assertEquals(Screen.Ended, failed.screen)
        val fresh = failed.startOver()
        assertEquals(UiState(myId = "id"), fresh)
    }

    @Test
    fun aWildcardListenAddressIsReachedThroughLoopback() {
        assertEquals("127.0.0.1:19132", minecraftAddress("0.0.0.0:19132"))
        assertEquals("127.0.0.1:19132", minecraftAddress("127.0.0.1:19132"))
    }
}
