package dev.mcrtc

import android.content.Context
import androidx.datastore.core.DataStore
import androidx.datastore.preferences.core.Preferences
import androidx.datastore.preferences.core.booleanPreferencesKey
import androidx.datastore.preferences.core.edit
import androidx.datastore.preferences.core.stringPreferencesKey
import androidx.datastore.preferences.preferencesDataStore
import dev.mcrtc.core.MobileSettings
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.map

/** What the Settings screen edits. Empty strings mean "use mcrtc's default". */
data class AppSettings(
    val stun: String = "",
    val turnUrl: String = "",
    val turnUser: String = "",
    val turnPass: String = "",
    /** Listen on every interface, so the world also shows under Friends, LAN Games. */
    val lanGames: Boolean = false,
    /**
     * Listen on UDP only. Off by default: NetherNet servers (Geyser's default transport, Bedrock
     * dedicated servers) sign players in over TCP on the Bedrock port. Only for RakNet servers that
     * also accept TCP there, where a TCP connection would lead the game away from RakNet.
     */
    val udpOnly: Boolean = false,
) {
    fun toMobile() = MobileSettings(
        stun = stun.trim(),
        turnUrl = turnUrl.trim(),
        turnUser = turnUser.trim(),
        turnPass = turnPass,
        listen = if (lanGames) "0.0.0.0:19132" else "",
        udpOnly = udpOnly,
    )
}

private val Context.dataStore: DataStore<Preferences> by preferencesDataStore("settings")

class SettingsStore(context: Context) {
    private val store = context.applicationContext.dataStore

    val settings: Flow<AppSettings> = store.data.map { prefs ->
        AppSettings(
            stun = prefs[STUN].orEmpty(),
            turnUrl = prefs[TURN_URL].orEmpty(),
            turnUser = prefs[TURN_USER].orEmpty(),
            turnPass = prefs[TURN_PASS].orEmpty(),
            lanGames = prefs[LAN_GAMES] ?: false,
            udpOnly = prefs[UDP_ONLY] ?: false,
        )
    }

    suspend fun save(settings: AppSettings) {
        store.edit { prefs ->
            prefs[STUN] = settings.stun
            prefs[TURN_URL] = settings.turnUrl
            prefs[TURN_USER] = settings.turnUser
            prefs[TURN_PASS] = settings.turnPass
            prefs[LAN_GAMES] = settings.lanGames
            prefs[UDP_ONLY] = settings.udpOnly
        }
    }

    private companion object {
        val STUN = stringPreferencesKey("stun")
        val TURN_URL = stringPreferencesKey("turn_url")
        val TURN_USER = stringPreferencesKey("turn_user")
        val TURN_PASS = stringPreferencesKey("turn_pass")
        val LAN_GAMES = booleanPreferencesKey("lan_games")
        val UDP_ONLY = booleanPreferencesKey("udp_only")
    }
}
