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

/** Image theme mapping and classification costs; decoding and serving need a device. */
class ImagesTest {
    private val plain = pageTheme(dark = false)
    private val recoloured = pageTheme(dark = true)

    @Test
    fun `an image is known by its extension, in any case`() {
        assertEquals(ImageKind.Raster, imageKind("Attachments/Scan 1.JPG"))
        assertEquals(ImageKind.Raster, imageKind("plot.avif"))
        assertEquals(ImageKind.Svg, imageKind("diagram.svg"))
        assertEquals(ImageKind.Raster, imageKind("loop.gif"))
        assertNull(imageKind("Note.md"))
        assertNull(imageKind("paper.pdf"))
        assertNull("a dot in a folder is not an extension", imageKind("v1.2/README"))
    }

    /** Dark theme remaps documents, but leaves photos unchanged. */
    @Test
    fun `a raster is recoloured when it reads as a document in a dark theme`() {
        assertEquals(recoloured, imageTheme(ImageKind.Raster, dark = true, inverted = false) { true })
        assertEquals(plain, imageTheme(ImageKind.Raster, dark = true, inverted = false) { false })
    }

    /** Inversion swaps page themes without image classification or decoding. */
    @Test
    fun `an inverted raster goes to the other page, and neither it nor a light one is classified`() {
        val never = { error("classified") }
        assertEquals(plain, imageTheme(ImageKind.Raster, dark = true, inverted = true, never))
        assertEquals(plain, imageTheme(ImageKind.Raster, dark = false, inverted = false, never))
        assertEquals(recoloured, imageTheme(ImageKind.Raster, dark = false, inverted = true, never))
    }

    @Test
    fun `an svg is always a drawing and an animation never recoloured`() {
        val never = { error("classified an svg or an animation") }
        assertEquals(recoloured, imageTheme(ImageKind.Svg, dark = true, inverted = false, never))
        assertEquals(plain, imageTheme(ImageKind.Svg, dark = true, inverted = true, never))
        assertEquals(recoloured, imageTheme(ImageKind.Svg, dark = false, inverted = true, never))
        for (dark in listOf(false, true)) for (inverted in listOf(false, true)) {
            assertEquals(plain, imageTheme(ImageKind.Animation, dark, inverted, never))
        }
    }

    /** Power-of-two sampling keeps the long side at most the limit and above half of it. */
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
