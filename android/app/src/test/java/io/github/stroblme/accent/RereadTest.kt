package io.github.stroblme.accent

import io.github.stroblme.accent.ffi.Etag
import io.github.stroblme.accent.ffi.Note
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** A note changed on disk is read again ([reread]) and taken only where that loses nothing. */
class RereadTest {
    private val first = Etag(1, 7uL, 1uL)
    private val second = Etag(2, 8uL, 1uL)
    private val asked = Open("a.md", "on disk", first)

    @Test
    fun `a save that lands during the read wins`() {
        val saved = asked.copy(text = "typed", etag = second)
        assertNull(reread(asked, saved, Note("on disk", first), "typed"))
    }

    @Test
    fun `a note nobody typed into takes the text read`() {
        val now = reread(asked, asked, Note("theirs", second), "on disk")!!
        assertEquals("theirs", now.text)
        assertEquals(second, now.etag)
    }

    @Test
    fun `a note with edits is marked changed on disk and keeps them`() {
        val now = reread(asked, asked, Note("theirs", second), "on disk and typed")!!
        assertTrue(now.changedOnDisk)
        assertEquals("on disk", now.text)
    }

    @Test
    fun `a file only touched takes its new etag and nothing else`() {
        assertEquals(asked.copy(etag = second), reread(asked, asked, Note("on disk", second), "x"))
    }

    @Test
    fun `another note in front is left alone`() {
        assertNull(reread(asked, Open("b.md", "on disk", first), Note("theirs", second), "on disk"))
        assertNull(reread(asked, null, Note("theirs", second), "on disk"))
    }
}
