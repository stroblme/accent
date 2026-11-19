package io.github.stroblme.accent

import org.junit.Assert.assertEquals
import org.junit.Test

/** Files that appeared together count as used in their order, as the desktop's `files_used` does. */
class RecentsTest {
    @Test
    fun `the last file of a batch is the most recent`() {
        assertEquals(
            listOf("c.md", "b.md", "a.md", "old.md"),
            fronted(listOf("a.md", "old.md"), listOf("a.md", "b.md", "c.md"), 50),
        )
    }

    @Test
    fun `a batch keeps the list to its cap`() {
        assertEquals(listOf("new.md", "a.md"), fronted(listOf("a.md", "b.md"), listOf("new.md"), 2))
    }
}
