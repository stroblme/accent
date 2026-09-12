package io.github.stroblme.accent

import android.graphics.Bitmap
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asImageBitmap
import io.github.stroblme.accent.ffi.InkStyle
import io.github.stroblme.accent.ffi.PageSize
import io.github.stroblme.accent.ffi.PdfSession
import io.github.stroblme.accent.ffi.Point
import io.github.stroblme.accent.ffi.Theme
import io.github.stroblme.accent.ffi.Tile
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.asCoroutineDispatcher
import kotlinx.coroutines.withContext
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
    var canUndo by mutableStateOf(false)
        private set
    var canRedo by mutableStateOf(false)
        private set

    suspend fun load() = on {
        sizes = session.pageSizes()
    }

    /** A whole page at [scale], which is what the reader sees until nothing better arrives. */
    suspend fun page(index: Int, scale: Float, theme: Theme): ImageBitmap? = on {
        runCatching { session.renderPage(index.toUInt(), scale, theme).bitmap() }.getOrNull()
    }

    suspend fun tile(index: Int, scale: Float, x: Int, y: Int, w: Int, h: Int, theme: Theme): ImageBitmap? =
        on {
            runCatching { session.renderTile(index.toUInt(), scale, x, y, w, h, theme).bitmap() }
                .getOrNull()
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
    suspend fun erase(page: Int, from: Point, to: Point, radius: Float, partial: Boolean) = on {
        var joined = false
        while (true) {
            val hit = runCatching {
                session.eraseAt(page.toUInt(), from, to, radius, partial, joined)
            }.getOrNull() ?: break
            joined = true
            if (hit == null) break
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
    }

    private suspend fun <T> on(block: () -> T): T = withContext(dispatcher) { block() }

    override fun close() {
        worker.shutdown()
        session.close()
    }

    companion object {
        suspend fun open(path: String): PdfModel = withContext(Dispatchers.IO) {
            PdfModel(PdfSession.open(path)).also { it.load() }
        }

        suspend fun of(bytes: ByteArray): PdfModel = withContext(Dispatchers.IO) {
            PdfModel(PdfSession.openBytes(bytes)).also { it.load() }
        }
    }
}

/** The core hands over tightly packed RGBA8; Android wants a bitmap. */
private fun Tile.bitmap(): ImageBitmap {
    val pixels = IntArray(width.toInt() * height.toInt())
    for (i in pixels.indices) {
        val at = i * 4
        val r = rgba[at].toInt() and 0xFF
        val g = rgba[at + 1].toInt() and 0xFF
        val b = rgba[at + 2].toInt() and 0xFF
        val a = rgba[at + 3].toInt() and 0xFF
        pixels[i] = (a shl 24) or (r shl 16) or (g shl 8) or b
    }
    return Bitmap.createBitmap(pixels, width.toInt(), height.toInt(), Bitmap.Config.ARGB_8888)
        .asImageBitmap()
}
