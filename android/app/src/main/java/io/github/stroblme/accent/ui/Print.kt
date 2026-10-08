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
import io.github.stroblme.accent.ffi.toHtml
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.delay
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlinx.coroutines.withTimeoutOrNull
import java.io.FileOutputStream
import kotlin.coroutines.resume

/**
 * Print… for a note, as the desktop prints one: the note as the reader has it, edits and all, on
 * the light page whatever the theme — links in the accent, images as their files are, diagrams in
 * mermaid's light theme — handed to the system's print UI, which paginates it for the paper picked
 * there and offers Save as PDF. A WebView of its own that is never shown, so the reader's page
 * keeps its scroll, its marks and the images it was served.
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
        return model.said("This note took too long to lay out for printing")
    }
    val name = title(rel)
    context.getSystemService(PrintManager::class.java)
        .print(name, view.createPrintDocumentAdapter(name), null)
}

/**
 * Print… for a PDF: [bytes], the copy of the document with the notes' highlights in it
 * ([io.github.stroblme.accent.PdfModel.printCopy]), handed to the system's print UI as it is —
 * vector pages, the print system doing ranges and copies itself, as the desktop's does.
 */
fun printPdf(context: Context, name: String, bytes: ByteArray, pages: Int) {
    context.getSystemService(PrintManager::class.java).print(name, PdfCopy(name, bytes, pages), null)
}

/** The accent on paper: the light theme's, whichever theme is showing. */
internal fun paperAccent(colors: ColorScheme): Color =
    if (colors.surface.dark()) colors.inversePrimary else colors.primary

/** How long a page may take to be ready to print before the print is given up. */
private const val PATIENCE_MS = 30_000L

/** What a script on the page answers, as the WebView spells it back. */
private suspend fun WebView.evaluate(script: String): String = suspendCancellableCoroutine { done ->
    evaluateJavascript(script) { done.resume(it) }
}

/**
 * A finished PDF, laid out once: its pages do not depend on the paper picked, so the system scales
 * them onto it, and every page is written whatever range is asked for, which the system then picks
 * from.
 */
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
