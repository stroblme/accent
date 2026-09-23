package io.github.stroblme.accent

import android.graphics.Bitmap
import android.util.LruCache
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asImageBitmap
import io.github.stroblme.accent.ffi.InkStyle
import io.github.stroblme.accent.ffi.Outline
import io.github.stroblme.accent.ffi.PageSize
import io.github.stroblme.accent.ffi.PdfLinkBox
import io.github.stroblme.accent.ffi.PdfSession
import io.github.stroblme.accent.ffi.Point
import io.github.stroblme.accent.ffi.Theme
import io.github.stroblme.accent.ffi.Tile
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.asCoroutineDispatcher
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.isActive
import kotlinx.coroutines.withContext
import java.nio.ByteBuffer
import java.util.concurrent.Executors

/**
 * One open document, and the single thread every call about it goes down.
 *
 * pdfium is serialised by one lock inside the core, so calling it from several threads only
 * queues them somewhere less useful. One executor per document keeps a stroke ordered after the
 * tile it was drawn over, which is what the desktop's render thread does; and since uniffi
 * cannot interrupt a call, a tile is the unit of work a caller can give up on.
 */
class PdfModel(private val session: PdfSession) : AutoCloseable {
    private val worker = Executors.newSingleThreadExecutor { r ->
        Thread(r, "accent-pdf").apply { isDaemon = true }
    }
    private val dispatcher = worker.asCoroutineDispatcher()

    val pageCount: Int = session.pageCount().toInt()
    var sizes: List<PageSize> = emptyList()
        private set

    var dirty by mutableStateOf(false)
        private set

    /**
     * How many changes this document has seen. Autosave waits on it rather than on [dirty], so a
     * second stroke restarts the wait instead of letting the first one's save land mid-drawing.
     */
    var revision by mutableIntStateOf(0)
        private set
    var canUndo by mutableStateOf(false)
        private set
    var canRedo by mutableStateOf(false)
        private set

    suspend fun load() = on {
        sizes = session.pageSizes()
    }

    /**
     * Pages already drawn, so scrolling back to one shows it instead of drawing it again.
     *
     * Sized against the heap the device hands this process rather than a number picked by hand: an
     * eighth of it, which is what Android's own bitmap-caching guidance spends. On the 256 MB a
     * phone usually gives that is 32 MB — five A4 pages at a 1080 px fit width, so about two
     * screens either side of the one being read survive a scroll away and back.
     *
     * Whole pages only. A tile is cut to the screen it was drawn for, so the next look at that page
     * wants a different rectangle of it, and keeping them would fill the budget with pixels nobody
     * asks for twice.
     */
    private val pages = object : LruCache<Drawn, ImageBitmap>(
        (Runtime.getRuntime().maxMemory() / 8).toInt(),
    ) {
        override fun sizeOf(key: Drawn, value: ImageBitmap) = value.width * value.height * 4
    }

    /** Everything about a whole-page render that decides what its pixels are. */
    private data class Drawn(val page: Int, val scale: Float, val theme: Theme)

    /** A whole page at [scale], which is what the reader sees until nothing better arrives. */
    suspend fun page(index: Int, scale: Float, theme: Theme): ImageBitmap? = on {
        val key = Drawn(index, scale, theme)
        pages[key]
            ?: runCatching { session.renderPage(index.toUInt(), scale, theme).bitmap() }
                .getOrNull()
                ?.also { pages.put(key, it) }
    }

    suspend fun tile(index: Int, scale: Float, x: Int, y: Int, w: Int, h: Int, theme: Theme): ImageBitmap? =
        on {
            runCatching { session.renderTile(index.toUInt(), scale, x, y, w, h, theme).bitmap() }
                .getOrNull()
        }

    /**
     * The document's bookmarks, flat, each carrying how deep it sits. A file with none answers an
     * empty list, and so does a file that will not say — there is nothing a reader could do about
     * the difference.
     */
    suspend fun outline(): List<Outline> = on {
        runCatching { session.outline() }.getOrDefault(emptyList())
    }

    /** The `/Link` boxes on a page, in page points. Uncached: a read is cheap beside a render. */
    suspend fun links(index: Int): List<PdfLinkBox> = on {
        runCatching { session.links(index.toUInt()) }.getOrDefault(emptyList())
    }

    suspend fun stroke(page: Int, points: List<Point>, style: InkStyle) = on {
        runCatching { session.addStroke(page.toUInt(), points, style) }
        readHistory()
    }

    /**
     * Rub out along a drag. The core answers one stroke at a time, so this asks until it says
     * there is nothing more under the line — and tells it after the first that the rest belong
     * to the same gesture, which is what makes one drag one Undo.
     */
    suspend fun erase(page: Int, points: List<Point>, radius: Float, partial: Boolean) = on {
        var joined = false
        for ((from, to) in points.zipWithNext()) {
            // Each segment may cross several strokes; the core answers one at a time and `null`
            // when the line is clear. `joined` from the first hit onwards, so the whole drag is
            // one step of the history however many strokes it took.
            while (true) {
                val hit = runCatching {
                    session.eraseAt(page.toUInt(), from, to, radius, partial, joined)
                }
                if (hit.isFailure || hit.getOrThrow() == null) break
                joined = true
            }
        }
        if (joined) readHistory()
    }

    suspend fun undo() = on { session.undo(); readHistory() }

    suspend fun redo() = on { session.redo(); readHistory() }

    /** Write the strokes back, refusing if the file moved under us. Its own error is the answer. */
    suspend fun save(): Result<Unit> = on {
        runCatching { session.save() }.map { readHistory() }
    }

    suspend fun bytes(): ByteArray? = on { runCatching { session.saveBytes() }.getOrNull() }

    private fun readHistory() {
        val history = session.history()
        canUndo = history.undo
        canRedo = history.redo
        dirty = session.dirty()
        revision++
        // A cached page under fresh ink is worse than one drawn again, and undo and redo do not
        // say which page they touched; a document changing is rare beside a scroll, so all of it
        // goes.
        pages.evictAll()
    }

    private suspend fun <T> on(block: () -> T): T = withContext(dispatcher) { block() }

    override fun close() {
        worker.shutdown()
        session.close()
    }

    companion object {
        suspend fun open(path: String): PdfModel = withContext(Dispatchers.IO) {
            PdfModel(PdfSession.open(path)).also { it.orClose() }
        }

        suspend fun of(bytes: ByteArray): PdfModel = withContext(Dispatchers.IO) {
            PdfModel(PdfSession.openBytes(bytes)).also { it.orClose() }
        }

        /**
         * Load, and put the document down again if nobody is waiting for it any more.
         *
         * A reader who leaves one PDF for another cancels this coroutine, and a cancelled
         * `withContext` throws its result away — which for a document is a session and a render
         * thread the caller never sees and so can never close.
         */
        private suspend fun PdfModel.orClose() {
            load()
            if (!currentCoroutineContext().isActive) close()
        }
    }
}

/**
 * The core hands over tightly packed RGBA8; Android wants a bitmap. One copy, not a loop.
 *
 * `ARGB_8888` is named for how a pixel packs into an `int`, not for how it lies in memory, and the
 * platform spells the memory out: "When accessing directly via #copyPixelsFromBuffer or
 * #copyPixelsToBuffer, use this formula to pack into 32 bits: `(A & 0xff) << 24 | (B & 0xff) << 16
 * | (G & 0xff) << 8 | (R & 0xff)`". Little-endian, that int is the bytes R, G, B, A in that order
 * — what the core already produces — and `copyPixelsFromBuffer` copies them without changing them.
 *
 * Which is also why it may: not converting means not premultiplying, and a page render is opaque.
 */
private fun Tile.bitmap(): ImageBitmap =
    Bitmap.createBitmap(width.toInt(), height.toInt(), Bitmap.Config.ARGB_8888)
        .apply { copyPixelsFromBuffer(ByteBuffer.wrap(rgba)) }
        .asImageBitmap()
