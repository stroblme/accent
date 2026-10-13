package io.github.stroblme.accent

import org.junit.Assert.assertEquals
import org.junit.Test

/**
 * Where Back from a backlink opens the document it was followed out of ([backTo]): where its
 * screen last said the reader was, and only if that was said of this document. The saying and the
 * putting back are the screens' and need a device.
 */
class BackTest {
    @Test
    fun `a backlink keeps where the reader was in the document it was followed from`() {
        val note = Back.File("Notes/Paper.md", scroll = 1280)
        assertEquals(note, backTo("Notes/Paper.md", note))

        val place = PdfPlace(page = 4, top = 120f, zoom = 2f, panX = -300f)
        val pdf = Back.File("Papers/paper.pdf", at = place)
        assertEquals(pdf, backTo("Papers/paper.pdf", pdf))
    }

    @Test
    fun `a place said of another document opens this one at its top`() {
        val elsewhere = Back.File("Notes/Other.md", scroll = 1280)
        assertEquals(Back.File("Notes/Paper.md"), backTo("Notes/Paper.md", elsewhere))
        assertEquals(Back.File("Notes/Paper.md"), backTo("Notes/Paper.md", null))
    }
}
