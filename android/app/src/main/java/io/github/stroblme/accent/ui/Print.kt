package io.github.stroblme.accent.ui

import android.content.Context
import android.os.Bundle
import android.os.CancellationSignal
import android.os.ParcelFileDescriptor
import android.print.PageRange
import android.print.PrintAttributes
import android.print.PrintDocumentAdapter
import android.print.PrintDocumentInfo
import android.print.PrintManager
import android.webkit.WebResourceRequest
import android.webkit.WebResourceResponse
import android.webkit.WebView
import android.webkit.WebViewClient
import androidx.compose.material3.ColorScheme
import androidx.compose.ui.graphics.Color
import io.github.stroblme.accent.VaultModel
import io.github.stroblme.accent.ffi.baseUri
import io.github.stroblme.accent.ffi.toHtml
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.delay
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlinx.coroutines.withTimeoutOrNull
import java.io.FileOutputStream
import kotlin.coroutines.resume

/**
 * Print the current note, including edits, on a light page through the system print UI.
 * Use a hidden WebView so the reader's scroll position and marks remain intact.
 */
suspend fun printNote(context: Context, model: VaultModel, rel: String, text: String, accent: Color) {
    val html = page(toHtml(text), InkLight, PageLight, accent)
    val view = WebView(context)
    view.settings.javaScriptEnabled = diagrams(html)
    view.settings.allowFileAccess = false
    view.settings.allowContentAccess = false
    val loaded = CompletableDeferred<Unit>()
    view.webViewClient = object : WebViewClient() {
        override fun onPageFinished(view: WebView, url: String) {
            loaded.complete(Unit)
        }

        override fun shouldOverrideUrlLoading(view: WebView, request: WebResourceRequest) = true

        override fun shouldInterceptRequest(
            view: WebView,
            request: WebResourceRequest,
        ): WebResourceResponse? = pageRequest(view, request, model, dark = false, inverted = emptySet())
    }
    view.freshen(dark = false, inverted = emptySet())
    view.loadDataWithBaseURL(baseUri(rel), html, "text/html", "utf-8", null)
    // The images are in once the page has loaded; the diagrams are drawn after it.
    val ready = withTimeoutOrNull(PATIENCE_MS) {
        loaded.await()
        while (diagrams(html) && view.evaluate("window.accentDrawn === true") != "true") delay(50)
    }
    if (ready == null) {
        view.destroy()
        return model.said("The note took too long to prepare for printing")
    }
    val name = title(rel)
    context.getSystemService(PrintManager::class.java)
        .print(name, view.createPrintDocumentAdapter(name), null)
}

/** Send [bytes], including note highlights, to the system print UI as vector pages. */
fun printPdf(context: Context, name: String, bytes: ByteArray, pages: Int) {
    context.getSystemService(PrintManager::class.java).print(name, PdfCopy(name, bytes, pages), null)
}

/** Use the light theme accent on paper. */
internal fun paperAccent(colors: ColorScheme): Color =
    if (colors.surface.dark()) colors.inversePrimary else colors.primary

/** Maximum time to wait for a printable page. */
private const val PATIENCE_MS = 30_000L

/** Decode a WebView script response. */
private suspend fun WebView.evaluate(script: String): String = suspendCancellableCoroutine { done ->
    evaluateJavascript(script) { done.resume(it) }
}

/** Print the laid-out PDF; the system scales pages and applies the selected page range. */
private class PdfCopy(
    private val name: String,
    private val bytes: ByteArray,
    private val pages: Int,
) : PrintDocumentAdapter() {
    override fun onLayout(
        oldAttributes: PrintAttributes?,
        newAttributes: PrintAttributes,
        cancellationSignal: CancellationSignal,
        callback: LayoutResultCallback,
        extras: Bundle?,
    ) {
        val info = PrintDocumentInfo.Builder(name)
            .setContentType(PrintDocumentInfo.CONTENT_TYPE_DOCUMENT)
            .setPageCount(pages)
            .build()
        callback.onLayoutFinished(info, oldAttributes == null)
    }

    override fun onWrite(
        pages: Array<out PageRange>,
        destination: ParcelFileDescriptor,
        cancellationSignal: CancellationSignal,
        callback: WriteResultCallback,
    ) {
        // Not closed here: the descriptor is the print system's, and it closes it.
        runCatching { FileOutputStream(destination.fileDescriptor).write(bytes) }
            .onSuccess { callback.onWriteFinished(arrayOf(PageRange.ALL_PAGES)) }
            .onFailure { callback.onWriteFailed(it.message) }
    }
}
