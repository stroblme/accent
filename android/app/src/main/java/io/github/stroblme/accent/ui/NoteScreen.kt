package io.github.stroblme.accent.ui

import android.graphics.Color as AndroidColor
import android.webkit.WebResourceRequest
import android.webkit.WebResourceResponse
import android.webkit.WebView
import android.webkit.WebViewClient
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.input.TextFieldBuffer
import androidx.compose.foundation.text.input.OutputTransformation
import androidx.compose.foundation.text.input.rememberTextFieldState
import androidx.compose.foundation.verticalScroll
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.luminance
import androidx.compose.ui.graphics.toArgb
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextDecoration
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.em
import androidx.compose.ui.viewinterop.AndroidView
import io.github.stroblme.accent.Open
import io.github.stroblme.accent.VaultModel
import io.github.stroblme.accent.ffi.Style
import io.github.stroblme.accent.ffi.analyzeUtf16
import io.github.stroblme.accent.ffi.toHtml
import kotlinx.coroutines.delay
import java.io.File
import java.net.URLDecoder

/**
 * One note, read or written.
 *
 * Reading is the default, because that is what a phone is mostly for here: the same HTML the
 * desktop preview renders, in a WebView, which is the only thing on the platform that draws
 * MathML. Writing is the same text with the same styling spans the desktop editor uses, applied
 * to the field's output rather than to a buffer of tags.
 */
@Composable
fun NoteScreen(model: VaultModel, open: Open, onMenu: () -> Unit, onPull: () -> Unit) {
    var editing by remember(open.rel) { mutableStateOf(false) }
    Column(Modifier.fillMaxSize()) {
        NoteBar(open, editing, onMenu, onToggle = { editing = !editing })
        open.conflicts.firstOrNull()?.let { conflict ->
            ConflictBanner(
                conflict = conflict,
                onMine = { model.keepMine(conflict) },
                onTheirs = { model.keepTheirs(conflict) },
            )
        }
        if (open.changedOnDisk) ChangedBanner(onReload = { model.reload() })
        Box(Modifier.weight(1f)) {
            if (editing) Editor(model, open) else Rendered(model, open, onPull)
        }
    }
}

@Composable
private fun NoteBar(open: Open, editing: Boolean, onMenu: () -> Unit, onToggle: () -> Unit) {
    Row(
        Modifier.fillMaxWidth().padding(horizontal = 4.dp, vertical = 4.dp),
        verticalAlignment = androidx.compose.ui.Alignment.CenterVertically,
    ) {
        TextButton(onClick = onMenu) { Text("Files") }
        Text(
            File(open.rel).nameWithoutExtension,
            style = MaterialTheme.typography.titleMedium,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
            modifier = Modifier.weight(1f).padding(horizontal = 8.dp),
        )
        TextButton(onClick = onToggle) { Text(if (editing) "Done" else "Edit") }
    }
}

@Composable
private fun ConflictBanner(conflict: String, onMine: () -> Unit, onTheirs: () -> Unit) {
    Column(Modifier.fillMaxWidth().padding(horizontal = Gutter, vertical = 8.dp)) {
        Text("This note was edited on two devices.", style = MaterialTheme.typography.bodyMedium)
        Row {
            TextButton(onClick = onMine) { Text("Keep mine") }
            TextButton(onClick = onTheirs) { Text("Keep theirs") }
        }
    }
    HorizontalDivider()
}

@Composable
private fun ChangedBanner(onReload: () -> Unit) {
    Column(Modifier.fillMaxWidth().padding(horizontal = Gutter, vertical = 8.dp)) {
        Text(
            "This note changed on disk. Saving is paused so your edits are not lost.",
            style = MaterialTheme.typography.bodyMedium,
        )
        TextButton(onClick = onReload) { Text("Take the version on disk") }
    }
    HorizontalDivider()
}

// ------------------------------------------------------------------------------------ reading

/**
 * The rendered note.
 *
 * `accent://open/…` is a link to another note and is handed back to the app; `accent://file/…` is
 * an image, served off the vault. Nothing else loads at all — the same rule the desktop preview
 * enforces with a content blocker.
 */
@Composable
private fun Rendered(model: VaultModel, open: Open, onPull: () -> Unit) {
    val context = LocalContext.current
    val colors = MaterialTheme.colorScheme
    val root = model.state.collectAsState().value.root.orEmpty()
    var atTop by remember { mutableStateOf(true) }

    AndroidView(
        modifier = Modifier.fillMaxSize().pullDown(atTop = { atTop }, onPull = onPull),
        factory = { ctx ->
            WebView(ctx).apply {
                settings.javaScriptEnabled = false
                settings.allowFileAccess = false
                settings.allowContentAccess = false
                setBackgroundColor(AndroidColor.TRANSPARENT)
                webViewClient = object : WebViewClient() {
                    override fun shouldOverrideUrlLoading(
                        view: WebView,
                        request: WebResourceRequest,
                    ): Boolean {
                        val url = request.url.toString()
                        if (!url.startsWith("accent://open/")) return true
                        val target = decode(url.removePrefix("accent://open/"))
                        model.openLink(target)
                        return true
                    }

                    override fun shouldInterceptRequest(
                        view: WebView,
                        request: WebResourceRequest,
                    ): WebResourceResponse? {
                        val url = request.url.toString()
                        if (!url.startsWith("accent://file/")) return blocked()
                        val rel = decode(url.removePrefix("accent://file/"))
                        val file = File(root, rel)
                        return runCatching {
                            WebResourceResponse(null, null, file.inputStream())
                        }.getOrElse { blocked() }
                    }
                }
                setOnScrollChangeListener { _, _, y, _, _ -> atTop = y <= 0 }
            }
        },
        update = { web ->
            web.loadDataWithBaseURL(
                "accent://file/",
                page(toHtml(open.text), colors.onSurface, colors.surface, colors.primary),
                "text/html",
                "utf-8",
                null,
            )
        },
    )
}

private fun blocked() = WebResourceResponse(null, null, null)

private fun decode(s: String): String = runCatching { URLDecoder.decode(s, "UTF-8") }.getOrDefault(s)

/**
 * The shell around `to_html`'s fragment.
 *
 * The colours are handed in rather than read from a stylesheet, exactly as the desktop does:
 * a WebView cannot see the app's palette, and the palette is the system's.
 */
private fun page(body: String, fg: Color, bg: Color, accent: Color): String = """
<!doctype html><html><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<style>
  :root { color-scheme: ${if (bg.dark()) "dark" else "light"}; }
  body {
    margin: 0 16px 96px; color: ${fg.css()}; background: ${bg.css()};
    font-family: system-ui, sans-serif; font-size: 16px; line-height: 1.6;
    overflow-wrap: break-word;
  }
  a { color: ${accent.css()}; text-decoration: none; }
  h1,h2,h3,h4,h5,h6 { line-height: 1.25; margin: 1.4em 0 0.5em; }
  h1 { font-size: 1.6em } h2 { font-size: 1.4em } h3 { font-size: 1.2em }
  img { max-width: 100%; height: auto; }
  pre, code { font-family: ui-monospace, monospace; font-size: 0.9em; }
  pre { overflow-x: auto; padding: 12px 0; }
  blockquote { margin: 1em 0; padding-left: 12px; border-left: 2px solid ${accent.css()}; }
  table { border-collapse: collapse; display: block; overflow-x: auto; }
  th, td { padding: 4px 10px 4px 0; text-align: left; }
  math[display="block"] { margin: 1.2em 0; }
  hr { border: 0; border-top: 1px solid ${fg.css()}; opacity: 0.15; }
</style></head><body>$body</body></html>
"""

private fun Color.css(): String = String.format("#%06X", 0xFFFFFF and toArgb())

private fun Color.dark(): Boolean = luminance() < 0.5f

// ------------------------------------------------------------------------------------ writing

/**
 * The editor: the note's own text, styled from the core's spans.
 *
 * The markup stays visible and is dimmed rather than hidden — Apostrophe's rule, and the
 * desktop's — because a phone keyboard has no way to put back a character it cannot see.
 * Saving is on a pause in the typing, the same second the desktop waits.
 */
@Composable
private fun Editor(model: VaultModel, open: Open) {
    val field = rememberTextFieldState(open.text)
    val colors = MaterialTheme.colorScheme
    val styling = remember(colors) { Styling(colors.onSurface, colors.primary, colors.onSurfaceVariant) }

    LaunchedEffect(open.rel) {
        snapshotFlow { field.text.toString() }.collect { text ->
            if (text == open.text) return@collect
            delay(1000)
            model.save(text)
        }
    }

    BasicTextField(
        state = field,
        modifier = Modifier.fillMaxSize().padding(horizontal = Gutter).verticalScroll(rememberScrollState()),
        textStyle = MaterialTheme.typography.bodyLarge.copy(color = colors.onSurface),
        cursorBrush = androidx.compose.ui.graphics.SolidColor(colors.primary),
        outputTransformation = styling,
    )
}

/** Applies the core's spans to the field's output; nothing it does reaches the saved text. */
private class Styling(
    private val fg: Color,
    private val accent: Color,
    private val muted: Color,
) : OutputTransformation {
    override fun TextFieldBuffer.transformOutput() {
        val analysis = runCatching { analyzeUtf16(asCharSequence().toString()) }.getOrNull() ?: return
        for (span in analysis.spans) {
            val style = styleOf(span.style) ?: continue
            val start = span.range.start.toInt().coerceIn(0, length)
            val end = span.range.end.toInt().coerceIn(start, length)
            if (start < end) addStyle(style, start, end)
        }
    }

    private fun styleOf(style: Style): SpanStyle? = when (style) {
        // The syntax characters themselves: still there, just out of the way.
        is Style.Marker -> SpanStyle(color = muted)
        is Style.Heading -> SpanStyle(
            fontWeight = FontWeight.SemiBold,
            fontSize = when (style.v1.toInt()) {
                1 -> 1.6.em
                2 -> 1.4.em
                3 -> 1.2.em
                else -> 1.1.em
            },
        )
        is Style.Emphasis -> SpanStyle(fontStyle = FontStyle.Italic)
        is Style.Strong -> SpanStyle(fontWeight = FontWeight.Bold)
        is Style.Strikethrough -> SpanStyle(textDecoration = TextDecoration.LineThrough)
        is Style.CodeInline, is Style.CodeBlock, is Style.Math ->
            SpanStyle(fontFamily = FontFamily.Monospace, fontSize = 0.9.em)
        is Style.Link, is Style.WikiLink, is Style.Image -> SpanStyle(color = accent)
        is Style.Tag -> SpanStyle(color = accent)
        is Style.Quote -> SpanStyle(color = muted)
        is Style.ListMarker -> SpanStyle(color = accent)
        is Style.TaskMarker -> SpanStyle(color = if (style.checked) muted else accent)
        is Style.Frontmatter, is Style.Html -> SpanStyle(color = muted, fontFamily = FontFamily.Monospace)
    }
}
