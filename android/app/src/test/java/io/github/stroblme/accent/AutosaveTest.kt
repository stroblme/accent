package io.github.stroblme.accent

import kotlinx.coroutines.delay
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Test

/** A save waiting out a pause in the typing is dropped by a file's text loaded into the buffer. */
class AutosaveTest {
    @Test
    fun `words typed into the note before are not saved over the note loaded after them`() = runTest {
        val saved = mutableListOf<String>()
        val autosave = Autosave(backgroundScope, 1000) { saved += it }
        autosave.typed("the note before")
        delay(500)
        // Another note's text goes in a frame before the snapshot says so.
        autosave.loaded()
        delay(1000)
        assertEquals(emptyList<String>(), saved)

        autosave.typed("the note now")
        delay(1001)
        assertEquals(listOf("the note now"), saved)
    }
}
