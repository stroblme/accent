package io.github.stroblme.accent

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The stretch between a pick and the core handing the vault over, which the start screen shows as
 * the vault being read instead of leaving the picker up.
 *
 * Leaving the picker at once means the reader can close the vault, and pick again, before the
 * core has answered, and an answer nobody is waiting for any more has to be let go rather than
 * taken. [VaultState.waitsFor] is that decision and it is plain Kotlin; the screen needs a device.
 */
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
