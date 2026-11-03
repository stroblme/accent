package io.github.stroblme.accent

import io.github.stroblme.accent.ffi.Phase
import io.github.stroblme.accent.ui.nothingFound
import io.github.stroblme.accent.ui.readingLine
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertNull
import org.junit.Test

/** Browse distinguishes partial search results during indexing from complete empty results. */
class ReadingTest {
    /** A running walk says so with its bar alone; a stopped one, with no bar, in words. */
    @Test
    fun `the line is up only while a stopped walk leaves files out`() {
        assertNull(readingLine(paused = false))
        assertEquals("Indexing paused. Results may be incomplete.", readingLine(paused = true))
    }

    /** A failed or unobserved walk clears progress state while retaining indexed results. */
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

    /** A worker gone ends the walk as a failed one does, and says so until the vault reopens. */
    @Test
    fun `a worker gone ends the walk and is said until the vault reopens`() {
        val walking = VaultState(root = "/v", indexing = true, scanned = 40, phase = Phase.INDEX)
        val gone = walking.workerGone()
        assertEquals(false, gone.indexing)
        assertEquals(true, gone.gone)
        assertEquals(false, VaultState(root = "/v", opening = true).gone)
    }

    /** Indexing notification before and after the file count is known. */
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
