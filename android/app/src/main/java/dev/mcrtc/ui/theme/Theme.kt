package dev.mcrtc.ui.theme

import android.os.Build
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.dynamicDarkColorScheme
import androidx.compose.material3.dynamicLightColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext

private val Grass = Color(0xFF2E7D32)
private val GrassLight = Color(0xFF81C784)

private val LightColors = lightColorScheme(primary = Grass, secondary = Color(0xFF52634F))
private val DarkColors = darkColorScheme(primary = GrassLight, secondary = Color(0xFFB9CCB4))

/** Material 3 with the wallpaper's colors on Android 12+, and a green fallback before that. */
@Composable
fun McrtcTheme(dark: Boolean = isSystemInDarkTheme(), content: @Composable () -> Unit) {
    val colors = when {
        Build.VERSION.SDK_INT >= 31 -> {
            val context = LocalContext.current
            if (dark) dynamicDarkColorScheme(context) else dynamicLightColorScheme(context)
        }
        dark -> DarkColors
        else -> LightColors
    }
    MaterialTheme(colorScheme = colors, content = content)
}
