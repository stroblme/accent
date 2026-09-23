package io.github.stroblme.accent.ui

import android.graphics.Color as AndroidColor
import androidx.activity.compose.BackHandler
import android.webkit.WebResourceRequest
import android.webkit.WebResourceResponse
import android.webkit.WebView
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
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.toArgb
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
import io.github.stroblme.accent.VaultModel
import io.github.stroblme.accent.ffi.Span
import io.github.stroblme.accent.ffi.Style
import io.github.stroblme.accent.ffi.analyzeUtf16
import io.github.stroblme.accent.ffi.toHtml
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
fun NoteScreen(model: VaultModel, open: Open, root: String, chrome: Chrome) {
    var editing by remember(open.rel) { mutableStateOf(false) }
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
        DocumentFrame(
            // The bar goes with the rest of the chrome while reading, and never while writing:
            // Done is the only way out of the editor, so it has to stay where it can be reached.
            barShown = editing || chrome.shown,
            bar = {
                DocumentBar(
                    title = File(open.rel).name.removeSuffix(".md"),
                    action = if (editing) "Done" else "Edit",
                    onAction = { editing = !editing },
                )
            },
            modifier = Modifier.weight(1f),
        ) { bar ->
            // A bar that never goes has nothing to move, so the editor keeps clear of it rather
            // than have it lie across the line being typed — outside the scroll, or the caret
            // could be brought into view underneath it.
            if (editing) {
                Box(Modifier.padding(top = bar)) { Editor(model) }
            } else {
                Rendered(model, open, root, chrome)
            }
        }
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
private fun ChangedBanner(onKeep: () -> Unit, onReload: () -> Unit) {
    Column(Modifier.fillMaxWidth().padding(horizontal = Gutter, vertical = 8.dp)) {
        Text(
            "This note changed on disk. Saving is paused so your edits are not lost.",
            style = MaterialTheme.typography.bodyMedium,
        )
        Row {
            TextButton(onClick = onKeep) { Text("Keep mine") }
            TextButton(onClick = onReload) { Text("Take the version on disk") }
        }
    }
    HorizontalDivider()
}

/**
 * Leaving a note — closing it, closing the vault, opening another — over edits saving was paused
 * on, which would otherwise drop them with nothing said.
 *
 * The app's one dialog. A decision is a row above the note (MOBILE_DESIGN.md), but this one is
 * asked on the way out, and the row goes with the note it sits over. The choices are the banner's,
 * and Cancel stays with it; stacked, because three labels this long do not fit side by side on a
 * phone.
 */
@Composable
private fun LeaveDialog(name: String, onAnswer: (Boolean?) -> Unit) {
    AlertDialog(
        onDismissRequest = { onAnswer(null) },
        title = { Text("Unsaved edits") },
        text = {
            Text("\"$name\" changed on disk while you were editing it, so your edits have not been saved.")
        },
        confirmButton = {
            Column(horizontalAlignment = Alignment.End) {
                TextButton(onClick = { onAnswer(true) }) { Text("Keep mine") }
                TextButton(onClick = { onAnswer(false) }) { Text("Take the version on disk") }
                TextButton(onClick = { onAnswer(null) }) { Text("Cancel") }
            }
        },
    )
}

// ------------------------------------------------------------------------------------ reading

/**
 * The rendered note, and where a search hit lands in it.
 *
 * `accent://open/…` is a link to another note and is handed back to the app; `accent://file/…` is
 * an image, served off the vault. Nothing else loads at all — the same rule the desktop preview
 * enforces with a content blocker.
 *
 * A hit arrives as the query it was found by ([Open.find]) and is placed by the WebView's own
 * find-in-page: every occurrence marked, the first one scrolled to. No offset crosses into the
 * app at all — `SearchHit.at` is bytes into the markdown source and this screen holds the page
 * that source rendered to — so there is nothing to convert and nothing to get wrong. What it
 * costs is that the two do not agree on everything: the index folds accents and reads the markup,
 * find-in-page does neither, so a query it cannot match marks nothing and the note opens at the
 * top, which is where it opened before any of this (see NOTEPAD).
 *
 * The marking goes on the reader's first tap, which is the Android analogue of the desktop's
 * reveal highlight going on the first keystroke: it says where you were sent, and once that has
 * been read it is in the way. A tap does *not* take the reader's own find off ([FindBar]): those
 * matches are being stepped through rather than read once, and a reveal and a find bar are never
 * on the screen together — opening the bar clears whatever was marked before it.
 */
@Composable
private fun Rendered(model: VaultModel, open: Open, root: String, chrome: Chrome) {
    val colors = MaterialTheme.colorScheme
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
    var loaded by remember { mutableStateOf<String?>(null) }
    // What the reader has typed into the find bar, and where in the page it got them: which match
    // of how many, straight off the view's own find listener. Reset every time the bar opens.
    var query by remember(open.finding) { mutableStateOf("") }
    var matches by remember { mutableStateOf(0 to 0) }
    // Read inside the tap below, which is captured once and so cannot close over a parameter.
    val finding by rememberUpdatedState(open.finding)

    // The reader's own find: every keystroke marks the page again, and an empty field — which is
    // where the bar opens, and what it leaves behind when it closes — takes the marks off. One
    // effect for the bar's whole life, so there is no "it has gone" to remember separately.
    LaunchedEffect(open.finding, query) {
        val web = view ?: return@LaunchedEffect
        if (query.isBlank()) web.clearMatches() else web.findAllAsync(query)
    }

    // Text can only be found once it is there to find, so the query waits for the load — and
    // since the page is loaded only when the note or the palette changes, a hit in the note
    // already in front is marked without one.
    LaunchedEffect(loaded, open.find) {
        val reveal = open.find ?: return@LaunchedEffect
        val web = view ?: return@LaunchedEffect
        if (loaded != html) return@LaunchedEffect
        web.findAllAsync(reveal)
        model.found()
    }

    // Back puts the bar away, which is how the panel closes too.
    BackHandler(enabled = open.finding) { model.finding(false) }

    Column(Modifier.fillMaxSize()) {
        AndroidView(
            modifier = Modifier.weight(1f).fillMaxWidth().onTap(chrome) {
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
                            loaded = view.tag as? String
                        }

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
                            if (request.isForMainFrame) return null
                            val url = request.url.toString()
                            if (!url.startsWith("accent://file/")) return blocked()
                            val rel = decode(url.removePrefix("accent://file/"))
                            val file = File(root, rel)
                            return runCatching {
                                WebResourceResponse(null, null, file.inputStream())
                            }.getOrElse { blocked() }
                        }
                    }
                    setOnScrollChangeListener { _, _, y, _, was -> chrome.scrolled((y - was).toFloat()) }
                    setFindListener { active, total, _ -> matches = active to total }
                    view = this
                }
            },
            update = { web ->
                // The view's own tag is what it last loaded: `update` runs on every recomposition and
                // only a different page is worth a load.
                if (web.tag != html) {
                    web.tag = html
                    web.loadDataWithBaseURL(baseUri(open.rel), html, "text/html", "utf-8", null)
                }
            },
        )
        if (open.finding) {
            FindBar(
                query = query,
                onQuery = { query = it },
                matches = matches,
                onStep = { forward -> view?.findNext(forward) },
            )
        }
    }
}

/**
 * The note's own find: the page in front, where Browse's Search is every note in the vault.
 *
 * At the foot of the screen, which is where every query field in this app is and where the
 * keyboard leaves the thumb. It is the one piece of chrome that does not go while the keyboard is
 * up, because the keyboard is what it is for — the Browse pill goes instead, so the vault's search
 * and the page's find are never on the screen together. It takes its space from the note rather
 * than floating over it: a bar over the last lines would cover the match it had just found.
 *
 * Back is the way out, as it is out of the panel. The arrows are disabled rather than absent while
 * there is nothing to step through, and the count is the only thing that says a word is not on the
 * page at all — everything else about a find that matches nothing looks like a find that has not
 * scrolled yet.
 */
@Composable
private fun FindBar(
    query: String,
    onQuery: (String) -> Unit,
    matches: Pair<Int, Int>,
    onStep: (Boolean) -> Unit,
) {
    val focus = remember { FocusRequester() }
    LaunchedEffect(Unit) { focus.requestFocus() }
    val (active, total) = matches
    Row(
        Modifier.fillMaxWidth().padding(end = 4.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Field(
            value = query,
            onValue = onQuery,
            placeholder = "Find in this note",
            modifier = Modifier.weight(1f).focusRequester(focus),
        )
        Text(
            when {
                query.isBlank() -> ""
                total == 0 -> "None"
                else -> "${active + 1}/$total"
            },
            style = MaterialTheme.typography.labelMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        // Off while there is nothing to step through, and off on an emptied field: the count is
        // whatever the last find reported, and the marks it counted have been cleared.
        val stepping = query.isNotBlank() && total > 1
        TextButton(onClick = { onStep(false) }, enabled = stepping) { Text("▴") }
        TextButton(onClick = { onStep(true) }, enabled = stepping) { Text("▾") }
    }
}

private fun blocked() = WebResourceResponse(null, null, null)

/** What a relative link inside the note resolves against: the directory the note is in. */
private fun baseUri(rel: String): String {
    val dir = rel.substringBeforeLast('/', "")
    return if (dir.isEmpty()) "accent://file/" else "accent://file/$dir/"
}

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

// ------------------------------------------------------------------------------------ writing

/**
 * The editor: the note's own text, styled from the core's spans.
 *
 * The markup stays visible and is dimmed rather than hidden — Apostrophe's rule, and the
 * desktop's — because a phone keyboard has no way to put back a character it cannot see.
 *
 * The text is the model's ([VaultModel.buffer]) and so is the writing of it: what is typed here
 * is what the rendered view draws and what autosave writes out, and nothing about either is keyed
 * on which note this is. The editor is the field and its styling, and nothing else.
 */
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
 * Applies the core's spans to the field's output; nothing it does reaches the saved text.
 *
 * The parse is kept against the text it was of, because this runs on every output pass — a
 * recomposition for any reason at all, not only a keystroke — and parsing a note of any size that
 * often is the composition thread's whole frame. One parse per text, as the desktop does behind
 * its render debounce.
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
