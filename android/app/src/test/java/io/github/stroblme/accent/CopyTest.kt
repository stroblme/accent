package io.github.stroblme.accent

import io.github.stroblme.accent.ffi.AccentException
import java.io.ByteArrayInputStream
import java.io.File
import java.io.InputStream
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit.SECONDS
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertThrows
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder

/** [throughCopy] leaves nothing in its folder, whether the document opens or not. */
class CopyTest {
    @get:Rule
    val tmp = TemporaryFolder()

    private val pdf = ByteArray(100_000) { it.toByte() }

    private fun dir() = File(tmp.root, "pdf")

    private fun left() = dir().list()?.toList().orEmpty()

    @Test
    fun `a document is opened from its copy, which goes once it is open`() = runBlocking {
        // What a process killed while copying left behind.
        dir().mkdirs()
        File(dir(), "stale.pdf").writeText("half a scan")
        val read = throughCopy(dir(), { ByteArrayInputStream(pdf) }) { File(it).readBytes() }
        assertArrayEquals(pdf, read)
        assertEquals(emptyList<String>(), left())
    }

    @Test
    fun `a copy that does not open is deleted`() {
        assertThrows(AccentException.Failed::class.java) {
            runBlocking {
                throughCopy(dir(), { ByteArrayInputStream(pdf) }) { throw AccentException.Failed("damaged") }
            }
        }
        assertEquals(emptyList<String>(), left())
    }

    /** A provider that hands over one block, then waits for [go] before the next and the end. */
    private class Slow : InputStream() {
        val reading = CountDownLatch(1)
        val go = CountDownLatch(1)
        private var blocks = 0

        override fun read(): Int = error("read in blocks")

        override fun read(b: ByteArray, off: Int, len: Int): Int {
            if (blocks > 0) go.await(5, SECONDS)
            reading.countDown()
            return if (blocks++ < 2) len else -1
        }
    }

    @Test
    fun `a cancelled copy stops and is deleted`() = runBlocking {
        val stream = Slow()
        var opened = false
        val job = launch(Dispatchers.IO) { throughCopy(dir(), { stream }) { opened = true } }
        stream.reading.await(5, SECONDS)
        job.cancel()
        stream.go.countDown()
        job.join()
        assertFalse("a cancelled copy is never opened", opened)
        assertEquals(emptyList<String>(), left())
    }
}
