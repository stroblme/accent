package io.github.stroblme.accent

import androidx.compose.ui.unit.IntSize
import io.github.stroblme.accent.ffi.LinkTarget
import io.github.stroblme.accent.ffi.PageSize
import io.github.stroblme.accent.ffi.PdfLink
import io.github.stroblme.accent.ffi.PdfLinkBox
import io.github.stroblme.accent.ffi.Point
import io.github.stroblme.accent.ffi.Rect
import io.github.stroblme.accent.ui.Mark
import io.github.stroblme.accent.ui.Pagination
import io.github.stroblme.accent.ui.hit
import io.github.stroblme.accent.ui.markAt
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/**
 * Where a bookmark or a link puts the reader, which page a finger landed on, and which link.
 *
 * All three are the sort of arithmetic that is wrong by a scale factor and looks almost right,
 * and none needs a device: the panel that lists the bookmarks and the intent that leaves the app
 * do, and are checked by hand instead.
 */
class NavigationTest {
    /** Five A4 pages with the 8 px gap the column puts under each, as at a 1080 px fit width. */
    private val pages = Pagination(List(5) { PageSize(595f, 842f) }, gap = 8f)
    private val width = 1080

    /**
     * The one thing a jump has to get right: the row it lands on does not depend on the zoom, and
     * the offset into that row scales with it. Going straight to `requestScrollToItem(page, y)`
     * with a `y` in points would put the reader 8× too high at 8×.
     */
    @Test
    fun `a jump lands in the same place whatever the zoom`() {
        val (row, into) = pages.to(2, 100f, width, 1f)
        assertEquals(2, row)
        // 100 pt at 1080/595 px per point.
        assertEquals(182f, into.toFloat(), 1f)

        val (deep, deepInto) = pages.to(2, 100f, width, 8f)
        assertEquals(2, deep)
        assertEquals(1452f, deepInto.toFloat(), 2f)
    }

    @Test
    fun `a bookmark at the top of a page lands at the top of its row`() {
        assertEquals(0, pages.to(0, 0f, width, 1f).first)
        assertEquals(0, pages.to(0, 0f, width, 1f).second)
    }

    /**
     * Back after a jump: the place left is kept on the paper, so a pinch at the destination does
     * not send the reader back to the right row at the wrong height.
     */
    @Test
    fun `a place left at one zoom is found again at another`() {
        val (page, top) = pages.place(pages.top(2, width, 1f) + 182f, width, 1f)
        assertEquals(2, page)
        assertEquals(100f, top, 1f)

        val (row, into) = pages.to(page, top, width, 8f)
        assertEquals(2, row)
        // The column counts whole pixels, and half of one at fit width is four at 8×.
        assertEquals(1452f, into.toFloat(), 5f)
    }

    /** A destination past the end of its own page is an overrun, and falls through like one. */
    @Test
    fun `a destination past the end of a page falls onto the next`() {
        assertEquals(1, pages.to(0, 900f, width, 1f).first)
    }

    /** A tap, once the pan and the scroll have been taken off it, is a point in the column. */
    @Test
    fun `a tap on a page comes back in the page's own points`() {
        val land = pages.on(540f, 764f, width, 1f)!!
        assertEquals(0, land.page)
        // Half the width of the column is half the width of the paper, whatever the zoom.
        assertEquals(297.5f, land.point.x, 0.01f)
        assertEquals(420.91f, land.point.y, 0.01f)
        assertEquals(1080f / 595f, land.scale, 0.001f)
    }

    /** The 8 px under each page belongs to no page, and neither does anything past the last. */
    @Test
    fun `a tap that missed every page is on none of them`() {
        val row = pages.top(1, width, 1f)
        assertNull(pages.on(540f, row - 4f, width, 1f))
        assertNull(pages.on(540f, pages.top(4, width, 1f) + 99_000f, width, 1f))
        assertNull(pages.on(540f, -1f, width, 1f))
    }

    /**
     * The property the hit test rests on: the same place on the paper is the same place in points
     * however far the pages have been pinched, because that is the space links are given in.
     */
    @Test
    fun `the same point on the paper reads the same at any zoom`() {
        for (zoom in listOf(1f, 8f)) {
            val scale = width * zoom / 595f
            val y = pages.top(2, width, zoom) + 100f * scale
            val land = pages.on(297.5f * scale, y, width, zoom)!!
            assertEquals(2, land.page)
            assertEquals(297.5f, land.point.x, 0.05f)
            assertEquals(100f, land.point.y, 0.05f)
        }
    }

    private fun link(left: Float, top: Float, right: Float, bottom: Float, to: LinkTarget) =
        PdfLinkBox(Rect(left, top, right, bottom), to)

    @Test
    fun `a tap inside a link follows it`() {
        val links = listOf(link(100f, 200f, 300f, 220f, LinkTarget.Page(4u, 50f)))
        assertEquals(links[0], hit(links, Point(200f, 210f), slop = 0f))
    }

    /** A link is usually one line of text, and a fingertip is taller than one line. */
    @Test
    fun `a tap just off a thin link still follows it`() {
        val links = listOf(link(100f, 200f, 300f, 210f, LinkTarget.Uri("https://example.org")))
        assertNull(hit(links, Point(200f, 216f), slop = 0f))
        assertEquals(links[0], hit(links, Point(200f, 216f), slop = 8f))
    }

    @Test
    fun `a tap on the page and nothing else follows nothing`() {
        val links = listOf(link(100f, 200f, 300f, 220f, LinkTarget.Uri("https://example.org")))
        assertNull(hit(links, Point(400f, 210f), slop = 8f))
        assertNull(hit(links, Point(200f, 400f), slop = 8f))
        assertNull(hit(emptyList(), Point(200f, 210f), slop = 8f))
    }

    /** `/Link` boxes may overlap; the later annotation is the one drawn over the other. */
    @Test
    fun `the link on top wins where two overlap`() {
        val under = link(100f, 200f, 300f, 220f, LinkTarget.Page(1u, null))
        val over = link(150f, 200f, 250f, 220f, LinkTarget.Page(2u, null))
        assertEquals(over, hit(listOf(under, over), Point(200f, 210f), slop = 0f))
    }

    private val screen = IntSize(1080, 2000)

    @Test
    fun `a passage already in view is left where it is`() {
        val rect = Rect(100f, 300f, 200f, 312f)
        assertNull(pages.reveal(0, rect, above = 0f, panX = 0f, screen, 1f, clear = 150f))
    }

    /** Down the document, it comes to the middle of what the bar leaves of the screen. */
    @Test
    fun `a passage further down is brought to the middle`() {
        val rect = Rect(100f, 400f, 200f, 412f)
        val to = pages.reveal(3, rect, above = 0f, panX = 0f, screen, 1f, clear = 150f)!!
        val above = pages.top(to.row, width, 1f) + to.into
        val middle = pages.top(3, width, 1f) + 406f * (1080f / 595f)
        assertEquals((150f + 2000f) / 2, middle - above, 1f)
        assertEquals(0f, to.panX, 0f)
    }

    /** At 4× a passage off the side is panned to the middle, and the scroll is left alone. */
    @Test
    fun `a passage off to the side is panned to it`() {
        val scale = 4 * 1080f / 595f
        val above = pages.top(1, width, 4f) + 400f * scale - 1000f
        val rect = Rect(400f, 400f, 440f, 412f)
        val to = pages.reveal(1, rect, above, panX = 0f, screen, 4f, clear = 0f)!!
        assertEquals(540f - 420f * scale, to.panX, 1f)
        assertEquals(above, pages.top(to.row, width, 4f) + to.into, 1f)
    }

    private fun mark(src: String, vararg quads: Rect) =
        Mark(quads.toList(), PdfLink(src, 0L, 0u, listOf(0u, 0u, 0u, 5u), null))

    /**
     * A highlight over two lines is hit on either, and where two notes highlight the same words
     * the first listed opens, as the desktop's `highlight_at` has it — unlike a `/Link`, where
     * the later box is the one on top.
     */
    @Test
    fun `a tap on a highlight finds its note, the first where two overlap`() {
        val a = mark("A.md", Rect(100f, 200f, 300f, 210f), Rect(100f, 212f, 180f, 222f))
        val b = mark("B.md", Rect(150f, 200f, 250f, 210f))
        assertEquals(a, markAt(listOf(a, b), Point(200f, 205f), slop = 0f))
        assertEquals(a, markAt(listOf(a, b), Point(120f, 220f), slop = 0f))
        assertEquals(b, markAt(listOf(b, a), Point(200f, 205f), slop = 0f))
        assertNull(markAt(listOf(a, b), Point(200f, 240f), slop = 8f))
    }
}
