package io.github.stroblme.accent

import androidx.compose.ui.geometry.Offset
import io.github.stroblme.accent.ffi.PageSize
import io.github.stroblme.accent.ui.Pagination
import io.github.stroblme.accent.ui.anchor
import io.github.stroblme.accent.ui.liveShift
import org.junit.Assert.assertEquals
import org.junit.Test

/** Pinch anchoring arithmetic, checked without a device. */
class AnchorTest {
    private fun near(expected: Float, actual: Float) = assertEquals(expected, actual, 0.001f)

    @Test
    fun `a drag moves the content with the fingers`() {
        // No pinch, so the centroid does not matter: 30 px of drag is 30 px less scrolled.
        near(70f, anchor(scrolled = 100f, centroid = 500f, by = 1f, pan = 30f))
        near(130f, anchor(scrolled = 100f, centroid = 0f, by = 1f, pan = -30f))
    }

    @Test
    fun `a pinch leaves what was between the fingers where it was`() {
        // Fingers at 500 with nothing scrolled past: that point is 500 into the document, is
        // 1000 into it once the document doubles, and has to come back 500 to stay put.
        near(500f, anchor(scrolled = 0f, centroid = 500f, by = 2f, pan = 0f))
        // And back again.
        near(0f, anchor(scrolled = 500f, centroid = 500f, by = 0.5f, pan = 0f))
        // Fingers at the very top edge: nothing above them, so nothing has to move.
        near(0f, anchor(scrolled = 0f, centroid = 0f, by = 3f, pan = 0f))
        // Which is the case the old code got right and every other one wrong: away from the
        // corner, a zoom at the top-left is the only place the two agree.
        near(600f, anchor(scrolled = 200f, centroid = 0f, by = 3f, pan = 0f))
    }

    @Test
    fun `a pinch that also drags does both`() {
        near(460f, anchor(scrolled = 0f, centroid = 500f, by = 2f, pan = 40f))
    }

    /** Keep the live scaled layer anchored at `pivot + (screen - pivot) * live`. */
    private fun shown(screen: Float, live: Float, base: Float, pivot: Float, shift: Float) =
        (screen - base) * live + liveShift(pivot, live, base, shift)

    @Test
    fun `a scaled layer keeps the point between the fingers between them`() {
        // Straight up, with the layer where it started: the pivot itself does not move.
        near(500f, shown(screen = 500f, live = 2f, base = 0f, pivot = 500f, shift = 0f))
        // A point 100 below it is twice as far below it afterwards.
        near(700f, shown(screen = 600f, live = 2f, base = 0f, pivot = 500f, shift = 0f))
        near(300f, shown(screen = 400f, live = 2f, base = 0f, pivot = 500f, shift = 0f))
        // Shrinking is the same sum the other way.
        near(550f, shown(screen = 600f, live = 0.5f, base = 0f, pivot = 500f, shift = 0f))
    }

    @Test
    fun `a layer already pushed sideways scales about the same point`() {
        // Pages pushed 200 left: what is at 500 on the screen is at 700 in the column, and the
        // pivot still holds.
        near(500f, shown(screen = 500f, live = 3f, base = -200f, pivot = 500f, shift = 0f))
        near(800f, shown(screen = 600f, live = 3f, base = -200f, pivot = 500f, shift = 0f))
    }

    @Test
    fun `the hand moving takes the whole layer with it`() {
        near(540f, shown(screen = 500f, live = 2f, base = 0f, pivot = 500f, shift = 40f))
    }

    /** The live pinch layer and final layout must show the same page position on release. */
    @Test
    fun `the release changes nothing that was on the screen`() {
        for (live in listOf(0.5f, 1.5f, 3f)) {
            for (shift in listOf(-120f, 0f, 80f)) {
                val (above, pivot, into) = Triple(900f, 400f, 1500f)
                near(
                    shown(screen = into - above, live = live, base = 0f, pivot = pivot, shift = shift),
                    into * live - anchor(scrolled = above, centroid = pivot, by = live, pan = shift),
                )
            }
        }
    }

    /** Forty pages with landscape pages and Pixel 8 sized gaps. */
    private val pages = Pagination(
        List(40) { if (it % 7 == 3) PageSize(842f, 595f) else PageSize(595f, 842f) },
        gap = 21f,
    )
    private val width = 1080

    /**
     * A pinch must preserve the point under the fingers while pages scale and gaps remain fixed.
     */
    @Test
    fun `a release leaves the word under the fingers under them`() {
        val pivot = Offset(400f, 900f)
        val lives = listOf(0.6f, 1.7f, 3f)
        val shifts = listOf(Offset(-90f, 130f), Offset.Zero, Offset(60f, -200f))
        for (page in listOf(0, 1, 17, 38)) for (zoom in listOf(1f, 2.5f)) for (live in lives) {
            // The gesture holds the layer between the zoom's two ends.
            if (zoom * live < 1f) continue
            for (shift in shifts) {
                // Centred sideways, and scrolled so the fingers land 500 pt down [page].
                val panX = -width * (zoom - 1f) / 2
                val scale = width * zoom / (if (page % 7 == 3) 842f else 595f)
                val above = pages.top(page, width, zoom) + 500f * scale - pivot.y
                val to = pages.released(above, panX, pivot, shift, live, width, zoom, maxZoom = 8f)
                // The word under the fingers, and one a line above it.
                for (at in listOf(pivot, pivot + Offset(120f, -60f))) {
                    val land = pages.on(at.x - panX, at.y + above, width, zoom)!!
                    val layer = Offset(
                        (at.x - panX) * live + liveShift(pivot.x, live, panX, shift.x),
                        at.y * live + liveShift(pivot.y, live, 0f, shift.y),
                    )
                    val (x, y) = land.point.x to land.point.y
                    near1(layer, pages.screen(land.page, x, y, to.above, to.panX, width, to.zoom))
                }
            }
        }
    }

    private fun near1(expected: Offset, actual: Offset) {
        assertEquals(expected.x, actual.x, 1f)
        assertEquals(expected.y, actual.y, 1f)
    }

    /** Shrunk with the fingers low on the first page, there is nothing above it to bring down. */
    @Test
    fun `a release past the top of the document stops at its top`() {
        val to = pages.released(0f, -540f, Offset(540f, 1800f), Offset.Zero, 0.5f, width, 2f, 8f)
        assertEquals(1f, to.zoom, 0f)
        assertEquals(0 to 0, pages.at(to.above, width, to.zoom))
        assertEquals(0f, to.panX, 0f)
    }

    /** Overscroll past the end must clamp there, not to the top of the last page. */
    @Test
    fun `a release past the end of the document stays past it`() {
        val end = pages.top(40, width, 3f)
        val (row, into) = pages.at(end + 500f, width, 3f)
        assertEquals(39, row)
        assertEquals(end + 500f, pages.top(row, width, 3f) + into, 1f)
    }

    /** At maximum zoom, a pinch drag pans without changing zoom. */
    @Test
    fun `a release at the ceiling keeps the zoom and moves with the hand`() {
        val hand = Offset(40f, -70f)
        val to = pages.released(50_000f, -3000f, Offset(500f, 1000f), hand, 1f, width, 8f, 8f)
        assertEquals(8f, to.zoom, 0f)
        assertEquals(-2960f, to.panX, 0.01f)
        assertEquals(50_070f, to.above, 0.5f)
    }
}
