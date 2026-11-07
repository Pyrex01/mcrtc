package dev.mcrtc

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class SettingsTest {
    @Test
    fun udpOnlyIsOffByDefaultForNetherNet() {
        assertFalse(AppSettings().toMobile().udpOnly)
        assertTrue(AppSettings(udpOnly = true).toMobile().udpOnly)
    }

    @Test
    fun lanGamesListensOnEveryInterface() {
        assertEquals("0.0.0.0:19132", AppSettings(lanGames = true).toMobile().listen)
        assertEquals("", AppSettings().toMobile().listen)
    }
}
