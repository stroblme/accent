package io.github.stroblme.accent

import androidx.compose.foundation.text.input.TextFieldState
import androidx.compose.ui.text.TextRange
import io.github.stroblme.accent.ui.pasteFormatted
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/** A formatted paste writes what the core reads the HTML as, or leaves the paste to the field. */
class PasteTest {
    @Test
    fun `formatted text goes over the selection as one undo step`() {
        val buffer = TextFieldState("a x c", initialSelection = TextRange(2, 3))
        var asked: Pair<String, Int>? = null
        val pasted = buffer.pasteFormatted("<b>b</b>") { text, at, _ ->
            asked = text to at
            "**b**"
        }
        assertTrue(pasted)
        assertEquals("a x c" to 2, asked)
        assertEquals("a **b** c", buffer.text.toString())
        assertEquals(TextRange(7), buffer.selection)
        buffer.undoState.undo()
        assertEquals("a x c", buffer.text.toString())
    }

    @Test
    fun `without HTML or with nothing kept the field pastes the plain text`() {
        val buffer = TextFieldState("a x c")
        assertFalse(buffer.pasteFormatted(null) { _, _, _ -> "**b**" })
        assertFalse(buffer.pasteFormatted("plain") { _, _, _ -> null })
        assertEquals("a x c", buffer.text.toString())
        assertFalse(buffer.undoState.canUndo)
    }
}
