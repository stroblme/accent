package io.github.stroblme.accent

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Test

/** Only the latest request's answer is applied ([Latest]), whichever answer lands first. */
class LatestTest {
    @Test
    fun `an answer to an older request arriving last is dropped`() {
        val shown = Latest()
        var front: String? = null
        val a = shown.next()
        val b = shown.next()
        if (shown.holds(b)) front = "b.md"
        if (shown.holds(a)) front = "a.md"
        assertEquals("b.md", front)
    }

    @Test
    fun `closing the vault drops the read in flight`() {
        val shown = Latest()
        val read = shown.next()
        shown.next()
        assertFalse(shown.holds(read))
    }
}
