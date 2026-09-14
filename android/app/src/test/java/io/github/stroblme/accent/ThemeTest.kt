package io.github.stroblme.accent

import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.ui.graphics.Color
import io.github.stroblme.accent.ui.flattened
import org.junit.Assert.assertEquals
import org.junit.Test

/**
 * The page a note sits on, which is the one colour role the app takes off the system rather than
 * from it. Material You tints its own surface towards the wallpaper and the result reads cream
 * beside the desktop's white view; light mode replaces that role and dark mode keeps what it was
 * given. Worth a test because it is pure arithmetic on a data class and nothing else here is.
 */
class ThemeTest {
    private val tinted = Color(0xFFFEF7FF)

    @Test
    fun `light mode is white everywhere the page shows`() {
        val light = lightColorScheme(surface = tinted, background = tinted).flattened(dark = false)
        assertEquals(Color.White, light.surface)
        assertEquals(Color.White, light.background)
        assertEquals(Color.White, light.surfaceVariant)
        assertEquals(Color.White, light.surfaceContainerHighest)
    }

    @Test
    fun `dark mode keeps the surface the system gave it`() {
        val shade = Color(0xFF141218)
        val dark = darkColorScheme(surface = shade, background = tinted).flattened(dark = true)
        assertEquals(shade, dark.surface)
        assertEquals(shade, dark.background)
    }
}
