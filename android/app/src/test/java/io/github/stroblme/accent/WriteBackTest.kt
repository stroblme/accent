package io.github.stroblme.accent

import io.github.stroblme.accent.ffi.NoHandle
import io.github.stroblme.accent.ffi.PdfSession
import java.io.IOException
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/** [PdfModel.saveThrough] calls a document saved only once its bytes are written. */
class WriteBackTest {
    /** A session with no core behind it, which remembers being told it is saved. */
    private class Session : PdfSession(NoHandle) {
        var marked = false

        override fun pageCount(): UInt = 1u

        override fun saveBytes(): ByteArray = byteArrayOf(0x25, 0x50, 0x44, 0x46)

        override fun markSaved() {
            marked = true
        }
    }

    @Test
    fun `a write that fails leaves the document unsaved`() = runBlocking {
        val session = Session()
        PdfModel(session).use { model ->
            val saved = model.saveThrough { throw IOException("the provider has gone") }
            assertTrue(saved.isFailure)
            assertFalse("still unsaved", session.marked)
        }
    }
}
