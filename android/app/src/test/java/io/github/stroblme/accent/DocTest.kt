package io.github.stroblme.accent

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** One document is in front at a time ([VaultState.doc]), and what acts on it reaches only it. */
class DocTest {
    private val pdf = OpenPdf("Papers/paper.pdf", "/v/Papers/paper.pdf", PdfPlace(4, zoom = 2f))

    @Test
    fun `closing a note reached from a PDF highlight returns to the PDF where it was`() {
        val note = VaultState(doc = Open("Notes/a.md", back = Back.Pdf(pdf)))
        assertEquals(pdf, note.closed().doc)

        assertNull("opened any other way", VaultState(doc = Open("Notes/a.md")).closed().doc)
        assertNull("the PDF itself", VaultState(doc = pdf).closed().doc)
    }

    @Test
    fun `find and print reach the document in front and nothing else`() {
        val image = VaultState(doc = OpenImage("fig.png", "/v/fig.png", animated = false))
        assertEquals(image, image.finding(true))
        assertEquals(image, image.printing(true))

        val paper = VaultState(doc = pdf)
        assertTrue((paper.finding(true).doc as OpenPdf).finding)
        assertTrue((paper.printing(true).doc as OpenPdf).printing)

        val note = VaultState(doc = Open("Notes/a.md"))
        assertTrue(note.finding(true).note!!.finding)
        assertTrue(note.printing(true).note!!.printing)
    }
}
