package io.github.stroblme.accent

import androidx.compose.ui.unit.IntRect
import androidx.compose.ui.unit.IntSize
import io.github.stroblme.accent.ui.ceiling
import io.github.stroblme.accent.ui.visible
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The two sums that let a PDF be pinched past 6×: which part of a page is worth drawing, and how
 * far in a given screen may go at all. Neither can be looked at from here — a bitmap is a stub in a
 * unit test and a layout needs a device — so the arithmetic under both is what is checked.
 */
class TileTest {
    /** A4 on a 1080 px phone at fit width. */
    private val a4 = IntRect(0, 0, 1080, 1528)

    /** A0 on the same phone at 16×, which is 422 M pixels and cannot be drawn in one go. */
    private val poster = IntRect(0, 0, 17280, 24432)

    private val screen = IntSize(1080, 2400)

    @Test
    fun `a page small enough to hold is drawn whole`() {
        // Even one the screen has left: it costs 6.6 MB, and blanking it would only mean drawing
        // it again on the way back.
        assertEquals(a4, visible(a4, IntRect(0, 9000, 1080, 11400)))
        assertEquals(a4, visible(a4, IntRect(0, 400, 1080, 2800)))
    }

    @Test
    fun `a page too big to hold is cut to the screen`() {
        val window = IntRect(2000, 9000, 3080, 11400)
        assertEquals(window, visible(poster, window))
    }

    @Test
    fun `the cut stops at the page's own edges`() {
        // Half a screen off the top-left corner: what is asked for stays inside the page, which is
        // what the core insists on.
        assertEquals(IntRect(0, 0, 580, 2200), visible(poster, IntRect(-500, -200, 580, 2200)))
    }

    @Test
    fun `a page the screen has left has nothing to draw`() {
        assertTrue(visible(poster, IntRect(0, -30000, 1080, -27600)).isEmpty)
    }

    @Test
    fun `the ceiling is what the layout can be asked for`() {
        // A phone has room to spare, so it gets the whole of what posters want.
        assertEquals(16f, ceiling(screen), 0.001f)
        // A landscape tablet runs into the packing first, and stops rather than throwing.
        assertEquals(12.799f, ceiling(IntSize(2560, 1600)), 0.001f)
        for (size in listOf(screen, IntSize(1440, 3200), IntSize(2560, 1600), IntSize(2960, 1848))) {
            val at = ceiling(size)
            assertTrue("$size wide at $at", size.width * at <= 32766f)
            assertTrue("$size tall at $at", size.height * at <= 65534f)
        }
    }
}
