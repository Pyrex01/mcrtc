package com.mcrtc.android

import com.mcrtc.android.net.Event
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class StateTest {
    @Test
    fun hostingWalksThroughTheInviteScreens() {
        val state = State()
        state.apply(Event.Hosting("127.0.0.1:25565"))
        assertEquals(Screen.Friends, state.screen)
        state.apply(Event.AskName(1, "friend-1"))
        state.apply(Event.Gathering)
        assertEquals("Generating Invite", state.progress.title)
        state.apply(Event.Invite("alex", "mcrtc1:x", 10))
        state.apply(Event.AskReply("alex"))
        assertEquals(Screen.Invite, state.screen)
        state.apply(Event.FriendConnected("alex", "host <-> srflx (direct)"))
        assertEquals("alex joined", state.toast)
        state.apply(Event.Stats("alex", 2, 40))
        assertEquals(2, state.friends.single().connections)
        assertEquals("", state.apply(Event.AskNext))
        state.apply(Event.SessionEnded(null))
        assertEquals(Screen.Title, state.screen)
    }

    @Test
    fun aPastedInviteIsSentWhenTheFlowAsks() {
        val state = State()
        state.forms.invite = " mcrtc1:abc \n def "
        state.beginJoin()
        assertEquals("mcrtc1:abcdef", state.apply(Event.AskInvite))
        assertNull(state.apply(Event.AskInvite))
        state.apply(Event.BadPaste("bad"))
        state.apply(Event.AskInvite)
        assertEquals(Screen.JoinSetup, state.screen)
    }

    @Test
    fun aHostLeavingIsShownButOurOwnDisconnectIsNot() {
        val state = State()
        state.apply(Event.AskInvite)
        state.apply(Event.Left("The host closed the connection."))
        state.apply(Event.SessionEnded(null))
        assertEquals(Screen.Failed, state.screen)

        val quiet = State()
        quiet.apply(Event.AskInvite)
        quiet.apply(Event.Left("Disconnected."))
        quiet.apply(Event.SessionEnded(null))
        assertEquals(Screen.Title, quiet.screen)
    }
}
