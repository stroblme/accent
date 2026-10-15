package io.github.stroblme.accent

import io.github.stroblme.accent.ffi.AccentException
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/** Keep theirs takes the conflict copy as the note, then removes the copy ([takeTheirs]). */
class ConflictTest {
    private val calls = mutableListOf<String>()

    @Test
    fun `keeping theirs removes the incoming copy after adopting it`() {
        assertEquals(Theirs.Taken, takeTheirs({ calls += "adopt" }, { calls += "remove" }))
        assertEquals(listOf("adopt", "remove"), calls)
    }

    @Test
    fun `a failed adoption removes nothing`() {
        val full = AccentException.Failed("disk full")
        assertEquals(Theirs.Failed(full), takeTheirs({ throw full }, { calls += "remove" }))
        assertTrue(calls.isEmpty())
    }

    @Test
    fun `a copy that cannot be removed is reported and the note is still taken`() {
        val denied = AccentException.Failed("permission denied")
        assertEquals(Theirs.CopyKept(denied), takeTheirs({ calls += "adopt" }, { throw denied }))
        assertEquals(listOf("adopt"), calls)
    }
}
