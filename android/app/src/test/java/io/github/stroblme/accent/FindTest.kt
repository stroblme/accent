package io.github.stroblme.accent

import io.github.stroblme.accent.ffi.Rect
import io.github.stroblme.accent.ui.Found
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/**
 * A find in a PDF, as far as it is arithmetic: the matches kept in page order while the pages
 * arrive out of it, and the steps round the ends. Where a match is brought on screen is
 * `NavigationTest`'s; the bar and the keyboard want a device.
 */
class FindTest {
    private fun box(y: Float) = Rect(0f, y, 10f, y + 10f)

    /**
     * A find starts at the page being read and comes round to the ones before it, which arrive
     * last and go in front — without moving the reader off the match they are on.
     */
    @Test
    fun `pages arriving round the end go in front and the reader stays put`() {
        var found = Found().plus(3, listOf(box(0f), box(20f))).plus(5, listOf(box(0f)))
        found = found.copy(at = found.from(3))
        assertEquals(0, found.at)
        found = found.plus(1, listOf(box(0f)))
        assertEquals(listOf(1, 3, 3, 5), found.hits.map { it.page })
        assertEquals(1, found.at)
        assertEquals(3, found.hits[found.at!!].page)
    }

    @Test
    fun `the first match at or after the page being read is the one landed on`() {
        val found = Found().plus(2, listOf(box(0f))).plus(7, listOf(box(0f)))
        assertEquals(1, found.from(4))
        assertEquals(0, found.from(2))
        assertNull(found.from(8))
    }

    @Test
    fun `steps go round both ends`() {
        val found = Found().plus(0, listOf(box(0f), box(20f), box(40f)))
        assertEquals(0, found.step(true).at)
        assertEquals(2, found.step(false).at)
        assertEquals(0, found.copy(at = 2).step(true).at)
        assertEquals(2, found.copy(at = 0).step(false).at)
        assertEquals(Found(), Found().step(true))
    }

    @Test
    fun `the count says where the reader is, and None only once every page is searched`() {
        val found = Found().plus(0, listOf(box(0f), box(20f)))
        assertEquals("2", found.count(done = false))
        assertEquals("2/2", found.copy(at = 1).count(done = true))
        assertEquals("", Found().count(done = false))
        assertEquals("None", Found().count(done = true))
    }
}
