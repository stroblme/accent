package io.github.stroblme.accent

import io.github.stroblme.accent.ffi.Glyph
import io.github.stroblme.accent.ffi.Point
import io.github.stroblme.accent.ffi.Rect
import io.github.stroblme.accent.ui.Caret
import io.github.stroblme.accent.ui.Selection
import io.github.stroblme.accent.ui.grownTo
import io.github.stroblme.accent.ui.nearest
import io.github.stroblme.accent.ui.pieces
import io.github.stroblme.accent.ui.text
import io.github.stroblme.accent.ui.wordAt
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/**
 * What a selection on a PDF covers: the desktop's rules over the core's glyphs, which is the part
 * of it that is arithmetic. The handles and the menu over it want a device.
 */
class SelectionTest {
    /** A line of text, one glyph a point wide per character; a space has no box of its own. */
    private fun line(text: String) = text.mapIndexed { i, ch ->
        val right = if (ch == ' ') i.toFloat() else i + 1f
        Glyph(ch.toString(), Rect(i.toFloat(), 0f, right, 10f), i.toUInt())
    }

    @Test
    fun `a selection over a page break takes the rest of one page and the start of the next`() {
        val glyphs = mapOf(0 to line("ab cd"), 1 to line("ef"))
        val pieces = Selection(Caret(0, 3), Caret(1, 0)).pieces(glyphs)!!
        val ranges = pieces.map { Triple(it.page, it.start, it.end) }
        assertEquals(listOf(Triple(0, 3, 5), Triple(1, 0, 1)), ranges)
        assertEquals(listOf("cd", "e"), pieces.map { it.text })
    }

    @Test
    fun `a space is selected but not painted`() {
        val glyphs = mapOf(0 to line("ab cd"))
        val piece = Selection(Caret(0, 0), Caret(0, 4)).pieces(glyphs)!!.single()
        assertEquals("ab cd", piece.text)
        assertEquals(4, piece.boxes.size)
    }

    /** A scan between two pages of text gives nothing, and does not stop the pages around it. */
    @Test
    fun `a page without text in the middle gives nothing`() {
        val glyphs = mapOf(0 to line("ab"), 1 to emptyList(), 2 to line("cd"))
        val pieces = Selection(Caret(0, 1), Caret(2, 0)).pieces(glyphs)!!
        assertEquals(listOf(0, 2), pieces.map { it.page })
    }

    @Test
    fun `a page not read yet is no answer yet`() {
        assertNull(Selection(Caret(0, 0), Caret(1, 0)).pieces(mapOf(0 to line("ab"))))
    }

    /** Inside a glyph's box it is that glyph; in the margin, the nearest one level with it. */
    @Test
    fun `a finger takes the glyph under it, or the nearest one`() {
        val glyphs = line("ab cd")
        assertEquals(1, nearest(glyphs, Point(1.5f, 5f)))
        assertEquals(4, nearest(glyphs, Point(30f, 5f)))
        assertEquals(0, nearest(glyphs, Point(-3f, 12f)))
        assertNull(nearest(emptyList(), Point(0f, 0f)))
    }

    @Test
    fun `a long press takes the word, and a glyph that is none alone`() {
        val glyphs = line("an ex-ample")
        assertEquals(0..1, wordAt(glyphs, 1))
        assertEquals(6..10, wordAt(glyphs, 8))
        assertEquals(5..5, wordAt(glyphs, 5))
    }

    /** Dragged either way, the word the press made stays selected. */
    @Test
    fun `a word grows towards the finger and keeps itself`() {
        val word = Selection(Caret(0, 3), Caret(0, 5))
        assertEquals(Selection(Caret(0, 3), Caret(1, 2)), word.grownTo(Caret(1, 2)))
        assertEquals(Selection(Caret(0, 1), Caret(0, 5)), word.grownTo(Caret(0, 1)))
        assertEquals(word, word.grownTo(Caret(0, 4)))
    }

    @Test
    fun `a selection over a page break copies as two lines`() {
        val glyphs = mapOf(0 to line("ab cd"), 1 to line("ef"))
        assertEquals("cd\nef", Selection(Caret(0, 3), Caret(1, 1)).pieces(glyphs)!!.text())
    }

    @Test
    fun `the ends are put in order however they were made`() {
        val back = Selection.between(Caret(2, 5), Caret(1, 9))
        assertEquals(Selection(Caret(1, 9), Caret(2, 5)), back)
    }
}
