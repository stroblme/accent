package io.github.stroblme.accent.ui

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.content.Intent
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
import kotlinx.coroutines.Dispatchers
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

/**
 * What the pen is doing. A finger never draws: it moves the page.
 *
 * [Read] is not on the toolbar — it is what the toolbar looks like with nothing chosen, and
 * tapping the tool in hand is what puts it down.
 */
enum class Tool { Read, Pen, Highlighter, Eraser }

/**
 * A PDF inside a vault: strokes are written back into the file it came from, and the notes that
 * link into it paint their highlights over it.
 */
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
    var notes by remember(pdf.rel) { mutableStateOf(emptyList<PdfLink>()) }
    LaunchedEffect(pdf.rel, indexing) { if (!indexing) notes = model.pdfLinks(pdf.rel) }
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

/**
 * The document's bookmarks over the page, put away by pulling it down.
 *
 * The surface Browse already uses, because this is the same kind of thing: a list the reader came
 * to on purpose and leaves by the gesture every other panel here leaves by. A depth is an indent
 * and nothing more — a PDF outline nests as deep as its author liked, and rows that fold are rows
 * whose folding has to be remembered. A bookmark naming no page is drawn and does nothing, which
 * is what it does in the file.
 */
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

/**
 * Whether a PDF may be drawn on at all.
 *
 * Off until the mobile design settles — the toolbar is five words where the desktop has a ring of
 * icons, and where it belongs on a phone is the same question the floating buttons answer. With
 * no way to pick a tool, [Tool.Read] is the only state there is and a finger only ever moves the
 * page. Not `const`, so the toolbar below is compiled rather than folded away.
 */
private val ANNOTATIONS = false

/** How long after the last stroke the document is written back. */
private const val INK_SAVE_MS = 1000L

/**
 * The document as a column of pages, panned and pinched as one surface.
 *
 * The column does the laying out and the recycling; the gesture does everything else. Both axes
 * move from the same handler, so a diagonal drag goes diagonally instead of picking a side.
 *
 * A pinch never lays the column out again. It scales one layer under the fingers and commits
 * once, on release — what every other reader on the platform does, and the only way the
 * arithmetic can be right: asking the list where it is and telling it where to go in the same
 * frame reads back the position from before the answer, so the page crept away from the fingers a
 * little more every frame. The page stretches the bitmap it already has and is drawn again at the
 * zoom the fingers leave it at.
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

    /**
     * The `/Link` boxes of the pages that are composed, left here by each of them.
     *
     * The hit test has to happen where the chrome's tap is decided, and that is this box rather
     * than any one page, so what each page reads goes somewhere the box can see. A page scrolled
     * away takes its entry with it.
     */
    val links = remember(doc) { mutableStateMapOf<Int, List<PdfLinkBox>>() }
    /** The highlights a note's links paint on the composed pages, left here as [links] are. */
    val marks = remember(doc) { mutableStateMapOf<Int, List<Mark>>() }
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
    /**
     * Whether the selection has its menu: one the reader made has, one a followed link shows does
     * not until a handle is touched — it is there to be read, not copied.
     */
    var menu by remember(doc) { mutableStateOf(false) }
    /** A finger is on a handle or still growing a long press's word. */
    var dragging by remember(doc) { mutableStateOf(false) }
    /** Where the pages' box is in the window, which is where the menu is placed from. */
    var origin by remember { mutableStateOf(Offset.Zero) }
    val haptics = LocalHapticFeedback.current
    /**
     * Where each jump in this document was taken from, the latest last: what Back retraces before
     * it leaves. Kept as a page and points down it rather than the row and offset the column was
     * at, which are pixels at one zoom and would be wrong after a pinch between the jump and Back.
     */
    val back = remember(doc) { mutableStateListOf<Pair<Int, Float>>() }
    // [onTap] captures its predicate once, so what the predicate reads has to be state it can read
    // again. The zoom, the pan, the viewport and the map above all are; the tool in hand is not.
    val inHand by rememberUpdatedState(tool)

    /**
     * The zoom the bitmaps were drawn at, and the part of the column they were drawn for. Both
     * follow the hands once they stop rather than every frame: a render started mid-gesture is
     * thrown away before it lands, and past [WHOLE_PAGE_PX] there is no whole page to draw anyway,
     * only whichever rectangle of it the screen is over.
     */
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

    /**
     * Put [top] points down page [index] at the top of the screen.
     *
     * Through [Pagination] and `requestScrollToItem`, which is the path a pinch commit takes, so a
     * jump lands in the same place whatever zoom the pages are at: a row index on its own means a
     * different place on the page at every zoom, and the offset into the row is where that is said.
     */
    fun goTo(index: Int, top: Float) {
        val (row, into) = pages.to(index, top, viewport.width, zoom)
        list.requestScrollToItem(row, into)
    }

    /**
     * Bring [rect], in points on page [index], into view below the bar if it is not: to the middle
     * of what the bar leaves, so it arrives with room around it to be read in. [jumped] puts where
     * the reader was on the way Back, as a find's first match does and its steps do not.
     */
    fun show(index: Int, rect: Rect, jumped: Boolean = false) {
        val above = pages.above(list, viewport.width, zoom)
        val to = pages.reveal(index, rect, above, panX, viewport, zoom, clear) ?: return
        if (jumped) back += pages.place(above, viewport.width, zoom)
        panX = to.panX
        list.requestScrollToItem(to.row, to.into)
    }

    /**
     * [goTo], remembering where the reader was so Back returns there. Only jumps come here: a
     * link and a bookmark, as on the desktop. Scrolling is reading, and a history of every page
     * passed would have nothing left to go back to.
     */
    fun jump(index: Int, top: Float) {
        back += pages.place(pages.above(list, viewport.width, zoom), viewport.width, zoom)
        goTo(index, top)
    }

    /**
     * Follow a link: down the document, or out of the app.
     *
     * The chrome is left exactly where the reader had it. This used to put it up, to undo a toggle
     * it could not stop; now that the tap is claimed before [Chrome.tapped] is reached, a tap on a
     * link is a link's tap and nothing else's, and somebody reading with the bar down stays that
     * way. A bookmark still calls [Chrome.show], because dismissing the panel really is something
     * else deciding where the reader is.
     */
    fun follow(target: LinkTarget) {
        when (target) {
            is LinkTarget.Page -> jump(target.page.toInt(), target.top ?: 0f)
            is LinkTarget.Uri -> leave(context, target.uri)
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

    /**
     * Where the two handles hang from on the screen: the foot of the first glyph's start and of
     * the last one's end. None while a pinch holds the layer scaled.
     */
    fun anchors(): Pair<Offset, Offset>? {
        if (live != 1f) return null
        val first = chosen.firstOrNull { it.boxes.isNotEmpty() } ?: return null
        val last = chosen.lastOrNull { it.boxes.isNotEmpty() } ?: return null
        val (a, b) = first.boxes.first() to last.boxes.last()
        val above = pages.above(list, viewport.width, zoom)
        return pages.screen(first.page, a.left, a.bottom, above, panX, viewport.width, zoom) to
            pages.screen(last.page, b.right, b.bottom, above, panX, viewport.width, zoom)
    }

    /**
     * The end a press on a handle keeps still, which is the other one; `null` for a press on
     * neither. The nearer handle where the two are close.
     */
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

    /**
     * Put the link to the selection on the clipboard, one per page it covers — `page=N` names one
     * page — and let it go. Pasting it into a note is what makes the highlight: there is no
     * Highlight of its own, as on the desktop.
     */
    fun copyLink() {
        val pieces = chosen
        selection = null
        scope.launch {
            val links = pieces.mapNotNull { doc.link(linkName, it.page, it.start, it.end) }
            if (links.isNotEmpty()) clip(links.joinToString("\n"))
        }
    }

    /**
     * Whether this tap belongs to a link rather than to the chrome.
     *
     * Decided here because this is where the chrome's own tap is decided and the two have to be
     * ordered — [onTap] asks before it toggles. A tap is never during a pinch (a second finger and
     * any movement past the slop both rule one out), so the layer under the fingers is at rest: the
     * pages are offset by [panX] across and by what the column has scrolled down, and nothing else.
     *
     * A tool in hand claims nothing. The page is a canvas then, and the drawing handler on it owns
     * every press that lands.
     *
     * A highlight is asked after the links, as on the desktop: a link inside one is still a link.
     * It opens the note it comes from, and hands over where the reader is, for Back from that note
     * to come back to.
     *
     * While there is a selection a tap only lets it go, wherever it lands — but for a tap on one
     * of its handles, which leaves it be.
     */
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
        val mark = markAt(marks[land.page].orEmpty(), land.point, slop) ?: return false
        val (page, top) = pages.place(scrolled, viewport.width, zoom)
        openNote(mark.link, PdfPlace(page, top, zoom, panX))
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
        val down = anchor(above, pivot.y, live, shift.y)
        val across = anchor(-panX, pivot.x, live, shift.x)
        zoom = (zoom * live).coerceIn(MIN_ZOOM, maxZoom)
        panX = holdXAt(-across, viewport.width, zoom)
        val (page, into) = pages.at(down, viewport.width, zoom)
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
                    panZoom(
                        onGesture = { centroid, pan, step ->
                            if (live == 1f && step == 1f) {
                                // One finger: the column scrolls and the pages slide, both at once.
                                list.dispatchRawDelta(-pan.y)
                                panX = holdXAt(panX + pan.x, viewport.width, zoom)
                                chrome.scrolled(-pan.y)
                            } else {
                                // Two: the layer below carries all of it until they are lifted.
                                if (live == 1f) pivot = centroid
                                live = (live * step).coerceIn(MIN_ZOOM / zoom, maxZoom / zoom)
                                shift += pan
                            }
                        },
                        onEnd = { velocity ->
                            if (live != 1f) {
                                // The one place the column is laid out again, on a list that has
                                // not moved since the pinch began: where the reader was, plus what
                                // the fingers did to it, is where they have to be put back.
                                commit()
                                return@panZoom
                            }
                            scope.launch {
                                var last = 0f
                                AnimationState(0f, -velocity.y).animateDecay(decay) {
                                    list.dispatchRawDelta(value - last)
                                    last = value
                                }
                            }
                            scope.launch {
                                var last = 0f
                                AnimationState(0f, velocity.x).animateDecay(decay) {
                                    panX = holdXAt(panX + (value - last), viewport.width, zoom)
                                    last = value
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

/**
 * Lay the content out exactly this big and put its top-left corner in the parent's, however much
 * bigger than the parent that is.
 *
 * `requiredWidth` looks like it does this and does not: it reports the parent a size coerced back
 * into the incoming constraints and then centres the content in it, so half of everything the zoom
 * added was taken off the left — a page that jumped sideways on every pinch and left a band of
 * background down its right. Anchoring the corner is the whole of what a scrolled, panned surface
 * wants, and it is the same answer on both axes.
 */
private fun Modifier.oversize(width: Int, height: Int) = layout { measurable, constraints ->
    val placeable = measurable.measure(Constraints.fixed(width, height))
    layout(constraints.constrainWidth(width), constraints.constrainHeight(height)) {
        placeable.place(0, 0)
    }
}

/** Sideways travel is bounded by how much wider than the screen the pages have become. */
private fun holdXAt(p: Float, width: Int, zoom: Float) = p.coerceIn(-width * (zoom - 1f), 0f)

/**
 * Where a layer scaled about its own corner has to be moved to, for the point the fingers began
 * between to stay between them.
 *
 * [pivot] is that point, [live] how much further apart they have got, [base] the translation the
 * layer already had along this axis and [shift] how far the hand has moved since.
 */
internal fun liveShift(pivot: Float, live: Float, base: Float, shift: Float): Float =
    pivot * (1f - live) + base * live + shift

/**
 * Where the content has to sit along one axis after a gesture, so that whatever was under the
 * fingers is still under them.
 *
 * [scrolled] is how much of the document is already past the near edge, [centroid] where the
 * fingers are between them, [by] how much further apart they got and [pan] how far they moved.
 * Everything grows away from the centroid, which is why the answer is not simply `scrolled * by`
 * — that grows away from the corner, and the page slides out from under the hand.
 */
internal fun anchor(scrolled: Float, centroid: Float, by: Float, pan: Float): Float =
    (scrolled + centroid) * by - centroid - pan

/**
 * Where the pages sit in the column, so a zoom can put the reader back where they were.
 *
 * The list only says which page is at the top and how far into it, both in whatever pixels the
 * current zoom makes; turning that into a distance from the start of the document, and back
 * again at another zoom, is all this does.
 */
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
    fun at(y: Float, width: Int, zoom: Float): Pair<Int, Int> {
        var left = y.coerceAtLeast(0f)
        for (i in sizes.indices) {
            val h = height(i, width, zoom)
            if (left < h) return i to left.roundToInt()
            left -= h
        }
        return maxOf(sizes.size - 1, 0) to 0
    }

    /**
     * Which page a point in the column fell on, and where on it, or `null` for one that fell on no
     * page at all — the gap between two, or past the end of the last.
     *
     * [x] and [y] are column pixels at [zoom], which is what a tap on the pages is once the pan and
     * the scroll are taken off it. The answer is in the page's own points, the space the core
     * talks about links in, and carries the scale that got it there, because that is what turns a
     * fingertip measured in pixels into a slop measured in points.
     */
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

    /**
     * The row, and the offset into it, that put [y] points down page [index] at the top of the
     * screen — what a bookmark and an internal link both ask for.
     *
     * The same two steps a pinch commit takes: a distance from the start of the document, then
     * back to a row at the zoom in hand. A row index on its own would be a different place on the
     * page at every zoom, and [y] past the end of the page falls through to the next one, which is
     * what [at] does with any overrun.
     */
    fun to(index: Int, y: Float, width: Int, zoom: Float): Pair<Int, Int> {
        val size = sizes.getOrNull(index) ?: return index to 0
        return at(top(index, width, zoom) + y * (width * zoom / size.width), width, zoom)
    }

    /**
     * The page, and the points down it, that [y] pixels from the start lands on at [zoom]: the
     * inverse of [to], and the form a place is kept in so that [to] can find it again at any zoom.
     */
    fun place(y: Float, width: Int, zoom: Float): Pair<Int, Float> {
        val (index, into) = at(y, width, zoom)
        val size = sizes.getOrNull(index) ?: return index to 0f
        return index to into / (width * zoom / size.width)
    }

    /**
     * Where [x], [y] points on page [index] are on the screen, [above] pixels scrolled and pushed
     * [panX] sideways: [on] the other way round, for what is drawn over the pages rather than on
     * them.
     */
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
     * Where the column has to be for [rect], in points on page [index], to be in view — or `null`
     * when it already is, [above] pixels scrolled and pushed [panX] sideways on a [viewport] whose
     * top [clear] pixels a bar covers. What is out of view along an axis is brought to the middle
     * of what the bar leaves along it; an axis it is already in view along is left alone.
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
}

/** Where the column has to be for something to be in view: the row, the offset into it, the pan. */
internal data class Reveal(val row: Int, val into: Int, val panX: Float)

/** Where a point on the pages fell: which page, where on it in points, and at what scale. */
internal data class Landing(val page: Int, val point: Point, val scale: Float)

private val PAGE_GAP = 8.dp
private const val MIN_ZOOM = 1f

/**
 * How far in a pinch may go.
 *
 * Memory no longer decides it: past [WHOLE_PAGE_PX] a page is drawn one screenful at a time, so a
 * render costs the same at any zoom. What the pixels are worth does. 8× fit width puts an A0 poster
 * — 2384 pt across, the widest paper anyone opens and the case that asked for this — at 3.6 px/pt on
 * a 1080 px screen, about the 2.6 px/pt such a screen draws a point at 1:1. Its 8 pt small print
 * stands 29 px tall, where the old ceiling of 6× reached 22 and then failed to draw at all, a whole
 * A0 page at 6× being 59 M pixels. An A4 page reaches 14.5 px/pt. Deeper buys detail the paper has
 * not got, and a shallower range is easier to land a pinch with on the thing being looked at.
 */
private const val MAX_ZOOM = 8f

/** How long the typing rests before a find starts over. */
private const val FIND_AFTER_MS = 150L

/** How long the hands rest before the pages are drawn again at where they left them. */
private const val RESHARPEN_MS = 180L

/**
 * How big a whole-page render may get before only the part on screen is drawn: 8 M pixels, which is
 * 32 MB of ARGB. An A4 page at a phone's fit width is 1.7 M, so ordinary reading and a pinch to
 * about twice that stay whole and nothing blanks while the column is scrolled.
 */
private const val WHOLE_PAGE_PX = 8_000_000L

/** Where the reader had got to when the hands stopped: the zoom, and the column rect on screen. */
private data class Settled(val zoom: Float, val window: IntRect)

/**
 * How far in this screen may be pinched, which is not always [MAX_ZOOM].
 *
 * [oversize] asks Compose to lay the column out `width × zoom` wide, and a pinch shrinking back
 * from the ceiling asks for it `height × zoom` tall. `Constraints` packs a pair of sizes into one
 * `Long` and can hold at most 32766 px in one dimension beside 65534 in the other, so a column past
 * that throws where it is measured. At 8× that leaves every phone and tablet the whole of it — the
 * clamp bites only above a 4095 px-wide or 8191 px-tall viewport, which is a desktop-sized window
 * rather than a device — but it is what makes [MAX_ZOOM] a number the layout can honour rather than
 * one that happens to fit the screens on sale.
 */
internal fun ceiling(viewport: IntSize): Float =
    minOf(MAX_ZOOM, 32766f / viewport.width, 65534f / viewport.height)

/**
 * The part of a page worth drawing, in the page's own pixels.
 *
 * The whole of [page] while that is small enough to hold, which is every page at reading zoom and
 * is why a scrolling column never waits for a tile; only what [window] covers once it is not.
 */
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
        if (tool == Tool.Read) links[index] = doc.links(index) else links.remove(index)
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
                                else -> doc.stroke(index, points, tool.style(colors.primary))
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

/**
 * A selection handle hanging from [at]: a round drop whose one square corner points at the glyph,
 * up and to the left for the end of a selection and up and to the right for its start — the
 * shape the platform's text handles have.
 */
private fun DrawScope.handle(at: Offset, r: Float, toRight: Boolean, colour: Color) {
    val side = if (toRight) 1f else -1f
    drawCircle(colour, r, at + Offset(side * r, r))
    drawRect(colour, Offset(if (toRight) at.x else at.x - r, at.y), Size(r, r))
}

/** How big a selection handle is drawn, and how far from its middle a finger still takes it. */
private val HANDLE = 10.dp
private val HANDLE_REACH = 24.dp

/**
 * The platform's floating text toolbar over what is selected, as every text view raises one: the
 * system draws it, places it clear of what [around] answers (window pixels) and follows it when
 * told the selection moved. `null` puts it away.
 *
 * [around] is read here rather than handed in as a value, so a scroll under a selection moves the
 * toolbar without composing the pages again.
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

/**
 * Which link a tap landed in, or none.
 *
 * [at] and [slop] are in page points, the space the core answers links in, so what zoom the page is
 * drawn at does not come into it. There is a slop at all because a link is usually one line of text
 * — about 18 px tall at a phone's fit width, against a fingertip of forty. The last match wins:
 * `/Link` boxes may overlap, and the later annotation is the one drawn on top of the other.
 */
internal fun hit(links: List<PdfLinkBox>, at: Point, slop: Float): PdfLinkBox? =
    links.lastOrNull { it.rect.near(at, slop) }

/** A highlight a note's link paints, and the link that paints it. */
internal data class Mark(val quads: List<Rect>, val link: PdfLink)

/**
 * Which highlight a tap landed in, or none: the first whose quads it is [near], which is the one
 * the desktop's `highlight_at` opens where two overlap — the links arrive in the order the index
 * lists them.
 */
internal fun markAt(marks: List<Mark>, at: Point, slop: Float): Mark? =
    marks.firstOrNull { mark -> mark.quads.any { it.near(at, slop) } }

/** Whether [at] is on this box or within [slop] of it, all in page points. */
private fun Rect.near(at: Point, slop: Float): Boolean =
    at.x >= left - slop && at.x <= right + slop && at.y >= top - slop && at.y <= bottom + slop

/** How far off a link a tap may land and still count, in view pixels. */
private const val TAP_SLOP = 12f

/**
 * Hand a document's URL to the platform.
 *
 * The one place this reader reaches the network, so the system's chooser answers it rather than a
 * view of ours: nothing is fetched, nothing is opened inside the app, and nothing happens at all
 * for a scheme a reader would not expect a document to carry — a PDF can say `file:` or
 * `javascript:` as easily as `https:`, and neither is a link anybody meant to follow.
 */
private fun leave(context: Context, uri: String) {
    val target = Uri.parse(uri)
    if (target.scheme?.lowercase() !in OUTWARD) return
    runCatching { context.startActivity(Intent(Intent.ACTION_VIEW, target)) }
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
