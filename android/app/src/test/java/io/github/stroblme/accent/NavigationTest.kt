package io.github.stroblme.accent

import io.github.stroblme.accent.ffi.PageSize
import io.github.stroblme.accent.ui.Pagination
import org.junit.Assert.assertEquals
import org.junit.Test

/**
 * Where a bookmark puts the reader.
 *
 * The sort of arithmetic that is wrong by a scale factor and looks almost right, and it needs no
 * device: the panel that lists the bookmarks does, and is checked by hand instead.
 */
class NavigationTest {
    /** Five A4 pages with the 8 px gap the column puts under each, as at a 1080 px fit width. */
    private val pages = Pagination(List(5) { PageSize(595f, 842f) }, gap = 8f)
    private val width = 1080

    /**
     * The one thing a jump has to get right: the row it lands on does not depend on the zoom, and
     * the offset into that row scales with it. Going straight to `requestScrollToItem(page, y)` with
     * a `y` in points would put the reader 8× too high at 8×.
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

    /** A destination past the end of its own page is an overrun, and falls through like any other. */
    @Test
    fun `a destination past the end of a page falls onto the next`() {
        assertEquals(1, pages.to(0, 900f, width, 1f).first)
    }
}
