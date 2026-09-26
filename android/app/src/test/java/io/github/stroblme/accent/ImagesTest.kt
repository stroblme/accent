package io.github.stroblme.accent

import io.github.stroblme.accent.ui.ImageKind
import io.github.stroblme.accent.ui.imageKind
import io.github.stroblme.accent.ui.imageTheme
import io.github.stroblme.accent.ui.pageTheme
import io.github.stroblme.accent.ui.sampleSize
import io.github.stroblme.accent.ui.verdictKey
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertNull
import org.junit.Test
import java.io.File

/**
 * Which image is recoloured, onto what, and what an image costs to find out. The classifier and
 * the remap are the core's and tested there; the decoding and the serving need a device.
 */
class ImagesTest {
    private val plain = pageTheme(dark = false)
    private val recoloured = pageTheme(dark = true)

    @Test
    fun `an image is known by its extension, in any case`() {
        assertEquals(ImageKind.Raster, imageKind("Attachments/Scan 1.JPG"))
        assertEquals(ImageKind.Raster, imageKind("plot.avif"))
        assertEquals(ImageKind.Svg, imageKind("diagram.svg"))
        assertEquals(ImageKind.Gif, imageKind("loop.gif"))
        assertNull(imageKind("Note.md"))
        assertNull(imageKind("paper.pdf"))
        assertNull("a dot in a folder is not an extension", imageKind("v1.2/README"))
    }

    /** A document goes onto the dark page in a dark theme; a photo never unless inverted. */
    @Test
    fun `a raster is recoloured when it reads as a document in a dark theme`() {
        assertEquals(recoloured, imageTheme(ImageKind.Raster, dark = true, inverted = false) { true })
        assertEquals(plain, imageTheme(ImageKind.Raster, dark = true, inverted = false) { false })
        assertEquals(plain, imageTheme(ImageKind.Raster, dark = true, inverted = true) { true })
        assertEquals(recoloured, imageTheme(ImageKind.Raster, dark = true, inverted = true) { false })
    }

    /** Nothing to recolour in a light theme, so nothing is decoded to find out. */
    @Test
    fun `a light theme never asks, and an invert recolours onto the dark page`() {
        val never = { error("classified in a light theme") }
        assertEquals(plain, imageTheme(ImageKind.Raster, dark = false, inverted = false, never))
        assertEquals(recoloured, imageTheme(ImageKind.Raster, dark = false, inverted = true, never))
    }

    @Test
    fun `an svg is always a drawing and a gif never recoloured`() {
        val never = { error("classified an svg or a gif") }
        assertEquals(recoloured, imageTheme(ImageKind.Svg, dark = true, inverted = false, never))
        assertEquals(plain, imageTheme(ImageKind.Svg, dark = true, inverted = true, never))
        assertEquals(recoloured, imageTheme(ImageKind.Svg, dark = false, inverted = true, never))
        for (dark in listOf(false, true)) for (inverted in listOf(false, true)) {
            assertEquals(plain, imageTheme(ImageKind.Gif, dark, inverted, never))
        }
    }

    /** `inSampleSize` halves: the long side ends at most at the limit, and above half of it. */
    @Test
    fun `an image is decoded by the power of two that fits it`() {
        assertEquals(1, sampleSize(512, 512))
        assertEquals(2, sampleSize(513, 512))
        assertEquals(8, sampleSize(4032, 512))
        assertEquals(4, sampleSize(8192, 2048))
    }

    @Test
    fun `a file written again is asked about again`() {
        val file = File.createTempFile("scan", ".png").apply { deleteOnExit() }
        file.writeBytes(ByteArray(4))
        val before = verdictKey(file)
        assertEquals(before, verdictKey(file))
        file.appendBytes(ByteArray(4))
        assertNotEquals(before, verdictKey(file))
    }
}
