package io.github.stroblme.accent

import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.toArgb
import io.github.stroblme.accent.ffi.Theme
import io.github.stroblme.accent.ui.conflictTints
import io.github.stroblme.accent.ui.flattened
import io.github.stroblme.accent.ui.page
import io.github.stroblme.accent.ui.pageTheme
import io.github.stroblme.accent.ui.paperAccent
import io.github.stroblme.accent.ui.rgba
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The split between what the device is asked for and what the app brings: the accent is the
 * system's, the page and the ink on it are the desktop's. Material You tints its own surface and
 * text towards the wallpaper, so a scheme handed in here arrives tinted and has to come out flat.
 * Worth a test because it is pure arithmetic on a data class and nothing else here is.
 */
class ThemeTest {
    private val tinted = Color(0xFFFEF7FF)
    private val white = Color(0xFFFFFFFF)
    private val shade = Color(0xFF1D1D20)

    @Test
    fun `the page is the desktop's view, not the wallpaper's`() {
        val light = lightColorScheme(surface = tinted, background = tinted).flattened(dark = false)
        assertEquals(white, light.surface)
        assertEquals(white, light.background)
        assertEquals(white, light.surfaceVariant)
        assertEquals(white, light.surfaceContainerHighest)

        val dark = darkColorScheme(surface = tinted, background = tinted).flattened(dark = true)
        assertEquals(shade, dark.surface)
        assertEquals(shade, dark.background)
        assertEquals(shade, dark.surfaceVariant)
        assertEquals(shade, dark.surfaceContainerHighest)
    }

    /** A printout is the light page in either theme, so its accent is the light theme's. */
    @Test
    fun `paper takes the light theme's accent`() {
        val ink = Color(0xFF3584E4)
        val pastel = Color(0xFF99C1F1)
        assertEquals(ink, paperAccent(lightColorScheme(primary = ink).flattened(dark = false)))
        val dark = darkColorScheme(primary = pastel, inversePrimary = ink).flattened(dark = true)
        assertEquals(ink, paperAccent(dark))
    }

    @Test
    fun `the ink on it is the desktop's too`() {
        val light = lightColorScheme(onSurface = tinted).flattened(dark = false)
        assertEquals(Color(0xFF333338), light.onSurface)
        val dark = darkColorScheme(onSurface = tinted).flattened(dark = true)
        assertEquals(Color(0xFFEBEBEB), dark.onSurface)
    }

    /**
     * The dim step is libadwaita's `.dim-label`, 55% of the ink over the page, and is pinned
     * because it is the one value here chosen for parity over contrast: 3.2:1 on white.
     */
    @Test
    fun `secondary text is the dim label over the same page`() {
        val light = lightColorScheme(onSurfaceVariant = tinted).flattened(dark = false)
        assertEquals(0xFF8F8F92.toInt(), light.onSurfaceVariant.toArgb())
        val dark = darkColorScheme(onSurfaceVariant = tinted).flattened(dark = true)
        assertEquals(0xFF8E8E8F.toInt(), dark.onSurfaceVariant.toArgb())
    }

    /**
     * A conflict block's sides are the desktop's green and blue mixed with the ink as its editor
     * and preview mix them: under white ink these are the numbers its Dark preview paints.
     */
    @Test
    fun `a conflict is tinted as on the desktop`() {
        val (current, base, incoming) = conflictTints(Color.White)
        assertEquals("rgba(114, 205, 147, 0.16)", current.first.rgba())
        assertEquals("rgba(114, 205, 147, 0.35)", current.second.rgba())
        assertEquals("rgba(255, 255, 255, 0.08)", base.first.rgba())
        assertEquals("rgba(122, 172, 238, 0.16)", incoming.first.rgba())
        val html = page("", Color.White, Color.Black, Color.Blue)
        assertTrue(html, ".conflict-incoming { background: rgba(122, 172, 238, 0.16); }" in html)
    }

    /** The one family that is still the device's answer to what colour it is. */
    @Test
    fun `the accent comes through untouched`() {
        val accent = Color(0xFF6750A4)
        assertEquals(accent, lightColorScheme(primary = accent).flattened(dark = false).primary)
        assertEquals(accent, darkColorScheme(primary = accent).flattened(dark = true).primary)
    }

    /**
     * A PDF page, and an image that reads as a document: onto the dark page in a dark theme, left
     * alone in a light one, and the other way round for a file the reader has inverted.
     */
    @Test
    fun `a document is recoloured onto the dark page`() {
        val dark = Theme.Recolour(0x1D1D20u, 0xEBEBEBu)
        assertEquals(Theme.Plain, pageTheme(dark = false))
        assertEquals(dark, pageTheme(dark = true))
        assertEquals(Theme.Plain, pageTheme(dark = true, inverted = true))
        assertEquals(dark, pageTheme(dark = false, inverted = true))
    }

    /** What makes the Browse pill dark on a light theme and light on a dark one. */
    @Test
    fun `the inverse pair is the other mode's page`() {
        val light = lightColorScheme().flattened(dark = false)
        assertEquals(shade, light.inverseSurface)
        assertEquals(Color(0xFFEBEBEB), light.inverseOnSurface)

        val dark = darkColorScheme().flattened(dark = true)
        assertEquals(white, dark.inverseSurface)
        assertEquals(Color(0xFF333338), dark.inverseOnSurface)
    }
}
