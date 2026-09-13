package io.github.stroblme.accent

import io.github.stroblme.accent.ui.anchor
import io.github.stroblme.accent.ui.liveShift
import org.junit.Assert.assertEquals
import org.junit.Test

/**
 * The arithmetic behind a pinch. It cannot be driven on an emulator — the Play Store image
 * refuses root, so there is no way to put a second finger on the screen — and it is exactly the
 * sort of thing that is wrong by a sign and looks almost right.
 */
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

    /**
     * The other half of the same sum: while the fingers are still down nothing is laid out again,
     * so a scaled layer has to be moved to where the arithmetic above says the reader will land.
     * What is drawn at [screen] must end up at `pivot + (screen - pivot) * live`.
     */
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

    /**
     * The two halves have to agree, or the page jumps at the moment the fingers come up: what the
     * layer showed during the pinch is what the laid-out column must show after it.
     *
     * Down the page, a point [into] pixels into the document with [above] of it past the top edge
     * is drawn at `into - above`. Afterwards the document is [live] times as long and the reader
     * is wherever `anchor` put them.
     */
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
}
