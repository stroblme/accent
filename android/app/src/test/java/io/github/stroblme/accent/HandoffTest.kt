package io.github.stroblme.accent

import io.github.stroblme.accent.ffi.AccentException
import io.github.stroblme.accent.ffi.NoHandle
import io.github.stroblme.accent.ffi.PageSize
import io.github.stroblme.accent.ffi.PdfSession
import java.util.concurrent.CountDownLatch
import java.util.concurrent.LinkedBlockingQueue
import java.util.concurrent.TimeUnit.SECONDS
import kotlin.coroutines.CoroutineContext
import kotlinx.coroutines.CoroutineDispatcher
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.launch
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/** [PdfModel.acquire] closes the session and the worker it made until its caller holds them. */
class HandoffTest {
    /** A session with no core behind it, whose page sizes are read once [go] opens. */
    private class Session(private val fails: Boolean = false) : PdfSession(NoHandle) {
        val reading = CountDownLatch(1)
        val go = CountDownLatch(1)

        /** The model's worker, which reads the page sizes. */
        @Volatile
        var worker: Thread? = null

        override fun pageCount(): UInt = 1u

        override fun pageSizes(): List<PageSize> {
            worker = Thread.currentThread()
            reading.countDown()
            go.await(5, SECONDS)
            if (fails) throw AccentException.Failed("unreadable")
            return listOf(PageSize(595f, 842f))
        }

        fun assertClosed() {
            assertTrue("the session is closed", uniffiIsDestroyed)
            worker!!.join(5000)
            assertFalse("the worker has stopped", worker!!.isAlive)
        }
    }

    /** The caller's thread, which runs what comes back only when the test says. */
    private class Caller : CoroutineDispatcher() {
        private val queue = LinkedBlockingQueue<Runnable>()

        override fun dispatch(context: CoroutineContext, block: Runnable) = queue.put(block)

        fun next(): Runnable = queue.poll(5, SECONDS) ?: error("nothing came back")
    }

    /** Start acquiring [session], up to its hop off the caller's thread. */
    private fun Caller.acquire(session: Session) =
        CoroutineScope(this).launch { runCatching { PdfModel.acquire { session } } }
            .also { next().run() }

    @Test
    fun `a model cancelled while it loads is closed`() {
        val caller = Caller()
        val session = Session()
        val job = caller.acquire(session)
        session.reading.await(5, SECONDS)
        job.cancel()
        session.go.countDown()
        caller.next().run()
        session.assertClosed()
    }

    @Test
    fun `a model cancelled as it is handed back is closed`() {
        val caller = Caller()
        val session = Session().apply { go.countDown() }
        val job = caller.acquire(session)
        // Loaded, and waiting for the caller's thread to take it.
        val back = caller.next()
        job.cancel()
        back.run()
        session.assertClosed()
    }

    @Test
    fun `a model that fails to load is closed`() {
        val caller = Caller()
        val session = Session(fails = true).apply { go.countDown() }
        caller.acquire(session)
        caller.next().run()
        session.assertClosed()
    }
}
