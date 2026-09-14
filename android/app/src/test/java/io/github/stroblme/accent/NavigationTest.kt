package io.github.stroblme.accent

import io.github.stroblme.accent.ffi.LinkTarget
import io.github.stroblme.accent.ffi.PageSize
import io.github.stroblme.accent.ffi.PdfLinkBox
import io.github.stroblme.accent.ffi.Point
import io.github.stroblme.accent.ffi.Rect
import io.github.stroblme.accent.ui.Pagination
import io.github.stroblme.accent.ui.hit
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/**
 * Where a bookmark or a link puts the reader, and which link a finger landed on.
 *
 * Both are the sort of arithmetic that is wrong by a scale factor and looks almost right, and
 * neither needs a device: the sheet that lists the bookmarks and the intent that leaves the app do,
 * and are checked by hand instead.
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

    /** A destination past the end of its own page is an overrun, and falls through like one. */
    @Test
    fun `a destination past the end of a page falls onto the next`() {
        assertEquals(1, pages.to(0, 900f, width, 1f).first)
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
}
