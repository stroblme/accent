package io.github.stroblme.accent

import androidx.compose.foundation.text.input.TextFieldState
import androidx.compose.foundation.text.input.insert
import kotlinx.coroutines.async
import kotlinx.coroutines.delay
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Test

/** A note closing or swapped writes what was typed while its write was out ([settle]). */
class SettleTest {
    @Test
    fun `words typed while a closing write is out are written before the note goes`() = runTest {
        val buffer = TextFieldState("typed")
        val disk = mutableListOf<String>()
        val closing = async {
            settle({ buffer.text.toString() }) { text ->
                delay(1000)
                disk += text
                Saved.Written
            }
        }
        delay(500)
        buffer.edit { insert(length, " and more") }
        assertEquals(Saved.Written, closing.await())
        assertEquals(listOf("typed", "typed and more"), disk)
    }
}
