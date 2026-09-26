package io.github.stroblme.accent.ui

import android.graphics.Color as AndroidColor
import android.net.Uri
import android.webkit.WebResourceRequest
import android.webkit.WebResourceResponse
import android.webkit.WebView
import android.webkit.WebViewClient
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.viewinterop.AndroidView
import io.github.stroblme.accent.OpenImage
import java.io.File

/**
 * An image on its own: opened from the files, the switcher, a link, or a tap on it in a note.
 *
 * The note's surface — the same bar lying over it, the same tap for the bar — with the image fitted
 * to the screen, and recoloured by the rule a note's images follow ([served]). The bar's one
 * action is Invert: this file against that rule, for as long as the app runs, the same switch a
 * long press on the image in a note flips. A pinch zooms it, which is the WebView's own.
 *
 * A WebView rather than an `Image` because an SVG is drawn by nothing else on the platform, and its
 * recolouring is a filter only a browser applies; an animated GIF moves in one too.
 */
@Composable
fun ImageScreen(image: OpenImage, chrome: Chrome) {
    val colors = MaterialTheme.colorScheme
    val name = File(image.rel).name
    val dark = colors.surface.dark()
    val inverted = Inverted.files
    // Read by the loading thread, whose client is built once with the view.
    val serving by rememberUpdatedState(dark)
    val path by rememberUpdatedState(image.path)
    DocumentFrame(
        barShown = chrome.shown,
        bar = {
            DocumentBar(name) {
                // A GIF is never recoloured, so there is nothing to invert: disabled rather than
                // gone, as Contents is on a PDF with no bookmarks.
                BarToggle(
                    "Invert",
                    on = image.path in inverted,
                    onClick = { Inverted.toggle(image.path) },
                    enabled = imageKind(name) != ImageKind.Gif,
                )
            }
        },
    ) {
        AndroidView(
            modifier = Modifier.fillMaxSize().onTap(chrome),
            factory = { ctx ->
                WebView(ctx).apply {
                    settings.javaScriptEnabled = false
                    settings.allowFileAccess = false
                    settings.allowContentAccess = false
                    // The pinch, without the on-screen buttons that come with it.
                    settings.builtInZoomControls = true
                    settings.displayZoomControls = false
                    setBackgroundColor(AndroidColor.TRANSPARENT)
                    webViewClient = object : WebViewClient() {
                        override fun shouldOverrideUrlLoading(
                            view: WebView,
                            request: WebResourceRequest,
                        ): Boolean = true

                        /** The page asks for one thing, the image, and gets the file. */
                        override fun shouldInterceptRequest(
                            view: WebView,
                            request: WebResourceRequest,
                        ): WebResourceResponse? =
                            if (request.isForMainFrame) null else served(File(path), serving)
                    }
                }
            },
            update = { web ->
                // At its vault path: the WebViews share one cache, and an address two files could
                // answer to would be one image shown for the other.
                val page = imagePage("accent://file/" + Uri.encode(image.rel, "/"), colors.surface)
                val load = Triple(page, dark, inverted)
                if (web.tag != load) {
                    web.tag = load
                    web.freshen(dark)
                    web.loadDataWithBaseURL("accent://file/", page, "text/html", "utf-8", null)
                }
            },
        )
    }
}

/**
 * The image on the app's page, as large as the screen holds it whole: a small one is scaled up
 * rather than left a postage stamp in the middle, and an SVG with no size of its own gets one.
 */
private fun imagePage(src: String, bg: Color): String = """
<!doctype html><html><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<style>
  :root { color-scheme: ${if (bg.dark()) "dark" else "light"}; }
  html, body { margin: 0; height: 100%; background: ${bg.css()}; }
  img { display: block; width: 100%; height: 100%; object-fit: contain; }
</style></head><body><img src="$src"></body></html>
"""
