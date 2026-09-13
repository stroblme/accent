package io.github.stroblme.accent

import io.github.stroblme.accent.ui.anchor
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
}
