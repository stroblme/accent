package io.github.stroblme.accent.ui

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.graphics.Rect as AndroidRect
import android.net.Uri
import android.os.Build
import android.provider.OpenableColumns
import android.view.ActionMode
import android.view.Menu
import android.view.MenuItem
import android.view.View
import androidx.activity.compose.BackHandler
import androidx.compose.animation.core.AnimationState
import androidx.compose.animation.core.animateDecay
import androidx.compose.animation.core.exponentialDecay
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.gestures.awaitEachGesture
import androidx.compose.foundation.gestures.awaitFirstDown
import androidx.compose.foundation.gestures.awaitLongPressOrCancellation
import androidx.compose.foundation.gestures.detectDragGestures
import androidx.compose.foundation.gestures.drag
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyListState
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.foundation.verticalScroll
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.draw.clipToBounds
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.TransformOrigin
import androidx.compose.ui.graphics.drawscope.DrawScope
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.input.pointer.PointerType
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.hapticfeedback.HapticFeedbackType
import androidx.compose.ui.layout.layout
import androidx.compose.ui.layout.onGloballyPositioned
import androidx.compose.ui.layout.positionInRoot
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.platform.LocalView
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.unit.Constraints
import androidx.compose.ui.unit.IntOffset
import androidx.compose.ui.unit.IntRect
import androidx.compose.ui.unit.IntSize
import androidx.compose.ui.unit.constrainHeight
import androidx.compose.ui.unit.constrainWidth
import kotlin.math.roundToInt
import androidx.compose.ui.unit.dp
import io.github.stroblme.accent.OpenPdf
import io.github.stroblme.accent.PdfModel
import io.github.stroblme.accent.PdfPlace
import io.github.stroblme.accent.VaultModel
import io.github.stroblme.accent.ffi.Comment
import io.github.stroblme.accent.ffi.Glyph
import io.github.stroblme.accent.ffi.InkStyle
import io.github.stroblme.accent.ffi.LinkTarget
import io.github.stroblme.accent.ffi.Outline
import io.github.stroblme.accent.ffi.PageSize
import io.github.stroblme.accent.ffi.PdfLink
import io.github.stroblme.accent.ffi.PdfLinkBox
import io.github.stroblme.accent.ffi.Point
import io.github.stroblme.accent.ffi.Rect
import io.github.stroblme.accent.ffi.Theme
import java.io.File
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/** Active PDF tool. [Read] means no drawing tool; fingers pan the page. */
enum class Tool { Read, Pen, Highlighter, Eraser }

/** Vault PDF with editable strokes and note-linked highlights. */
@Composable
fun PdfScreen(model: VaultModel, pdf: OpenPdf, indexing: Boolean, chrome: Chrome) {
    val path = pdf.path
    var doc by remember(path) { mutableStateOf<PdfModel?>(null) }
    var failed by remember(path) { mutableStateOf<String?>(null) }
    LaunchedEffect(path) {
        runCatching { PdfModel.open(path) }
            .onSuccess { doc = it }
            .onFailure { failed = it.message ?: "This file could not be opened." }
    }
    DisposableEffect(path) { onDispose { doc?.close() } }
    // Asked again whenever a walk ends, which is when a link written elsewhere reaches the index.
    // An answer that could not be read leaves the highlights already painted.
    var notes by remember(pdf.rel) { mutableStateOf(emptyList<PdfLink>()) }
    LaunchedEffect(pdf.rel, indexing) { if (!indexing) model.pdfLinks(pdf.rel)?.let { notes = it } }
    // Print… from the palette, once the document is open: a copy with those links' highlights.
    // The request is taken back once the print system has it, as a note's is.
    val context = LocalContext.current
    val accent = paperAccent(MaterialTheme.colorScheme)
    LaunchedEffect(pdf.printing, doc) {
        val open = doc ?: return@LaunchedEffect
        if (!pdf.printing) return@LaunchedEffect
        try {
            open.printCopy(notes, (accent.rgb() shl 8) or 0xFFu)
                .onSuccess { printPdf(context, File(path).name.removeSuffix(".pdf"), it, open.pageCount) }
                .onFailure { model.said("This PDF could not be printed") }
        } finally {
            model.printing(false)
        }
    }
    Reader(
        doc,
        failed,
        File(path).name.removeSuffix(".pdf"),
        chrome,
        linkName = pdf.rel,
        invertKey = path,
        at = pdf.at,
        notes = notes,
        onNote = model::openFromPdf,
        onPlace = { model.reading(pdf.rel, at = it) },
        finding = pdf.finding,
        onFinding = model::finding,
    ) { it.save() }
}

/** A PDF opened from somewhere else: there is no vault, so it is written back where it came from. */
@Composable
fun LoosePdfScreen(uri: Uri) {
    val context = LocalContext.current
    // Nothing to browse and nothing to launch without a vault, so this chrome moves nothing; the
    // reader still takes one, because the gesture that hides a toolbar is the gesture that hides
    // the toolbar this document will have again.
    val chrome = remember { Chrome() }
    var doc by remember(uri) { mutableStateOf<PdfModel?>(null) }
    var failed by remember(uri) { mutableStateOf<String?>(null) }
    LaunchedEffect(uri) {
        runCatching {
            // Off the main thread: the whole file is read, and it is somebody else's document,
            // which may be on a network provider.
            val bytes = withContext(Dispatchers.IO) {
                context.contentResolver.openInputStream(uri)!!.use { it.readBytes() }
            }
            PdfModel.of(bytes)
        }.onSuccess { doc = it }
            .onFailure { failed = it.message ?: "This file could not be opened." }
    }
    DisposableEffect(uri) { onDispose { doc?.close() } }
    // The name the app that handed the file over gives it: what the bar shows, and what a copied
    // link names it by, as the desktop links a PDF from outside any vault by its file name. The
    // URI's own last segment is often an opaque id, and is only the fallback.
    var name by remember(uri) {
        mutableStateOf(uri.lastPathSegment?.substringAfterLast('/').orEmpty())
    }
    LaunchedEffect(uri) {
        withContext(Dispatchers.IO) { displayName(context, uri) }?.let { name = it }
    }
    // No palette out here, so the bar's Find is the only way in and the flag is this screen's own.
    var finding by remember(uri) { mutableStateOf(false) }
    // Opened straight from another app, so there is no vault screen around this one to keep it
    // clear of the status bar and the gesture strip.
    Box(Modifier.windowInsetsPadding(WindowInsets.safeDrawing)) {
        Reader(
            doc,
            failed,
            name.removeSuffix(".pdf"),
            chrome,
            linkName = name,
            invertKey = uri.toString(),
            finding = finding,
            onFinding = { finding = it },
        ) { model ->
            // No path on this side: the bytes go back through whatever handed them over.
            val bytes = model.bytes() ?: return@Reader Result.failure(Exception("Nothing to write"))
            runCatching {
                withContext(Dispatchers.IO) {
                    context.contentResolver.openOutputStream(uri, "wt")!!.use { it.write(bytes) }
                }
            }
        }
    }
}

/** What the provider calls the file behind [uri], if it says. */
private fun displayName(context: Context, uri: Uri): String? = runCatching {
    context.contentResolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)
        ?.use { if (it.moveToFirst()) it.getString(0) else null }
}.getOrNull()

@Composable
private fun Reader(
    doc: PdfModel?,
    failed: String?,
    title: String,
    chrome: Chrome,
    /** What a copied link calls the document: its vault path, or its file name from outside. */
    linkName: String,
    /** What Invert remembers it by ([Inverted]): its path, or its address from outside. */
    invertKey: String,
    /** Where it opens, if not at the top. */
    at: PdfPlace? = null,
    /** The note links into it, which paint as highlights; none for a document from outside. */
    notes: List<PdfLink> = emptyList(),
    onNote: (PdfLink, PdfPlace) -> Unit = { _, _ -> },
    /** Where the reader is, said as it changes, for Back from a backlink to return to. */
    onPlace: (PdfPlace) -> Unit = {},
    /** Whether its find bar is open. */
    finding: Boolean,
    onFinding: (Boolean) -> Unit,
    onSave: suspend (PdfModel) -> Result<Unit>,
) {
    val scope = rememberCoroutineScope()
    var tool by remember { mutableStateOf(Tool.Read) }
    val snackbar = remember { SnackbarHostState() }
    /** The bookmarks, read once: a document's outline does not change under the reader. */
    var marks by remember(doc) { mutableStateOf(emptyList<Outline>()) }
    var contents by remember(doc) { mutableStateOf(false) }
    /** A page something outside the column has asked for, until the column has gone there. */
    var wanted by remember(doc) { mutableStateOf<Int?>(null) }
    val inverted = invertKey in Inverted.files

    if (failed != null) {
        Column(
            Modifier.fillMaxSize().padding(Gutter),
            verticalArrangement = Arrangement.Center,
            horizontalAlignment = Alignment.CenterHorizontally,
        ) {
            Text("Cannot open this PDF", style = MaterialTheme.typography.headlineSmall)
            Spacer(Modifier.height(8.dp))
            Text(failed, color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
        return
    }
    if (doc == null) {
        Box(Modifier.fillMaxSize(), Alignment.Center) { CircularProgressIndicator() }
        return
    }

    LaunchedEffect(doc) { marks = doc.outline() }

    // Strokes are written back a second after the last one, the way the desktop does it and the
    // way an edited note does: there is no Save to forget.
    LaunchedEffect(doc.revision) {
        if (!doc.dirty) return@LaunchedEffect
        delay(INK_SAVE_MS)
        onSave(doc).onFailure {
            snackbar.showSnackbar(it.message ?: "This file could not be saved.")
        }
    }

    // No insets of its own: inside a vault the screen around it has already kept clear of the
    // status bar, and applying them twice is what put this bar lower than a note's.
    // [LoosePdfScreen], which has no such screen around it, keeps them itself.
    Scaffold(
        snackbarHost = { SnackbarHost(snackbar) },
        contentWindowInsets = WindowInsets(0, 0, 0, 0),
    ) { padding ->
        Box(Modifier.fillMaxSize().padding(padding)) {
            DocumentFrame(
                barShown = chrome.shown,
                bar = {
                    // Find, the bookmarks, and Invert. Contents says so even on a file carrying no
                    // outline, the way it used to say Edit: a gap where a control belongs is worse
                    // than one that says it has nothing to offer. Where the annotation tools go is
                    // still open ([ANNOTATIONS]).
                    DocumentBar(title) {
                        TextButton(onClick = { onFinding(true) }) { Text("Find") }
                        TextButton(onClick = { contents = true }, enabled = marks.isNotEmpty()) {
                            Text("Contents")
                        }
                        BarToggle("Invert", on = inverted, onClick = { Inverted.toggle(invertKey) })
                    }
                },
            ) { bar ->
                val clear = with(LocalDensity.current) { bar.toPx() }
                Pages(
                    doc,
                    linkName,
                    tool,
                    inverted,
                    chrome,
                    wanted,
                    at,
                    notes,
                    onNote,
                    clear,
                    finding,
                    onFinding,
                    onSay = { scope.launch { snackbar.showSnackbar(it) } },
                    onPlace = onPlace,
                ) { wanted = null }
                if (ANNOTATIONS) {
                    PdfToolbar(
                        tool = tool,
                        // Tapping the tool in hand puts it down, the only way back to reading.
                        onTool = { tool = if (it == tool) Tool.Read else it },
                        canUndo = doc.canUndo,
                        canRedo = doc.canRedo,
                        onUndo = { scope.launch { doc.undo() } },
                        onRedo = { scope.launch { doc.redo() } },
                        modifier = Modifier.align(Alignment.BottomEnd).padding(Gutter),
                    )
                }
            }
            // Over the document rather than beside it, which is what makes Back and a pull the
            // way out of it.
            BackHandler(enabled = contents) { contents = false }
            if (contents) {
                Contents(marks, onClose = { contents = false }) { page ->
                    wanted = page
                    contents = false
                    // Somewhere new is shown with its bar up, the same rule a followed link takes.
                    chrome.show()
                }
            }
        }
    }
}

/** Show the PDF outline in a pull-to-close panel, indenting rows by bookmark depth. */
@Composable
private fun Contents(marks: List<Outline>, onClose: () -> Unit, onGo: (Int) -> Unit) {
    PullDownPanel(onClose) {
        ScreenBar("Contents")
        LazyColumn(Modifier.fillMaxSize()) {
            items(marks.size) { i ->
                val mark = marks[i]
                val page = mark.page?.toInt()
                ListItem(
                    headlineContent = {
                        Text(mark.title, maxLines = 2, overflow = TextOverflow.Ellipsis)
                    },
                    supportingContent = page?.let { { Text("${it + 1}") } },
                    colors = flatRow(),
                    modifier = Modifier
                        .padding(start = (mark.depth.toInt() * INDENT_DP).dp)
                        .let { if (page == null) it else it.row { onGo(page) } },
                )
            }
        }
    }
}

/** How far one level of the outline is pushed in. Half a gutter: deep outlines are common. */
private const val INDENT_DP = 8

/** Drawing stays disabled until the mobile toolbar design is settled. */
private val ANNOTATIONS = false

/** How long after the last stroke the document is written back. */
private const val INK_SAVE_MS = 1000L

/**
 * Lay out pages as one scrollable, pannable, zoomable column. Scale the existing layer
 * during a pinch and commit layout and rendering on release.
 */
@Composable
private fun Pages(
    doc: PdfModel,
    linkName: String,
    tool: Tool,
    /** The reader has turned the recolouring round for this file ([pageTheme]). */
    inverted: Boolean,
    chrome: Chrome,
    wanted: Int?,
    /** Where the document opens, if not at the top. */
    opening: PdfPlace?,
    notes: List<PdfLink>,
    onNote: (PdfLink, PdfPlace) -> Unit,
    /** How much of the top of the screen the document's bar covers, in pixels. */
    clear: Float,
    finding: Boolean,
    onFinding: (Boolean) -> Unit,
    /** A line for the snackbar. */
    onSay: (String) -> Unit,
    /** Where the reader is: the page and the points down it at the top, the zoom and the pan. */
    onPlace: (PdfPlace) -> Unit,
    onWent: () -> Unit,
) {
    val density = LocalDensity.current
    val list = rememberLazyListState()
    val scope = rememberCoroutineScope()
    val context = LocalContext.current
    val colors = MaterialTheme.colorScheme
    val theme = remember(colors, inverted) { pageTheme(colors.surface.dark(), inverted) }

    var viewport by remember { mutableStateOf(IntSize.Zero) }
    var zoom by remember { mutableFloatStateOf(1f) }
    /** How far the pages are pushed sideways: 0 at fit width, negative once they are wider. */
    var panX by remember { mutableFloatStateOf(0f) }
    /** How much further apart the fingers have got since the pinch began; 1 while none is on. */
    var live by remember { mutableFloatStateOf(1f) }
    /** Where they were between them when it began, and how far they have moved since. */
    var pivot by remember { mutableStateOf(Offset.Zero) }
    var shift by remember { mutableStateOf(Offset.Zero) }

    val gap = with(density) { PAGE_GAP.toPx() }
    val pages = remember(doc) { Pagination(doc.sizes, gap) }
    val maxZoom = remember(viewport) { ceiling(viewport) }

    /** Collect composed pages' `/Link` boxes for tap hit-testing above the pages. */
    val links = remember(doc) { mutableStateMapOf<Int, List<PdfLinkBox>>() }
    /** The highlights a note's links paint on the composed pages, left here as [links] are. */
    val marks = remember(doc) { mutableStateMapOf<Int, List<Mark>>() }
    /** Other readers' comments on the composed pages, left here as [links] are. */
    val comments = remember(doc) { mutableStateMapOf<Int, List<Comment>>() }
    /** The comments a tap opened, while their dialog is up. */
    var shown by remember(doc) { mutableStateOf<List<Comment>?>(null) }
    val notesOn = remember(notes) { notes.groupBy { it.page.toInt() } }
    val openNote by rememberUpdatedState(onNote)
    /** The glyphs of the pages a selection has needed, read once each. */
    val glyphs = remember(doc) { mutableStateMapOf<Int, List<Glyph>>() }
    var selection by remember(doc) { mutableStateOf<Selection?>(null) }
    val chosen by remember(doc) { derivedStateOf { selection?.pieces(glyphs).orEmpty() } }
    /** Pages whose glyphs are on their way, so a drag over one asks for them once. */
    val asked = remember(doc) { mutableSetOf<Int>() }
    /** The word a long press made, which the drag after it grows from. */
    var word by remember(doc) { mutableStateOf<Selection?>(null) }
    /** Show the menu for a user selection, but delay it for a linked selection until touched. */
    var menu by remember(doc) { mutableStateOf(false) }
    /** A finger is on a handle or still growing a long press's word. */
    var dragging by remember(doc) { mutableStateOf(false) }
    /** Where the pages' box is in the window, which is where the menu is placed from. */
    var origin by remember { mutableStateOf(Offset.Zero) }
    val haptics = LocalHapticFeedback.current
    /** Back stack of page positions in points, independent of zoom. */
    val back = remember(doc) { mutableStateListOf<Pair<Int, Float>>() }
    // [onTap] captures its predicate once, so what the predicate reads has to be state it can read
    // again. The zoom, the pan, the viewport and the map above all are; the tool in hand is not.
    val inHand by rememberUpdatedState(tool)

    /** Render scale and visible column region, updated after a gesture settles. */
    var settled by remember { mutableStateOf(Settled(1f, IntRect.Zero)) }
    LaunchedEffect(pages, viewport) {
        snapshotFlow {
            val top = pages.above(list, viewport.width, zoom).roundToInt()
            val left = (-panX).roundToInt()
            Settled(zoom, IntRect(left, top, left + viewport.width, top + viewport.height))
        }.collectLatest {
            delay(RESHARPEN_MS)
            settled = it
        }
    }

    // Where the reader is, said as it changes: a backlink followed out of the document keeps it,
    // and Back opens the document there again, as it does from a highlight's note.
    val tell by rememberUpdatedState(onPlace)
    LaunchedEffect(pages, viewport) {
        val width = viewport.width
        if (width == 0) return@LaunchedEffect
        snapshotFlow {
            val (page, top) = pages.place(pages.above(list, width, zoom), width, zoom)
            PdfPlace(page, top, zoom, panX)
        }.collect { tell(it) }
    }

    /** Scroll to [top] points down page [index] using zoom-aware [Pagination]. */
    fun goTo(index: Int, top: Float) {
        val (row, into) = pages.to(index, top, viewport.width, zoom)
        list.requestScrollToItem(row, into)
    }

    /** Bring [rect] on page [index] below the bar if needed; [jumped] records Back history. */
    fun show(index: Int, rect: Rect, jumped: Boolean = false) {
        val above = pages.above(list, viewport.width, zoom)
        val to = pages.reveal(index, rect, above, panX, viewport, zoom, clear) ?: return
        if (jumped) back += pages.place(above, viewport.width, zoom)
        panX = to.panX
        list.requestScrollToItem(to.row, to.into)
    }

    /**
     * Perform [goTo] and record the previous position for Back; ordinary scrolling adds no history.
     */
    fun jump(index: Int, top: Float) {
        back += pages.place(pages.above(list, viewport.width, zoom), viewport.width, zoom)
        goTo(index, top)
    }

    /** Follow an internal or external link without changing chrome visibility. */
    fun follow(target: LinkTarget) {
        when (target) {
            is LinkTarget.Page -> jump(target.page.toInt(), target.top ?: 0f)
            is LinkTarget.Uri -> leave(context, target.uri, onSay)
        }
    }

    /** Read a page's glyphs if they are not here and not on their way. */
    fun need(page: Int) {
        if (page in glyphs || !asked.add(page)) return
        scope.launch { glyphs[page] = doc.glyphs(page) }
    }

    fun select(to: Selection) {
        for (page in to.from.page..to.to.page) need(page)
        selection = to
    }

    /** Where a point on the screen falls in the text: a page, and the glyph nearest it there. */
    fun caretAt(at: Offset): Caret? {
        val above = pages.above(list, viewport.width, zoom)
        val land = pages.on(at.x - panX, at.y + above, viewport.width, zoom) ?: return null
        val on = glyphs[land.page] ?: return null.also { need(land.page) }
        return nearest(on, land.point)?.let { Caret(land.page, it) }
    }

    /** A long press: the word under the finger, once its page's glyphs are here. */
    fun pressed(at: Offset) {
        val above = pages.above(list, viewport.width, zoom)
        val land = pages.on(at.x - panX, at.y + above, viewport.width, zoom) ?: return
        scope.launch {
            val on = glyphs[land.page] ?: doc.glyphs(land.page).also { glyphs[land.page] = it }
            val glyph = nearest(on, land.point) ?: return@launch
            val range = wordAt(on, glyph)
            word = Selection(Caret(land.page, range.first), Caret(land.page, range.last))
                .also { select(it) }
            menu = true
        }
    }

    /** Screen positions of selection handles, unavailable during a live pinch. */
    fun anchors(): Pair<Offset, Offset>? {
        if (live != 1f) return null
        val first = chosen.firstOrNull { it.boxes.isNotEmpty() } ?: return null
        val last = chosen.lastOrNull { it.boxes.isNotEmpty() } ?: return null
        val (a, b) = first.boxes.first() to last.boxes.last()
        val above = pages.above(list, viewport.width, zoom)
        return pages.screen(first.page, a.left, a.bottom, above, panX, viewport.width, zoom) to
            pages.screen(last.page, b.right, b.bottom, above, panX, viewport.width, zoom)
    }

    /** Return the stationary endpoint when a handle is pressed, preferring the nearer handle. */
    fun heldAt(at: Offset): Caret? {
        val (start, end) = anchors() ?: return null
        val now = selection ?: return null
        val r = with(density) { HANDLE.toPx() }
        val reach = with(density) { HANDLE_REACH.toPx() }
        val toStart = (at - (start + Offset(-r, r))).getDistance()
        val toEnd = (at - (end + Offset(r, r))).getDistance()
        return when {
            minOf(toStart, toEnd) > reach -> null
            toStart < toEnd -> now.to
            else -> now.from
        }
    }

    /** The window rectangle the menu keeps clear of: what is on screen of the selection. */
    fun menuAround(): AndroidRect? {
        if (live != 1f) return null
        val above = pages.above(list, viewport.width, zoom)
        var box: Rect? = null
        val width = viewport.width
        for (piece in chosen) for (glyph in piece.boxes) {
            val a = pages.screen(piece.page, glyph.left, glyph.top, above, panX, width, zoom)
            val b = pages.screen(piece.page, glyph.right, glyph.bottom, above, panX, width, zoom)
            val onScreen = Rect(a.x, a.y, b.x, b.y)
            box = box?.union(onScreen) ?: onScreen
        }
        val on = box ?: return null
        val left = maxOf(on.left, 0f)
        val top = maxOf(on.top, clear)
        val right = minOf(on.right, viewport.width.toFloat())
        val bottom = minOf(on.bottom, viewport.height.toFloat())
        if (left >= right || top >= bottom) return null
        return AndroidRect(
            (origin.x + left).toInt(),
            (origin.y + top).toInt(),
            (origin.x + right).toInt(),
            (origin.y + bottom).toInt(),
        )
    }

    fun clip(text: String) {
        context.getSystemService(ClipboardManager::class.java)
            .setPrimaryClip(ClipData.newPlainText("PDF text", text))
        // Android 13 and later say so themselves, with what was copied.
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU) onSay("Copied")
    }

    /** Put the selection on the clipboard and let it go, as a copy from a text view does. */
    fun copy() {
        chosen.text().takeIf { it.isNotEmpty() }?.let(::clip)
        selection = null
    }

    /** Copy one selection link per page and clear the selection; note links create highlights. */
    fun copyLink() {
        val pieces = chosen
        selection = null
        scope.launch {
            val links = pieces.mapNotNull { doc.link(linkName, it.page, it.start, it.end) }
            if (links.isNotEmpty()) clip(links.joinToString("\n"))
        }
    }

    /** Handle selections, PDF links, highlights, and comments before chrome toggling. */
    fun claimed(at: Offset): Boolean {
        if (selection != null) {
            if (heldAt(at) == null) selection = null
            return true
        }
        if (inHand != Tool.Read || viewport.width == 0) return false
        val scrolled = pages.above(list, viewport.width, zoom)
        val land = pages.on(at.x - panX, at.y + scrolled, viewport.width, zoom) ?: return false
        val slop = TAP_SLOP / land.scale
        hit(links[land.page].orEmpty(), land.point, slop)?.let {
            follow(it.target)
            return true
        }
        markAt(marks[land.page].orEmpty(), land.point, slop)?.let {
            val (page, top) = pages.place(scrolled, viewport.width, zoom)
            openNote(it.link, PdfPlace(page, top, zoom, panX))
            return true
        }
        val under = commentsAt(comments[land.page].orEmpty(), land.point, slop)
        if (under.isEmpty()) return false
        shown = under
        return true
    }

    // Where the document opens, once the screen has a size to lay it out at: the place a reader
    // left it for a note, which Back from the note returns to, or the passage a followed link
    // quotes, shown as the selection. Once — after that the column is wherever the reader takes
    // it.
    var opened by remember(doc) { mutableStateOf(false) }
    LaunchedEffect(viewport) {
        val place = opening ?: return@LaunchedEffect
        if (opened || viewport.width == 0) return@LaunchedEffect
        opened = true
        zoom = place.zoom.coerceIn(MIN_ZOOM, maxZoom)
        panX = holdXAt(place.panX, viewport.width, zoom)
        // Numbers written against another engine's reading of the page point at nothing here, and
        // the page they name is still where the reader wanted to be — the desktop's rule.
        val found = place.selection?.let { doc.locate(place.page, it) }
        if (found == null || found.quads.isEmpty()) {
            goTo(place.page, place.top)
            return@LaunchedEffect
        }
        glyphs[place.page] = doc.glyphs(place.page)
        val (start, end) = found.start.toInt() to found.end.toInt()
        selection = Selection(Caret(place.page, start), Caret(place.page, end - 1))
        menu = false
        show(place.page, found.quads.reduce(Rect::union))
    }

    // A bookmark is the same jump from further away: the panel that made it is gone by now, and
    // only the column knows how tall its rows are at the zoom in hand.
    LaunchedEffect(wanted, viewport) {
        val page = wanted ?: return@LaunchedEffect
        if (viewport.width == 0) return@LaunchedEffect
        jump(page, 0f)
        onWent()
    }

    // Composed before the Contents panel's handler, so an open panel is still what Back closes
    // first; with nothing left to retrace, Back falls through and leaves the document.
    BackHandler(enabled = back.isNotEmpty()) {
        val (page, top) = back.removeAt(back.lastIndex)
        goTo(page, top)
    }
    // The find: the whole document a page at a time, from the page under the middle of the screen
    // round to the one before it, so the match the reader lands on is found first — the first at or
    // after that page, as a note's find lands on the first after where it is. Typing again starts
    // over, and what was running stops between two pages: one page is the smallest unit pdfium
    // searches, and tiles asked for meanwhile are drawn between them. Opening the bar starts empty.
    var query by remember(doc, finding) { mutableStateOf("") }
    var found by remember(doc) { mutableStateOf(Found()) }
    var searched by remember(doc) { mutableStateOf(false) }
    LaunchedEffect(query) {
        found = Found()
        searched = false
        if (query.isBlank()) return@LaunchedEffect
        delay(FIND_AFTER_MS)
        val middle = pages.above(list, viewport.width, zoom) + viewport.height / 2f
        val reading = pages.at(middle, viewport.width, zoom).first
        for (page in (reading until doc.pageCount) + (0 until reading)) {
            found = found.plus(page, doc.search(page, query))
            if (found.at == null) found.from(reading)?.let { first ->
                found = found.copy(at = first)
                found.hits[first].let { show(it.page, it.box, jumped = true) }
            }
        }
        searched = true
        // Nothing at or after the page being read: the first there is, round the end.
        if (found.at == null && found.hits.isNotEmpty()) {
            found = found.copy(at = 0)
            found.hits[0].let { show(it.page, it.box, jumped = true) }
        }
    }
    fun step(forward: Boolean) {
        found = found.step(forward)
        found.at?.let { found.hits[it] }?.let { show(it.page, it.box) }
    }

    // After the jumps', so a find open and then a selection are put away first.
    BackHandler(enabled = finding) { onFinding(false) }
    BackHandler(enabled = selection != null) { selection = null }

    /** Take what the fingers did to the layer and lay the column out that way. */
    fun commit() {
        val above = pages.above(list, viewport.width, zoom)
        val to = pages.released(above, panX, pivot, shift, live, viewport.width, zoom, maxZoom)
        zoom = to.zoom
        panX = to.panX
        val (page, into) = pages.at(to.above, viewport.width, zoom)
        list.requestScrollToItem(page, into)
        live = 1f
        shift = Offset.Zero
    }

    Column(Modifier.fillMaxSize()) {
        Box(
            Modifier
                .weight(1f)
                .fillMaxWidth()
                .clipToBounds()
                .onSizeChanged { viewport = it }
                .onGloballyPositioned { origin = it.positionInRoot() }
                .onTap(chrome, claimed = ::claimed)
                .pointerInput(doc, viewport) {
                    val decay = exponentialDecay<Float>()
                    // Not `live != 1f`: at either end of the zoom the layer is held at 1 while the
                    // fingers still move it.
                    var pinching = false
                    // The throw the last drag left. The next finger down stops it, as on any
                    // scroller, before that gesture reads where the column is: a column still
                    // moving under a pinch is one the release would lay out from elsewhere.
                    var thrown: Job? = null
                    panZoom(
                        onDown = { thrown?.cancel() },
                        onGesture = { centroid, pan, step ->
                            if (!pinching && step == 1f) {
                                // One finger: the column scrolls and the pages slide, both at once.
                                list.dispatchRawDelta(-pan.y)
                                panX = holdXAt(panX + pan.x, viewport.width, zoom)
                                chrome.scrolled(-pan.y)
                            } else {
                                // Two: the layer below carries all of it until they are lifted,
                                // from where they were before this event, as [shift] counts.
                                if (!pinching) pivot = centroid - pan
                                pinching = true
                                live = (live * step).coerceIn(MIN_ZOOM / zoom, maxZoom / zoom)
                                shift += pan
                            }
                        },
                        onEnd = { velocity ->
                            if (pinching) {
                                pinching = false
                                // The one place the column is laid out again, on a list that has
                                // not moved since the pinch began: where the reader was, plus what
                                // the fingers did to it, is where they have to be put back.
                                commit()
                                return@panZoom
                            }
                            thrown = scope.launch {
                                launch {
                                    var last = 0f
                                    AnimationState(0f, -velocity.y).animateDecay(decay) {
                                        list.dispatchRawDelta(value - last)
                                        last = value
                                    }
                                }
                                launch {
                                    var last = 0f
                                    AnimationState(0f, velocity.x).animateDecay(decay) {
                                        panX = holdXAt(panX + (value - last), viewport.width, zoom)
                                        last = value
                                    }
                                }
                            }
                        },
                    )
                }
                // After the pan and the pinch, so it is asked first and what it takes they drop:
                // a press on a handle moves that end, and a long press selects the word under
                // the finger and grows it as the finger moves on. Before either, a scroll or a
                // pinch has already taken the gesture, which is what cancels a long press.
                .pointerInput(doc, viewport, tool) {
                    if (tool != Tool.Read) return@pointerInput
                    awaitEachGesture {
                        val down = awaitFirstDown(requireUnconsumed = false)
                        val held = heldAt(down.position)
                        val press = if (held != null) down.also { it.consume() }
                        else awaitLongPressOrCancellation(down.id) ?: return@awaitEachGesture
                        if (held == null) {
                            haptics.performHapticFeedback(HapticFeedbackType.LongPress)
                            // Not the last press's word: this one's may still be on its way.
                            word = null
                            pressed(press.position)
                        }
                        menu = true
                        dragging = true
                        try {
                            drag(press.id) { change ->
                                change.consume()
                                val caret = caretAt(change.position) ?: return@drag
                                if (held != null) select(Selection.between(held, caret))
                                else word?.let { select(it.grownTo(caret)) }
                            }
                        } finally {
                            dragging = false
                        }
                    }
                },
        ) {
            LazyColumn(
                state = list,
                // Every drag goes through the gesture above, which is what lets one follow both
                // axes.
                userScrollEnabled = false,
                modifier = Modifier
                    // As wide as the zoom makes it, and — while a pinch is shrinking the layer —
                    // tall enough that the rows to fill the screen are still composed.
                    .oversize(
                        width = (viewport.width * zoom).toInt(),
                        height = (viewport.height / live.coerceAtMost(1f)).toInt(),
                    )
                    .graphicsLayer {
                        transformOrigin = TransformOrigin(0f, 0f)
                        scaleX = live
                        scaleY = live
                        translationX = liveShift(pivot.x, live, panX, shift.x)
                        translationY = liveShift(pivot.y, live, 0f, shift.y)
                    },
                verticalArrangement = Arrangement.spacedBy(PAGE_GAP),
            ) {
                items(doc.pageCount) { index ->
                    val size = doc.sizes.getOrNull(index)
                    if (size != null && viewport.width > 0) {
                        Page(
                            doc = doc,
                            index = index,
                            pageWidth = size.width,
                            pageHeight = size.height,
                            shownPx = (viewport.width * zoom).toInt(),
                            renderPx = (viewport.width * settled.zoom).toInt(),
                            // The settled screen in this page's own pixels: a page is exactly as
                            // wide as the column, so the only difference is where the page starts
                            // down it.
                            window = settled.window.translate(
                                0,
                                -pages.top(index, viewport.width, settled.zoom).roundToInt(),
                            ),
                            theme = theme,
                            tool = tool,
                            links = links,
                            comments = comments,
                            notes = notesOn[index].orEmpty(),
                            marks = marks,
                            chosen = chosen.firstOrNull { it.page == index }?.boxes.orEmpty(),
                            hits = found.hits.filter { it.page == index }.map { it.box },
                            current = found.at?.let { found.hits[it] }
                                ?.takeIf { it.page == index }?.box,
                        )
                    }
                }
            }
            // Over the pages rather than on them, where no page's edge can cut one off.
            val grip = colors.primary
            Canvas(Modifier.matchParentSize()) {
                val (start, end) = anchors() ?: return@Canvas
                handle(start, HANDLE.toPx(), toRight = false, grip)
                handle(end, HANDLE.toPx(), toRight = true, grip)
            }
        }
        SelectionMenu(
            around = { if (selection != null && menu && !dragging) menuAround() else null },
            actions = listOf("Copy" to ::copy, "Copy link" to ::copyLink),
        )
        shown?.let { Comments(it) { shown = null } }
        // Below the pages rather than over them, as a note's is: a bar over the foot of the screen
        // would cover the match it had just found.
        if (finding) {
            FindBar(
                query = query,
                onQuery = { query = it },
                placeholder = "Find in this document",
                count = found.count(searched),
                canStep = found.hits.size > 1,
                onStep = ::step,
            )
        }
    }
}

/** Measure at [width] × [height] while anchoring oversized content at the parent's top-left. */
private fun Modifier.oversize(width: Int, height: Int) = layout { measurable, constraints ->
    val placeable = measurable.measure(Constraints.fixed(width, height))
    layout(constraints.constrainWidth(width), constraints.constrainHeight(height)) {
        placeable.place(0, 0)
    }
}

/** Sideways travel is bounded by how much wider than the screen the pages have become. */
private fun holdXAt(p: Float, width: Int, zoom: Float) = p.coerceIn(-width * (zoom - 1f), 0f)

/** Translation that keeps [pivot] under moving fingers as scale [live] and pan change. */
internal fun liveShift(pivot: Float, live: Float, base: Float, shift: Float): Float =
    pivot * (1f - live) + base * live + shift

/** Final offset that preserves the point beneath the fingers after zoom [by] and [pan]. */
internal fun anchor(scrolled: Float, centroid: Float, by: Float, pan: Float): Float =
    (scrolled + centroid) * by - centroid - pan

/** Map list row and offset to a document position independent of zoom. */
internal class Pagination(private val sizes: List<PageSize>, private val gap: Float) {
    private fun height(index: Int, width: Int, zoom: Float): Float {
        val size = sizes.getOrNull(index) ?: return gap
        return size.height * (width * zoom / size.width) + gap
    }

    /** Where page [index] starts, in pixels from the start of the document at [zoom]. */
    fun top(index: Int, width: Int, zoom: Float): Float {
        var y = 0f
        for (i in 0 until index) y += height(i, width, zoom)
        return y
    }

    /** How much of the document is above the top of the screen, in pixels at [zoom]. */
    fun above(list: LazyListState, width: Int, zoom: Float): Float =
        top(list.firstVisibleItemIndex, width, zoom) + list.firstVisibleItemScrollOffset

    /** The page, and the offset into it, that [y] pixels from the start lands on at [zoom]. */
    fun at(y: Float, width: Int, zoom: Float): Pair<Int, Int> =
        row(y, width, zoom).let { (index, into) -> index to into.roundToInt() }

    /** Round [at], preserving overrun past the last row so the column clamps at its end. */
    private fun row(y: Float, width: Int, zoom: Float): Pair<Int, Float> {
        var left = y.coerceAtLeast(0f)
        for (i in sizes.indices) {
            val h = height(i, width, zoom)
            if (left < h) return i to left
            left -= h
        }
        val last = sizes.lastIndex
        return if (last < 0) 0 to 0f else last to left + height(last, width, zoom)
    }

    /** Map [y] between zoom levels while keeping fixed page gaps unchanged. */
    private fun rescale(y: Float, width: Int, from: Float, to: Float): Float {
        val (index, into) = row(y, width, from)
        return top(index, width, to) + into * to / from
    }

    /** Map column pixel coordinates [x], [y] at [zoom] to page points, or null for gaps. */
    fun on(x: Float, y: Float, width: Int, zoom: Float): Landing? {
        if (y < 0f) return null
        var left = y
        for (i in sizes.indices) {
            val h = height(i, width, zoom)
            if (left < h) {
                val scale = width * zoom / sizes[i].width
                // The gap under a page is part of no page, and neither is a tap that lands in it.
                if (left > sizes[i].height * scale) return null
                return Landing(i, Point(x / scale, left / scale), scale)
            }
            left -= h
        }
        return null
    }

    /** Find the row and offset for [y] points down page [index] at the current zoom. */
    fun to(index: Int, y: Float, width: Int, zoom: Float): Pair<Int, Int> {
        val size = sizes.getOrNull(index) ?: return index to 0
        return at(top(index, width, zoom) + y * (width * zoom / size.width), width, zoom)
    }

    /** Map document pixel position [y] at [zoom] to page and point offset. */
    fun place(y: Float, width: Int, zoom: Float): Pair<Int, Float> {
        val (index, into) = at(y, width, zoom)
        val size = sizes.getOrNull(index) ?: return index to 0f
        return index to into / (width * zoom / size.width)
    }

    /** Map page point [x], [y] to screen coordinates with scroll [above] and [panX]. */
    fun screen(
        index: Int,
        x: Float,
        y: Float,
        above: Float,
        panX: Float,
        width: Int,
        zoom: Float,
    ): Offset {
        val scale = width * zoom / (sizes.getOrNull(index)?.width ?: return Offset.Zero)
        return Offset(panX + x * scale, top(index, width, zoom) - above + y * scale)
    }

    /**
     * Return scroll and pan needed to show [rect] within [viewport] below [clear],
     * or null when it is already visible.
     */
    fun reveal(
        index: Int,
        rect: Rect,
        above: Float,
        panX: Float,
        viewport: IntSize,
        zoom: Float,
        clear: Float,
    ): Reveal? {
        val size = sizes.getOrNull(index) ?: return null
        val scale = viewport.width * zoom / size.width
        val top = top(index, viewport.width, zoom)
        val (y0, y1) = top + rect.top * scale to top + rect.bottom * scale
        val (x0, x1) = rect.left * scale to rect.right * scale
        val down = y0 >= above + clear && y1 <= above + viewport.height
        val across = x0 >= -panX && x1 <= -panX + viewport.width
        if (down && across) return null
        val y = if (down) above else (y0 + y1) / 2 - (clear + viewport.height) / 2
        val x = if (across) panX else viewport.width / 2f - (x0 + x1) / 2
        val (row, into) = at(y, viewport.width, zoom)
        return Reveal(row, into, holdXAt(x, viewport.width, zoom))
    }

    /** Commit pinch zoom, pan, and scroll while preserving the point under the fingers. */
    fun released(
        above: Float,
        panX: Float,
        pivot: Offset,
        shift: Offset,
        live: Float,
        width: Int,
        zoom: Float,
        maxZoom: Float,
    ): Release {
        val next = (zoom * live).coerceIn(MIN_ZOOM, maxZoom)
        val across = anchor(-panX, pivot.x, live, shift.x)
        val down = rescale(above + pivot.y, width, zoom, next) - pivot.y - shift.y
        return Release(next, holdXAt(-across, width, next), down)
    }
}

/** Where a pinch leaves the column: the zoom, the pan, and how far down the document it is. */
internal data class Release(val zoom: Float, val panX: Float, val above: Float)

/** Where the column has to be for something to be in view: the row, the offset into it, the pan. */
internal data class Reveal(val row: Int, val into: Int, val panX: Float)

/** Where a point on the pages fell: which page, where on it in points, and at what scale. */
internal data class Landing(val page: Int, val point: Point, val scale: Float)

private val PAGE_GAP = 8.dp
private const val MIN_ZOOM = 1f

/** Maximum zoom relative to fit width; large pages use visible tiles beyond [WHOLE_PAGE_PX]. */
private const val MAX_ZOOM = 8f

/** How long the typing rests before a find starts over. */
private const val FIND_AFTER_MS = 150L

/** How long the hands rest before the pages are drawn again at where they left them. */
private const val RESHARPEN_MS = 180L

/** Render whole pages up to 8 Mpx (32 MB ARGB); use visible tiles beyond that. */
private const val WHOLE_PAGE_PX = 8_000_000L

/** Where the reader had got to when the hands stopped: the zoom, and the column rect on screen. */
private data class Settled(val zoom: Float, val window: IntRect)

/** Clamp [MAX_ZOOM] so Compose constraints stay within 32766 px width and 65534 px height. */
internal fun ceiling(viewport: IntSize): Float =
    minOf(MAX_ZOOM, 32766f / viewport.width, 65534f / viewport.height)

/** Draw all of a small [page], or only its intersection with [window] when large. */
internal fun visible(page: IntRect, window: IntRect): IntRect =
    if (page.width.toLong() * page.height <= WHOLE_PAGE_PX) page else page.intersect(window)

@Composable
private fun Page(
    doc: PdfModel,
    index: Int,
    pageWidth: Float,
    pageHeight: Float,
    /** The width the page is laid out at, which a pinch changes every frame. */
    shownPx: Int,
    /** The width its bitmap was drawn at, which follows the pinch once it settles. */
    renderPx: Int,
    /** What the screen covers of this page, in its own pixels at [renderPx]. */
    window: IntRect,
    theme: Theme,
    tool: Tool,
    /** Where this page leaves its `/Link` boxes for the box above to hit-test against. */
    links: MutableMap<Int, List<PdfLinkBox>>,
    /** Where it leaves other readers' comments, as it leaves [links]. */
    comments: MutableMap<Int, List<Comment>>,
    /** The note links into this page. */
    notes: List<PdfLink>,
    /** Where this page leaves the highlights they paint, as it leaves [links]. */
    marks: MutableMap<Int, List<Mark>>,
    /** The boxes of the selected glyphs on this page. */
    chosen: List<Rect>,
    /** The find's matches on this page, and the one stepped to if it is here. */
    hits: List<Rect>,
    current: Rect?,
) {
    val density = LocalDensity.current
    val scale = shownPx / pageWidth
    val heightPx = (pageHeight * scale).toInt()
    var sheet by remember(index, theme) { mutableStateOf<Sheet?>(null) }
    var generation by remember(index) { mutableIntStateOf(0) }
    val scope = rememberCoroutineScope()
    val colors = MaterialTheme.colorScheme

    val whole = IntRect(0, 0, renderPx, (pageHeight * renderPx / pageWidth).toInt())
    val want = visible(whole, window)
    LaunchedEffect(index, renderPx, theme, generation, want) {
        // Nothing of it on screen and too big to draw whole: keep whatever it has until it is.
        if (want.isEmpty) return@LaunchedEffect
        val perPoint = renderPx / pageWidth
        val image =
            if (want == whole) doc.page(index, perPoint, theme)
            else doc.tile(index, perPoint, want.left, want.top, want.width, want.height, theme)
        if (image != null) sheet = Sheet(image, want, renderPx)
    }

    // What is being drawn right now, in view pixels, before the core has it.
    val wet = remember { mutableStateListOf<Offset>() }

    // Read here, where the page number is, and left where the box above can reach it. Only while
    // a finger is a pointer: with a tool in hand the page is a canvas and a tap on it is a stroke.
    LaunchedEffect(index, tool) {
        if (tool == Tool.Read) {
            links[index] = doc.links(index)
            comments[index] = doc.comments(index)
        } else {
            links.remove(index)
            comments.remove(index)
        }
    }
    // Where the note links land on the page today, which is the core's to say: by their numbers,
    // then by the text they quote, and not at all once exported into the file.
    LaunchedEffect(index, notes) {
        if (notes.isEmpty()) marks.remove(index)
        else marks[index] = doc.highlights(notes).map { Mark(it.quads, notes[it.link.toInt()]) }
    }
    DisposableEffect(index) {
        onDispose {
            links.remove(index)
            comments.remove(index)
            marks.remove(index)
        }
    }
    val highlight = colors.primary.copy(alpha = HIGHLIGHT_ALPHA)
    val selected = colors.primary.copy(alpha = SELECTION_ALPHA)
    val match = colors.primary.copy(alpha = MARK_ALPHA)
    val stepped = colors.primary.copy(alpha = CURRENT_MARK_ALPHA)

    Box(
        Modifier
            .fillMaxWidth()
            .height(with(density) { heightPx.toDp() })
            .pointerInput(tool, index, scale) {
                if (tool == Tool.Read) return@pointerInput
                detectDragGestures(
                    onDragStart = { at ->
                        wet.clear()
                        wet += at
                    },
                    onDrag = { change, _ ->
                        // A finger moves the page; only a stylus draws. The exception is a
                        // device with no stylus at all, where there would otherwise be no way in.
                        if (change.type == PointerType.Touch && hasStylus) return@detectDragGestures
                        wet += change.position
                        change.consume()
                    },
                    onDragEnd = {
                        val points = wet.map { Point(it.x / scale, it.y / scale) }
                        wet.clear()
                        if (points.size < 2) return@detectDragGestures
                        scope.launch {
                            when (tool) {
                                Tool.Eraser -> doc.erase(index, points, ERASER_RADIUS, partial = false)
                                else -> doc.stroke(index, points, tool.style(colors.primary), scale)
                            }
                            generation++
                        }
                    },
                )
            },
    ) {
        sheet?.let { (image, at, px) ->
            Canvas(Modifier.fillMaxSize()) {
                // Stretched from the pixels it was drawn in to the ones it is shown at rather than
                // drawn 1:1, so a pinch moves the page with the fingers and the sharper render
                // catches up afterwards. A tile lands where on the page it came from; a whole
                // page's rectangle is the page, so it covers the lot as it always did.
                val stretch = size.width / px
                drawImage(
                    image = image,
                    srcSize = IntSize(image.width, image.height),
                    dstOffset = IntOffset(
                        (at.left * stretch).roundToInt(),
                        (at.top * stretch).roundToInt(),
                    ),
                    dstSize = IntSize(
                        (image.width * stretch).roundToInt(),
                        (image.height * stretch).roundToInt(),
                    ),
                )
                if (wet.size > 1) {
                    val path = Path().apply {
                        moveTo(wet[0].x, wet[0].y)
                        for (p in wet.drop(1)) lineTo(p.x, p.y)
                    }
                    drawPath(
                        path = path,
                        color = tool.wetColour(colors.primary),
                        style = Stroke(width = tool.width() * scale),
                    )
                }
            }
        } ?: Box(
            Modifier.fillMaxSize(),
            Alignment.Center,
        ) { Text("${index + 1}", color = colors.onSurfaceVariant) }
        // Over the page rather than in its pixels, so a highlight comes and goes without a render
        // and shows on a page still waiting for one; at the size the page is shown, so a pinch
        // carries it along. The highlights under the selection and the find's matches, as on the
        // desktop: they are what the page says, the other two what the reader is doing to it now.
        val painted = marks[index].orEmpty()
        if (painted.isNotEmpty() || chosen.isNotEmpty() || hits.isNotEmpty()) {
            Canvas(Modifier.fillMaxSize()) {
                for (mark in painted) for (quad in mark.quads) box(quad, scale, highlight)
                for (glyph in chosen) box(glyph, scale, selected)
                for (hit in hits) box(hit, scale, if (hit == current) stepped else match)
            }
        }
    }
}

/** Draw a platform-shaped selection handle anchored at [at]. */
private fun DrawScope.handle(at: Offset, r: Float, toRight: Boolean, colour: Color) {
    val side = if (toRight) 1f else -1f
    drawCircle(colour, r, at + Offset(side * r, r))
    drawRect(colour, Offset(if (toRight) at.x else at.x - r, at.y), Size(r, r))
}

/** How big a selection handle is drawn, and how far from its middle a finger still takes it. */
private val HANDLE = 10.dp
private val HANDLE_REACH = 24.dp

/**
 * Show the system selection toolbar around [around], updating its position on scroll.
 * Pass null to dismiss it.
 */
@Composable
private fun SelectionMenu(around: () -> AndroidRect?, actions: List<Pair<String, () -> Unit>>) {
    val view = LocalView.current
    val ask by rememberUpdatedState(around)
    val rect by remember { derivedStateOf { ask() } }
    val run by rememberUpdatedState(actions)
    val mode = remember { mutableStateOf<ActionMode?>(null) }
    val shown = rect != null
    DisposableEffect(shown) {
        if (!shown) return@DisposableEffect onDispose {}
        val callback = object : ActionMode.Callback2() {
            override fun onCreateActionMode(m: ActionMode, menu: Menu): Boolean {
                run.forEachIndexed { i, (label, _) -> menu.add(Menu.NONE, i, i, label) }
                return true
            }

            override fun onPrepareActionMode(m: ActionMode, menu: Menu) = false

            override fun onActionItemClicked(m: ActionMode, item: MenuItem): Boolean {
                run.getOrNull(item.itemId)?.second?.invoke()
                return true
            }

            override fun onDestroyActionMode(m: ActionMode) {
                mode.value = null
            }

            override fun onGetContentRect(m: ActionMode, v: View, out: AndroidRect) {
                rect?.let { out.set(it) }
            }
        }
        mode.value = view.startActionMode(callback, ActionMode.TYPE_FLOATING)
        onDispose { mode.value?.finish() }
    }
    LaunchedEffect(rect) { mode.value?.invalidateContentRect() }
}

/** Fill [r], in page points, on a page drawn at [scale] pixels per point. */
private fun DrawScope.box(r: Rect, scale: Float, colour: Color) = drawRect(
    colour,
    topLeft = Offset(r.left * scale, r.top * scale),
    size = Size((r.right - r.left) * scale, (r.bottom - r.top) * scale),
)

/** A page as it was last drawn: the bitmap, the part of the page it covers, and in what pixels. */
private data class Sheet(val image: ImageBitmap, val at: IntRect, val px: Int)

/** Hit-test links in page points using [slop]; the last overlapping annotation wins. */
internal fun hit(links: List<PdfLinkBox>, at: Point, slop: Float): PdfLinkBox? =
    links.lastOrNull { it.rect.near(at, slop) }

/** A highlight a note's link paints, and the link that paints it. */
internal data class Mark(val quads: List<Rect>, val link: PdfLink)

/** Return the first highlight near a tap, matching desktop overlap order. */
internal fun markAt(marks: List<Mark>, at: Point, slop: Float): Mark? =
    marks.firstOrNull { mark -> mark.quads.any { it.near(at, slop) } }

/** Every comment a tap landed on, in the page's order: all of them show, as on the desktop. */
internal fun commentsAt(comments: List<Comment>, at: Point, slop: Float): List<Comment> =
    comments.filter { comment -> comment.areas.any { it.near(at, slop) } }

/** Show read-only PDF comments, grouping overlaps under "Comments" with selectable text. */
@Composable
private fun Comments(comments: List<Comment>, onClose: () -> Unit) {
    val one = comments.singleOrNull()
    AlertDialog(
        onDismissRequest = onClose,
        title = { Text(if (one == null) "Comments" else one.author ?: "Comment") },
        text = {
            SelectionContainer {
                Column(
                    Modifier.verticalScroll(rememberScrollState()),
                    verticalArrangement = Arrangement.spacedBy(16.dp),
                ) {
                    for (comment in comments) {
                        Column {
                            if (one == null) comment.author?.let {
                                Text(
                                    it,
                                    style = MaterialTheme.typography.labelMedium,
                                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                                )
                            }
                            Text(comment.text)
                        }
                    }
                }
            }
        },
        confirmButton = { TextButton(onClick = onClose) { Text("Close") } },
    )
}

/** Whether [at] is on this box or within [slop] of it, all in page points. */
private fun Rect.near(at: Point, slop: Float): Boolean =
    at.x >= left - slop && at.x <= right + slop && at.y >= top - slop && at.y <= bottom + slop

/** How far off a link a tap may land and still count, in view pixels. */
private const val TAP_SLOP = 12f

/** Open an allowed document URL through [launchUri]; say why a rejected scheme goes nowhere. */
private fun leave(context: Context, uri: String, onSay: (String) -> Unit) {
    val target = Uri.parse(uri)
    if (target.scheme?.lowercase() !in OUTWARD) return onSay("This type of link can't be opened")
    launchUri(context, uri, onSay)
}

/** The schemes a document may send the reader out to. */
private val OUTWARD = setOf("http", "https", "mailto", "tel")

private const val ERASER_RADIUS = 6f

/** Widths in page points, the same three the desktop ring offers for each tool. */
private fun Tool.width(): Float = when (this) {
    Tool.Highlighter -> 14f
    else -> 2f
}

private fun Tool.style(accent: Color): InkStyle = InkStyle(
    width = width(),
    rgba = when (this) {
        // 0xRRGGBBAA: a highlighter is laid on at 40 %, a pen opaque.
        Tool.Highlighter -> (accent.rgb() shl 8) or 0x66u
        else -> (accent.rgb() shl 8) or 0xFFu
    },
    multiply = this == Tool.Highlighter,
)

private fun Tool.wetColour(accent: Color): Color =
    if (this == Tool.Highlighter) accent.copy(alpha = 0.4f) else accent

/** Whether this device has a stylus at all; if it has none, a finger is allowed to draw. */
private val hasStylus: Boolean = false


@Composable
private fun PdfToolbar(
    tool: Tool,
    onTool: (Tool) -> Unit,
    canUndo: Boolean,
    canRedo: Boolean,
    onUndo: () -> Unit,
    onRedo: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Surface(
        modifier = modifier,
        shape = MaterialTheme.shapes.extraLarge,
        color = MaterialTheme.colorScheme.surface,
        tonalElevation = 0.dp,
        shadowElevation = 6.dp,
    ) {
        Row(
            Modifier.padding(horizontal = 8.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            for (each in Tool.entries.filter { it != Tool.Read }) {
                TextButton(onClick = { onTool(each) }) {
                    Text(
                        each.name,
                        color = if (each == tool) MaterialTheme.colorScheme.primary
                        else MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }
            TextButton(onClick = onUndo, enabled = canUndo) { Text("Undo") }
            TextButton(onClick = onRedo, enabled = canRedo) { Text("Redo") }
        }
    }
}
