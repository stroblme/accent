package io.github.stroblme.accent

import android.graphics.Bitmap
import android.util.LruCache
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asImageBitmap
import io.github.stroblme.accent.ffi.Comment
import io.github.stroblme.accent.ffi.Glyph
import io.github.stroblme.accent.ffi.InkStyle
import io.github.stroblme.accent.ffi.LinkHighlight
import io.github.stroblme.accent.ffi.Located
import io.github.stroblme.accent.ffi.Outline
import io.github.stroblme.accent.ffi.PageSize
import io.github.stroblme.accent.ffi.PdfLink
import io.github.stroblme.accent.ffi.PdfLinkBox
import io.github.stroblme.accent.ffi.PdfSession
import io.github.stroblme.accent.ffi.Point
import io.github.stroblme.accent.ffi.Rect
import io.github.stroblme.accent.ffi.Theme
import io.github.stroblme.accent.ffi.Tile
import io.github.stroblme.accent.ffi.linkWithAlias
import io.github.stroblme.accent.ui.union
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.asCoroutineDispatcher
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.isActive
import kotlinx.coroutines.withContext
import java.nio.ByteBuffer
import java.util.concurrent.Executors

/**
 * Serialize one PDF's core calls on a dedicated executor to preserve rendering and stroke order.
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

    /** Change revision used to restart autosave delay after each edit. */
    var revision by mutableIntStateOf(0)
        private set
    var canUndo by mutableStateOf(false)
        private set
    var canRedo by mutableStateOf(false)
        private set

    suspend fun load() = on {
        sizes = session.pageSizes()
    }

    /** Cache whole pages using one-eighth of the app heap; viewport tiles remain uncached. */
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

    /** Return flat bookmarks with depth, or an empty list when none can be read. */
    suspend fun outline(): List<Outline> = on {
        runCatching { session.outline() }.getOrDefault(emptyList())
    }

    /** The `/Link` boxes on a page, in page points. Uncached: a read is cheap beside a render. */
    suspend fun links(index: Int): List<PdfLinkBox> = on {
        runCatching { session.links(index.toUInt()) }.getOrDefault(emptyList())
    }

    /** What other readers wrote on a page, each over its areas in page points. Never written. */
    suspend fun comments(index: Int): List<Comment> = on {
        runCatching { session.comments(index.toUInt()) }.getOrDefault(emptyList())
    }

    /** Every character of a page with its box, in reading order: what a selection is made of. */
    suspend fun glyphs(index: Int): List<Glyph> = on {
        runCatching { session.glyphs(index.toUInt()) }.getOrDefault(emptyList())
    }

    /** The glyphs a link's four numbers cover on [index] today, or `null` if they fit no line. */
    suspend fun locate(index: Int, numbers: List<UInt>): Located? = on {
        runCatching { session.locate(index.toUInt(), numbers) }.getOrNull()
    }

    /**
     * Build a selection link from page [index] glyphs [start] through [end], using text as alias.
     */
    suspend fun link(name: String, index: Int, start: Int, end: Int): String? = on {
        runCatching { session.selectionLink(name, index.toUInt(), start.toUInt(), end.toUInt()) }
            .map { linkWithAlias(it.link, it.text) }
            .getOrNull()
    }

    /** Find [text] on page [index], returning boxes one page at a time. */
    suspend fun search(index: Int, text: String): List<Rect> = on {
        runCatching { session.search(index.toUInt(), text) }.getOrDefault(emptyList())
            .mapNotNull { hit -> hit.quads.reduceOrNull { a, b -> a.union(b) } }
    }

    /** Resolve current destinations for [links], keyed by their list indexes. */
    suspend fun highlights(links: List<PdfLink>): List<LinkHighlight> = on {
        session.linkHighlights(links)
    }

    suspend fun stroke(page: Int, points: List<Point>, style: InkStyle) = on {
        runCatching { session.addStroke(page.toUInt(), points, style) }
        readHistory()
    }

    /** Erase all strokes along a drag as one undo gesture. */
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

    /**
     * Return a print copy with note [links] embedded as [rgba] highlights; leave the source unchanged.
     */
    suspend fun printCopy(links: List<PdfLink>, rgba: UInt): Result<ByteArray> = on {
        runCatching { session.copyWithHighlights(links, rgba) }
    }

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

        /** Close a loaded PDF if its waiting coroutine was cancelled before receiving it. */
        private suspend fun PdfModel.orClose() {
            load()
            if (!currentCoroutineContext().isActive) close()
        }
    }
}

/** Copy core RGBA8 bytes into `ARGB_8888`; little-endian layout matches without conversion. */
private fun Tile.bitmap(): ImageBitmap =
    Bitmap.createBitmap(width.toInt(), height.toInt(), Bitmap.Config.ARGB_8888)
        .apply { copyPixelsFromBuffer(ByteBuffer.wrap(rgba)) }
        .asImageBitmap()
