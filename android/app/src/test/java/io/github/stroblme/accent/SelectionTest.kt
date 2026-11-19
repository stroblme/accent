package io.github.stroblme.accent

import androidx.compose.ui.geometry.Offset
import io.github.stroblme.accent.ffi.Glyph
import io.github.stroblme.accent.ffi.Point
import io.github.stroblme.accent.ffi.Rect
import io.github.stroblme.accent.ui.Caret
import io.github.stroblme.accent.ui.Selection
import io.github.stroblme.accent.ui.grownTo
import io.github.stroblme.accent.ui.loupeAt
import io.github.stroblme.accent.ui.nearest
import io.github.stroblme.accent.ui.pieces
import io.github.stroblme.accent.ui.slid
import io.github.stroblme.accent.ui.slides
import io.github.stroblme.accent.ui.text
import io.github.stroblme.accent.ui.wordAt
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** PDF text selection over core glyphs; handles and menus need device coverage. */
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

    /** An empty text page does not block selection across adjacent pages. */
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

    /** Select the containing glyph, or the nearest aligned glyph from its margin. */
    @Test
    fun `a finger takes the glyph under it, or the nearest one`() {
        val glyphs = line("ab cd")
        assertEquals(1, nearest(glyphs, Point(1.5f, 5f)))
        assertEquals(4, nearest(glyphs, Point(30f, 5f)))
        assertEquals(0, nearest(glyphs, Point(-3f, 12f)))
        assertNull(nearest(emptyList(), Point(0f, 0f)))
    }

    /** The loupe keeps to the end's line, a space taken at the glyph before it. */
    @Test
    fun `a loupe keeps to the line its end is on and lets go far past it`() {
        val below = line("xyz").map {
            it.copy(rect = Rect(it.rect.left + 10f, 20f, it.rect.right + 10f, 30f))
        }
        val glyphs = line("ab cd") + below
        assertEquals(Point(1.5f, 5f), loupeAt(glyphs, 1, 1.5f, 2f))
        assertEquals(Point(1f, 5f), loupeAt(glyphs, 2, 1f, 2f))
        // Past the line's end the loupe stops at it, the next line's boxes no part of it.
        assertEquals(Point(5f, 5f), loupeAt(glyphs, 4, 6f, 2f))
        assertNull(loupeAt(glyphs, 4, 8f, 2f))
        assertEquals(Point(10f, 25f), loupeAt(glyphs, 5, 9f, 2f))
    }

    @Test
    fun `the loupe slides onto another line and keeps to the finger along one`() {
        val (line, next) = (Offset(10f, 5f) to Offset(10f, 25f))
        assertFalse(slides(Offset.Unspecified, line))
        assertFalse(slides(line, Offset(40f, 5f)))
        assertTrue(slides(line, next))
        assertFalse(slides(next, Offset.Unspecified))
        // Half way down, the finger moving on along the new line meanwhile: no new slide, and the
        // loupe heads for where the end is now.
        assertFalse(slides(next, Offset(30f, 25f)))
        assertEquals(Offset(20f, 15f), slid(line, Offset(30f, 25f), 0.5f))
        // Another line half way there slides on from where the loupe is, not from where it began.
        val now = slid(line, next, 0.5f)
        assertEquals(Offset(10f, 15f), now)
        assertEquals(Offset(10f, 30f), slid(now, Offset(10f, 45f), 0.5f))
        // Nothing to slide from, or nothing to show, is no slide at all.
        assertEquals(next, slid(Offset.Unspecified, next, 0.5f))
        assertEquals(Offset.Unspecified, slid(line, Offset.Unspecified, 0.5f))
    }

    @Test
    fun `a long press takes the word, and a glyph that is none alone`() {
        val glyphs = line("an ex-ample")
        assertEquals(0..1, wordAt(glyphs, 1))
        assertEquals(6..10, wordAt(glyphs, 8))
        assertEquals(5..5, wordAt(glyphs, 5))
    }

    /** Preserve the pressed word when a selection drag reverses direction. */
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
