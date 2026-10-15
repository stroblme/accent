package io.github.stroblme.accent

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/** During vault opening, [VaultState.waitsFor] ignores results from a superseded pick. */
class OpeningTest {
    @Test
    fun `a pick waits for its own vault`() {
        assertTrue(VaultState(root = "/sdcard/Notes", opening = true).waitsFor("/sdcard/Notes"))
    }

    @Test
    fun `a vault nobody waits for any more is let go`() {
        assertFalse("closed: back on the picker", VaultState().waitsFor("/sdcard/Notes"))
        assertFalse(
            "another vault picked since",
            VaultState(root = "/sdcard/Other", opening = true).waitsFor("/sdcard/Notes"),
        )
        assertFalse(
            "picked twice, and the first answer already taken",
            VaultState(root = "/sdcard/Notes", indexing = true).waitsFor("/sdcard/Notes"),
        )
    }
}
