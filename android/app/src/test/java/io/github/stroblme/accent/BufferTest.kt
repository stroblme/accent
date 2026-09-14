package io.github.stroblme.accent

import androidx.compose.foundation.text.input.TextFieldState
import androidx.compose.foundation.text.input.insert
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The buffer a note is edited in, which lives on the model rather than inside the editor.
 *
 * Worth asserting because it is what was silently wrong: a field state built inside the editor
 * took its text once, so taking the version on disk cleared the banner and kept the old words.
 * [load] is the whole of the difference and it is plain Kotlin, so it can be driven here; the
 * editor around it needs a composition and a real vault and is left to a device.
 */
class BufferTest {
    @Test
    fun `loading a note replaces what was typed into the last one`() {
        val buffer = TextFieldState()
        buffer.load("on disk")
        buffer.edit { insert(length, " and typed") }
        assertEquals("on disk and typed", buffer.text.toString())

        buffer.load("the version on disk")
        assertEquals("the version on disk", buffer.text.toString())
    }

    @Test
    fun `a loaded note has nothing to undo back into`() {
        val buffer = TextFieldState()
        buffer.load("first note")
        buffer.edit { insert(length, "!") }
        assertTrue("an edit is an undo step", buffer.undoState.canUndo)

        buffer.load("second note")
        assertFalse("undo must not reach the note before", buffer.undoState.canUndo)
    }
}
