package io.github.stroblme.accent

import io.github.stroblme.accent.ffi.AccentException
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** A query that failed says so ([failure]), rather than passing for one that found nothing. */
class FailureTest {
    @Test
    fun `a failed search is not an empty answer`() {
        val locked = AccentException.Failed("index is locked")
        val failed = VaultState().answered("q", Result.failure(locked))
        assertEquals("Cannot search: index is locked", failed.searchFailed)
        assertEquals("q", failed.searched)
        assertTrue(failed.results.isEmpty())

        assertNull(failed.answered("q", Result.success(emptyList())).searchFailed)
    }

    @Test
    fun `a failure says what and why`() {
        assertEquals(
            "Cannot save this note: disk full",
            failure("Cannot save this note", AccentException.Failed("disk full")),
        )
        assertEquals("Cannot save this note", failure("Cannot save this note", RuntimeException()))
    }
}
