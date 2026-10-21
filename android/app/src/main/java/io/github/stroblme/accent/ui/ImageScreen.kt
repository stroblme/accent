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
import io.github.stroblme.accent.VaultModel
import java.io.File
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.platform.LocalContext
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

/**
 * Show an image with the document bar and pinch zoom. [served] applies note image theming;
 * Invert overrides it for this session.
 */
@Composable
fun ImageScreen(model: VaultModel, image: OpenImage, chrome: Chrome) {
    var details by remember(image.rel) { mutableStateOf(false) }
    val context = LocalContext.current
    val colors = MaterialTheme.colorScheme
    val name = File(image.rel).name
    val dark = colors.surface.dark()
    val inverted = Inverted.files
    // Read by the loading thread, whose client is built once with the view.
    val serving by rememberUpdatedState(dark)
    val shown by rememberUpdatedState(image)
    DocumentFrame(
        barShown = chrome.shown,
        bar = {
            DocumentBar(name) {
                // An animation is never recoloured, so there is nothing to invert: disabled rather
                // than gone, as Contents is on a PDF with no bookmarks.
                BarToggle(
                    "Invert",
                    on = image.path in inverted,
                    onClick = { Inverted.toggle(image.path) },
                    enabled = !image.animated,
                )
                DetailsMenu { details = true }
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
                            if (request.isForMainFrame) null else served(shown, serving)
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
    if (details) DetailsSheet(onDismiss = { details = false }) {
        val stat = model.stat(image.rel)
        fileDetails(stat, context) + withContext(Dispatchers.IO) { imageDetails(image.path) }
    }
}

/** Fit the image to the screen, enlarging small images and sizing dimensionless SVGs. */
private fun imagePage(src: String, bg: Color): String = """
<!doctype html><html><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<style>
  :root { color-scheme: ${if (bg.dark()) "dark" else "light"}; }
  html, body { margin: 0; height: 100%; background: ${bg.css()}; }
  img { display: block; width: 100%; height: 100%; object-fit: contain; }
</style></head><body><img src="$src"></body></html>
"""
