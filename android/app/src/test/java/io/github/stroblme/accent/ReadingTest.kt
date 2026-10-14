package io.github.stroblme.accent

import io.github.stroblme.accent.ffi.Phase
import io.github.stroblme.accent.ui.nothingFound
import io.github.stroblme.accent.ui.readingLine
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertNull
import org.junit.Test

/**
 * What Browse says while the index does not hold every file. A search that finds nothing during
 * the first walk used to look exactly like one that never will, which is the case this is for.
 */
class ReadingTest {
    @Test
    fun `the line is up exactly while a list may be missing files`() {
        assertEquals(
            "Reading your vault… 120 files so far. Some may not be listed yet.",
            readingLine(indexing = true, paused = false, scanned = 120),
        )
        // The rescan's scan, which counts nothing it could show.
        assertEquals(
            "Reading your vault… Some files may not be listed yet.",
            readingLine(indexing = true, paused = false, scanned = 0),
        )
        assertEquals(
            "Reading paused. Files not read yet are not listed.",
            readingLine(indexing = false, paused = true, scanned = 0),
        )
        assertNull(readingLine(indexing = false, paused = false, scanned = 0))
    }

    /**
     * A walk that fails, or whose events stop being read, puts down what drives the status line
     * and the service a walk holds, and leaves what the index already lets the reader do.
     */
    @Test
    fun `a walk that ends short is over`() {
        val walking = VaultState(root = "/v", indexing = true, scanned = 40, phase = Phase.INDEX)
        val over = walking.copy(ready = true).walkOver()
        assertEquals(false, over.indexing)
        assertEquals(false, over.paused)
        assertEquals(0L, over.scanned)
        assertNull(over.phase)
        assertEquals(true, over.ready)
    }

    /** The notification a walk holds, before it has a count and once it has one. */
    @Test
    fun `the notification names the vault and counts its files`() {
        assertEquals("Indexing Notes…", indexingText("Notes", 0))
        assertEquals("Indexing Notes… 120 files", indexingText("Notes", 120))
    }

    @Test
    fun `nothing found while reading is not nothing found`() {
        val done = nothingFound(indexing = false, paused = false)
        assertNotEquals(done, nothingFound(indexing = true, paused = false))
        assertNotEquals(done, nothingFound(indexing = false, paused = true))
    }
}
