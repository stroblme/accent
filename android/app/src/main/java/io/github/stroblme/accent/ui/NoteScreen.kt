package io.github.stroblme.accent.ui

import android.graphics.Color as AndroidColor
import android.net.Uri
import androidx.activity.compose.BackHandler
import android.webkit.WebResourceRequest
import android.webkit.WebResourceResponse
import android.webkit.WebView
import android.webkit.WebView.HitTestResult
import android.webkit.WebViewClient
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.input.TextFieldBuffer
import androidx.compose.foundation.text.input.OutputTransformation
import androidx.compose.foundation.verticalScroll
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.toArgb
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextDecoration
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.em
import androidx.compose.ui.viewinterop.AndroidView
import io.github.stroblme.accent.Open
import io.github.stroblme.accent.OpenImage
import io.github.stroblme.accent.VaultModel
import io.github.stroblme.accent.ffi.Click
import io.github.stroblme.accent.ffi.Span
import io.github.stroblme.accent.ffi.Style
import io.github.stroblme.accent.ffi.accentFile
import io.github.stroblme.accent.ffi.analyzeUtf16
import io.github.stroblme.accent.ffi.baseUri
import io.github.stroblme.accent.ffi.click
import io.github.stroblme.accent.ffi.toHtml
import java.io.File
import java.util.Locale
import java.util.UUID
import java.util.concurrent.ConcurrentHashMap
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import kotlin.math.roundToInt

/** Display a note as rendered HTML or editable Markdown with core styling spans. */
@Composable
fun NoteScreen(model: VaultModel, open: Open, chrome: Chrome, onTag: (String) -> Unit) {
    var editing by remember(open.rel) { mutableStateOf(false) }
    var details by remember(open.rel) { mutableStateOf(false) }
    val context = LocalContext.current
    Column(Modifier.fillMaxSize()) {
        // Above the bar rather than under it, since the bar lies over the note and would cover
        // them. They never fade, so one coming or going moves the note once, as a decision may.
        open.conflicts.firstOrNull()?.let { conflict ->
            ConflictBanner(
                conflict = conflict,
                onMine = { model.keepMine(conflict) },
                onTheirs = { model.keepTheirs(conflict) },
            )
        }
        if (open.changedOnDisk) {
            ChangedBanner(onKeep = { model.overwrite() }, onReload = { model.reload() })
        }
        if (open.leaving) {
            LeaveDialog(name = File(open.rel).name.removeSuffix(".md"), onAnswer = { model.answer(it) })
        }
        open.unsaved?.let { why -> UnsavedDialog(why = why, onAnswer = { model.answer(it) }) }
        DocumentFrame(
            // The bar goes with the rest of the chrome while reading, and never while writing:
            // Done is the only way out of the editor, so it has to stay where it can be reached.
            barShown = editing || chrome.shown,
            bar = {
                DocumentBar(title = File(open.rel).name.removeSuffix(".md")) {
                    TextButton(
                        onClick = {
                            // Now rather than on Done: the view built again then would load before
                            // the place reached it.
                            if (!editing) model.keepPlace()
                            editing = !editing
                        },
                    ) {
                        Text(if (editing) "Done" else "Edit")
                    }
                    DetailsMenu { details = true }
                }
            },
            modifier = Modifier.weight(1f),
        ) { bar ->
            // A bar that never goes has nothing to move, so the editor keeps clear of it rather
            // than have it lie across the line being typed — outside the scroll, or the caret
            // could be brought into view underneath it.
            if (editing) {
                Box(Modifier.padding(top = bar)) { Editor(model) }
            } else {
                Rendered(model, open, chrome, onTag)
            }
        }
    }
    if (details) DetailsSheet(onDismiss = { details = false }) {
        val text = model.buffer.text.toString()
        val stat = model.stat(open.rel)
        fileDetails(stat, context) + withContext(Dispatchers.Default) { noteDetails(text) }
    }
}

@Composable
private fun ConflictBanner(conflict: String, onMine: () -> Unit, onTheirs: () -> Unit) {
    Column(Modifier.fillMaxWidth().padding(horizontal = Gutter, vertical = 8.dp)) {
        Text("Sync found another version of this note.", style = MaterialTheme.typography.bodyMedium)
        Row {
            TextButton(onClick = onMine) { Text("Keep mine") }
            TextButton(onClick = onTheirs) { Text("Keep theirs") }
        }
    }
    HorizontalDivider()
}

@Composable
private fun ChangedBanner(onKeep: () -> Unit, onReload: () -> Unit) {
    Column(Modifier.fillMaxWidth().padding(horizontal = Gutter, vertical = 8.dp)) {
        Text(
            "This note changed on disk. Saving is paused until you choose a version.",
            style = MaterialTheme.typography.bodyMedium,
        )
        Row {
            TextButton(onClick = onKeep) { Text("Keep mine") }
            TextButton(onClick = onReload) { Text("Use saved version") }
        }
    }
    HorizontalDivider()
}

/** Ask how to handle edits paused by a save conflict before leaving the note. */
@Composable
private fun LeaveDialog(name: String, onAnswer: (Boolean?) -> Unit) {
    AlertDialog(
        onDismissRequest = { onAnswer(null) },
        title = { Text("Unsaved edits") },
        text = {
            Text("\"$name\" changed on disk while you were editing. Your edits have not been saved.")
        },
        confirmButton = {
            Column(horizontalAlignment = Alignment.End) {
                TextButton(onClick = { onAnswer(true) }) { Text("Keep mine") }
                TextButton(onClick = { onAnswer(false) }) { Text("Use saved version") }
                TextButton(onClick = { onAnswer(null) }) { Text("Cancel") }
            }
        },
    )
}

/** Ask whether to leave when edits could not be saved for [why]. */
@Composable
private fun UnsavedDialog(why: String, onAnswer: (Boolean?) -> Unit) {
    AlertDialog(
        onDismissRequest = { onAnswer(null) },
        title = { Text("Unsaved edits") },
        text = { Text("$why. Your edits have not been saved.") },
        confirmButton = { TextButton(onClick = { onAnswer(null) }) { Text("Stay") } },
        dismissButton = {
            TextButton(onClick = { onAnswer(false) }) { Text("Leave without saving") }
        },
    )
}

// ------------------------------------------------------------------------------------ reading

/**
 * Render a note with search highlights. Route app links locally, serve vault images through
 * [served], load [MERMAID] from the APK, and block other requests.
 */
@Composable
private fun Rendered(model: VaultModel, open: Open, chrome: Chrome, onTag: (String) -> Unit) {
    val colors = MaterialTheme.colorScheme
    // What the images are served under, read by the loading thread; a change in either loads the
    // page again, since an image is recoloured on its way into it.
    val dark = colors.surface.dark()
    val serving by rememberUpdatedState(dark)
    val inverted = Inverted.files
    // Rendered from the same buffer the editor writes into rather than from what the vault last
    // read, so Done shows what was typed instead of what was saved a second ago. The whole page is
    // rebuilt only when the text or the palette changes; everything else that recomposes this
    // screen — indexing progress, a snackbar, a search — must not reload the WebView, because a
    // reload is a scroll back to the top and a fling cut off mid-throw.
    val text = model.buffer.text
    val html = remember(text, colors) {
        page(toHtml(text.toString()), colors.onSurface, colors.surface, colors.primary)
    }
    // The view, and the page it has finished loading. Both are held here rather than read from
    // inside the client, which is built once and would keep whichever note was open then.
    var view by remember { mutableStateOf<WebView?>(null) }
    var loaded by remember { mutableStateOf<Load?>(null, referentialEqualityPolicy()) }
    // The file each image on the page was served from, by the address the page asks for it at:
    // what a tap on one opens and a long press inverts. Written on the loading thread, read on the
    // main one.
    val images = remember { ConcurrentHashMap<String, OpenImage>() }
    // What the reader has typed into the find bar, and where in the page it got them: which match
    // of how many, straight off the view's own find listener. Reset every time the bar opens.
    var query by remember(open.finding) { mutableStateOf("") }
    var matches by remember { mutableStateOf(0 to 0) }
    // Read inside the tap and the client below, which are captured once and so cannot close over
    // a parameter.
    val finding by rememberUpdatedState(open.finding)
    val tagged by rememberUpdatedState(onTag)

    // The reader's own find: every keystroke marks the page again, and an empty field — which is
    // where the bar opens, and what it leaves behind when it closes — takes the marks off. One
    // effect for the bar's whole life, so there is no "it has gone" to remember separately.
    LaunchedEffect(open.finding, query) {
        val web = view ?: return@LaunchedEffect
        if (query.isBlank()) web.clearMatches() else web.findAllAsync(query)
    }

    // Text can only be found once it is there to find, so the query waits for the load — and
    // since the page is loaded only when the note, the palette or an inverted image changes, a hit
    // in the note already in front is marked without one.
    LaunchedEffect(loaded, open.find) {
        val reveal = open.find ?: return@LaunchedEffect
        val web = view ?: return@LaunchedEffect
        if (loaded !== web.tag || loaded?.rel != open.rel || loaded?.html != html) return@LaunchedEffect
        web.findAllAsync(reveal)
        model.found()
    }

    // A cross-note link names an element in the newly rendered document. Wait for that load and
    // its first visual state, then let the WebView perform its normal fragment navigation.
    LaunchedEffect(loaded, open.anchor) {
        val anchor = open.anchor ?: return@LaunchedEffect
        val web = view ?: return@LaunchedEffect
        val load = loaded ?: return@LaunchedEffect
        if (load !== web.tag || load.rel != open.rel || load.html != html) return@LaunchedEffect
        web.postVisualStateCallback(0, object : WebView.VisualStateCallback() {
            override fun onComplete(requestId: Long) {
                if (web.tag !== load || model.state.value.note?.anchor != anchor) return
                load.scroll = 0
                web.loadUrl("${baseUri(load.rel)}#${Uri.encode(anchor)}")
                model.anchored(load.rel, anchor)
            }
        })
    }

    // Back from a backlink put the reader where they were ([Open.scroll]), and that is spent once
    // the page is up: the load has already taken it.
    LaunchedEffect(loaded, open.scroll) {
        if (open.scroll != 0 && loaded?.rel == open.rel && loaded?.html == html) model.placed()
    }

    // Back puts the bar away, which is how the panel closes too.
    BackHandler(enabled = open.finding) { model.finding(false) }

    // Print… from the palette: the note as it is typed, on paper. The request is taken back once
    // the print system has it, not before, which would cancel this on the way.
    val context = LocalContext.current
    LaunchedEffect(open.printing) {
        if (!open.printing) return@LaunchedEffect
        try {
            printNote(context, model, open.rel, text.toString(), paperAccent(colors))
        } finally {
            model.printing(false)
        }
    }

    Column(Modifier.fillMaxSize()) {
        AndroidView(
            modifier = Modifier.weight(1f).fillMaxWidth().onTap(
                chrome,
                // Not an image inside a link: the WebView follows that one on the same tap.
                claimed = { _ ->
                    val image = view?.imageHit(HitTestResult.IMAGE_TYPE)?.let { images[it] }
                    image?.let { model.openFile(it.rel) } != null
                },
            ) {
                if (!finding) view?.clearMatches()
            },
            factory = { ctx ->
                WebView(ctx).apply {
                    settings.javaScriptEnabled = false
                    settings.allowFileAccess = false
                    settings.allowContentAccess = false
                    setBackgroundColor(AndroidColor.TRANSPARENT)
                    webViewClient = object : WebViewClient() {
                        /** What is on the screen now, and so what can be searched. */
                        override fun onPageFinished(view: WebView, url: String) {
                            val load = view.tag as? Load ?: return
                            loaded = load
                            // Once the page has been drawn rather than now: the view scrolls no
                            // further than the content it has, and until then it has none.
                            if (load.scroll > 0) {
                                view.postVisualStateCallback(0, object : WebView.VisualStateCallback() {
                                    override fun onComplete(requestId: Long) {
                                        if (view.tag !== load) return
                                        view.scrollTo(0, load.scroll)
                                        // The reader did not scroll there: a note they are brought
                                        // back to arrives with its bar up, as any document does.
                                        if (load.back) chrome.show()
                                    }
                                })
                            }
                        }

                        /** Route a tapped link as the desktop preview does ([click]). */
                        override fun shouldOverrideUrlLoading(
                            view: WebView,
                            request: WebResourceRequest,
                        ): Boolean {
                            val url = request.url.toString()
                            val load = view.tag as? Load ?: return true
                            when (val to = click(url, baseUri(load.rel))) {
                                // An anchor into this note, which the view scrolls to itself.
                                Click.Scroll -> return false
                                is Click.Open -> model.openLink(to.v1)
                                is Click.Tag -> tagged(to.v1)
                                // Only for a tap: a refresh or a redirect launches nothing.
                                Click.Leave -> if (request.hasGesture()) {
                                    launchUri(view.context, url) { model.said(it) }
                                }
                                Click.Nowhere -> {}
                            }
                            return true
                        }

                        override fun shouldInterceptRequest(
                            view: WebView,
                            request: WebResourceRequest,
                        ): WebResourceResponse? =
                            pageRequest(view, request, model, serving, Inverted.files) { url, image ->
                                images[url] = image
                            }
                    }
                    // Taken only on an image, a linked one too, where it inverts; anywhere else,
                    // and on an animation, which is never recoloured, it is left to the view,
                    // whose long press is the selection.
                    setOnLongClickListener {
                        val hit = imageHit(HitTestResult.IMAGE_TYPE, HitTestResult.SRC_IMAGE_ANCHOR_TYPE)
                        val image = hit?.let { images[it] }?.takeUnless { it.animated }
                        image?.let { Inverted.toggle(it.path) } != null
                    }
                    setOnScrollChangeListener { _, _, y, _, was ->
                        chrome.scrolled((y - was).toFloat())
                        (tag as? Load)?.let { model.reading(it.rel, scroll = y) }
                    }
                    setFindListener { active, total, _ -> matches = active to total }
                    view = this
                }
            },
            update = { web ->
                // The view's own tag is what it last loaded: `update` runs on every recomposition and
                // only a different page, or the same one with its images served otherwise, is worth
                // a load. The note in front loaded again keeps the reader's place; another opens at
                // its top, or where Back brings the reader to ([Open.scroll]).
                val load = Load(open.rel, html, dark, inverted)
                val last = web.tag as? Load
                if (load != last) {
                    if (open.anchor != null) {
                        load.scroll = 0
                    } else if (last?.rel == open.rel) {
                        load.scroll = web.scrollY
                    } else {
                        load.scroll = open.scroll
                        load.back = open.scroll > 0
                    }
                    model.reading(open.rel, scroll = load.scroll)
                    web.tag = load
                    web.freshen(dark)
                    // Scripts only for a page with diagrams to draw, and there only the app's own.
                    web.settings.javaScriptEnabled = diagrams(html)
                    web.loadDataWithBaseURL(baseUri(open.rel), html, "text/html", "utf-8", null)
                }
            },
        )
        if (open.finding) {
            val (active, total) = matches
            FindBar(
                query = query,
                onQuery = { query = it },
                placeholder = "Find in this note",
                count = when {
                    query.isBlank() -> ""
                    total == 0 -> "None"
                    else -> "${active + 1}/$total"
                },
                // Off while there is nothing to step through, and off on an emptied field: the
                // count is whatever the last find reported, and the marks it counted have been
                // cleared.
                canStep = query.isNotBlank() && total > 1,
                onStep = { forward -> view?.findNext(forward) },
            )
        }
    }
}

/** Rendered note page and image theme; [scroll] and [back] control restoration, not identity. */
private data class Load(val rel: String, val html: String, val dark: Boolean, val inverted: Set<String>) {
    var scroll = 0
    var back = false
}

/** Capture the touched image URL before tap or long-press recognition. */
private fun WebView.imageHit(vararg types: Int): String? = hitTestResult.takeIf { it.type in types }?.extra

/** Allow only APK diagram scripts and themed vault images; block all other WebView requests. */
internal fun pageRequest(
    view: WebView,
    request: WebResourceRequest,
    model: VaultModel,
    dark: Boolean,
    inverted: Set<String>,
    onImage: (String, OpenImage) -> Unit = { _, _ -> },
): WebResourceResponse? {
    if (request.isForMainFrame) return null
    val url = request.url.toString()
    if (url in SCRIPTS) {
        return WebResourceResponse(
            "text/javascript",
            "utf-8",
            view.context.assets.open(url.substringAfterLast('/')),
        )
    }
    val image = accentFile(url)?.let { model.image(it) } ?: return blocked()
    onImage(url, image)
    return served(image, dark, inverted)
}

/** Wrap `to_html` in the current app palette and optional Mermaid scripts. */
internal fun page(body: String, fg: Color, bg: Color, accent: Color): String = """
<!doctype html><html><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
${if (diagrams(body)) mermaid(if (bg.dark()) "dark" else "neutral") else ""}
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
  /* `[/]`, a task in progress: the desktop preview's half-filled box (no checkbox state draws it). */
  input.in-progress {
    appearance: none; box-sizing: border-box; width: 12px; height: 12px;
    border: 2px solid ${accent.css()}; border-radius: 3px; opacity: 0.5;
    background: linear-gradient(to right, ${accent.css()} 50%, transparent 50%);
  }
  .conflict { margin: 1em 0; border-radius: 6px; overflow: auto; }
  .conflict > div { display: flow-root; padding: 0 12px; }
  .conflict-label { margin: 0 -12px; padding: 2px 12px; font-size: 0.85em; font-weight: 700; }
${conflictCss(fg)}
</style></head><body>$body</body></html>
"""

/** Style conflict blocks with desktop tints and marker captions ([conflictTints]). */
private fun conflictCss(ink: Color): String =
    listOf("current", "base", "incoming").zip(conflictTints(ink)).joinToString("\n") { (side, tint) ->
        "  .conflict-$side { background: ${tint.first.rgba()}; }\n" +
            "  .conflict-$side > .conflict-label { background: ${tint.second.rgba()}; }"
    }

internal fun Color.css(): String = String.format("#%06X", 0xFFFFFF and toArgb())

/** A colour with its alpha, which [css] drops. */
internal fun Color.rgba(): String = String.format(
    Locale.ROOT, "rgba(%d, %d, %d, %.2f)",
    (red * 255).roundToInt(), (green * 255).roundToInt(), (blue * 255).roundToInt(), alpha,
)

/** Whether rendered HTML holds a mermaid fence: pulldown-cmark's class on a fence's code. */
internal fun diagrams(html: String): Boolean = "<code class=\"language-mermaid\">" in html

/** Where a page asks for mermaid and its bootstrap, answered from the APK's assets. */
private const val MERMAID = "accent://app/mermaid.min.js"
private const val BOOTSTRAP = "accent://app/bootstrap.js"
private val SCRIPTS = setOf(MERMAID, BOOTSTRAP)

/**
 * Load Mermaid and its bootstrap with a fresh nonce, excluding note scripts.
 * Defer execution until rendering completes; printing waits for `accentDrawn`.
 */
private fun mermaid(theme: String): String {
    val nonce = UUID.randomUUID()
    return """
<meta http-equiv="Content-Security-Policy" content="script-src 'nonce-$nonce'">
<script nonce="$nonce" defer src="$MERMAID"></script>
<script nonce="$nonce" defer src="$BOOTSTRAP"></script>
<script nonce="$nonce">
document.addEventListener('DOMContentLoaded', function () {
  Promise.resolve(accentDiagrams('$theme')).then(function () { window.accentDrawn = true; });
});
</script>"""
}

// ------------------------------------------------------------------------------------ writing

/** Edit [VaultModel.buffer] with core styling spans while keeping Markdown markup visible. */
@Composable
private fun Editor(model: VaultModel) {
    val colors = MaterialTheme.colorScheme
    val styling = remember(colors) { Styling(colors.onSurface, colors.primary, colors.onSurfaceVariant) }

    BasicTextField(
        state = model.buffer,
        modifier = Modifier.fillMaxSize().padding(horizontal = Gutter).verticalScroll(rememberScrollState()),
        textStyle = MaterialTheme.typography.bodyLarge.copy(color = colors.onSurface),
        cursorBrush = androidx.compose.ui.graphics.SolidColor(colors.primary),
        outputTransformation = styling,
    )
}

/**
 * Style the field output without changing saved text; cache parsing by text to avoid work
 * on unrelated recompositions.
 */
private class Styling(
    private val fg: Color,
    private val accent: Color,
    private val muted: Color,
) : OutputTransformation {
    private var last: Pair<String, List<Span>>? = null

    override fun TextFieldBuffer.transformOutput() {
        val text = asCharSequence().toString()
        val spans = last?.takeIf { it.first == text }?.second
            ?: runCatching { analyzeUtf16(text).spans }.getOrNull()?.also { last = text to it }
            ?: return
        for (span in spans) {
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
        is Style.Underline -> SpanStyle(textDecoration = TextDecoration.Underline)
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
