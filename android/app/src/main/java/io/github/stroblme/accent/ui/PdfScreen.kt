package io.github.stroblme.accent.ui

import android.content.Context
import android.content.Intent
import android.net.Uri
import androidx.activity.compose.BackHandler
import androidx.compose.animation.core.AnimationState
import androidx.compose.animation.core.animateDecay
import androidx.compose.animation.core.exponentialDecay
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.gestures.detectDragGestures
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyListState
import androidx.compose.foundation.lazy.rememberLazyListState
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
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.input.pointer.PointerType
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.layout.layout
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
import io.github.stroblme.accent.PdfModel
import io.github.stroblme.accent.ffi.InkStyle
import io.github.stroblme.accent.ffi.LinkTarget
import io.github.stroblme.accent.ffi.Outline
import io.github.stroblme.accent.ffi.PageSize
import io.github.stroblme.accent.ffi.PdfLinkBox
import io.github.stroblme.accent.ffi.Point
import io.github.stroblme.accent.ffi.Theme
import java.io.File
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.launch

/**
 * What the pen is doing. A finger never draws: it moves the page.
 *
 * [Read] is not on the toolbar — it is what the toolbar looks like with nothing chosen, and
 * tapping the tool in hand is what puts it down.
 */
enum class Tool { Read, Pen, Highlighter, Eraser }

/** A PDF inside a vault: strokes are written back into the file it came from. */
@Composable
fun PdfScreen(path: String, chrome: Chrome) {
    var doc by remember(path) { mutableStateOf<PdfModel?>(null) }
    var failed by remember(path) { mutableStateOf<String?>(null) }
    LaunchedEffect(path) {
        runCatching { PdfModel.open(path) }
            .onSuccess { doc = it }
            .onFailure { failed = it.message ?: "This file could not be opened." }
    }
    DisposableEffect(path) { onDispose { doc?.close() } }
    Reader(doc, failed, File(path).name.removeSuffix(".pdf"), chrome) { it.save() }
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
            val bytes = context.contentResolver.openInputStream(uri)!!.use { it.readBytes() }
            PdfModel.of(bytes)
        }.onSuccess { doc = it }
            .onFailure { failed = it.message ?: "This file could not be opened." }
    }
    DisposableEffect(uri) { onDispose { doc?.close() } }
    val name = uri.lastPathSegment?.substringAfterLast('/')?.removeSuffix(".pdf").orEmpty()
    // Opened straight from another app, so there is no vault screen around this one to keep it
    // clear of the status bar and the gesture strip.
    Box(Modifier.windowInsetsPadding(WindowInsets.safeDrawing)) {
        Reader(doc, failed, name, chrome) { model ->
            // No path on this side: the bytes go back through whatever handed them over.
            val bytes = model.bytes() ?: return@Reader Result.failure(Exception("Nothing to write"))
            runCatching {
                context.contentResolver.openOutputStream(uri, "wt")!!.use { it.write(bytes) }
            }
        }
    }
}

@Composable
private fun Reader(
    doc: PdfModel?,
    failed: String?,
    title: String,
    chrome: Chrome,
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
        // The same shape a note has: one bar that fades, then the document under it in the same
        // rectangle, so that moving between the two does not move what is being read.
        Box(Modifier.fillMaxSize().padding(padding)) {
            Column(Modifier.fillMaxSize()) {
                FadingBar(visible = chrome.shown) {
                    // The bar has one action, and Contents is now what a PDF puts in it: it says
                    // so even on a file carrying no outline, the way it used to say Edit. Where
                    // the annotation tools go is still open ([ANNOTATIONS]) and is not this slot.
                    DocumentBar(
                        title = title,
                        action = "Contents",
                        enabled = marks.isNotEmpty(),
                        onAction = { contents = true },
                    )
                }
                Box(Modifier.weight(1f).padding(vertical = DocumentGap)) {
                    Pages(doc, tool, chrome, wanted) { wanted = null }
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
private fun Pages(doc: PdfModel, tool: Tool, chrome: Chrome, wanted: Int?, onWent: () -> Unit) {
    val density = LocalDensity.current
    val list = rememberLazyListState()
    val scope = rememberCoroutineScope()
    val context = LocalContext.current
    val colors = MaterialTheme.colorScheme
    val theme = remember(colors) {
        // The same recolouring the desktop applies in a dark theme: the document's paper lands on
        // the app's surface and its ink on the app's text, each pixel keeping its own chroma.
        if (colors.surface.dark()) {
            Theme.Recolour(colors.surface.rgb(), colors.onSurface.rgb())
        } else {
            Theme.Plain
        }
    }

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
            is LinkTarget.Page -> goTo(target.page.toInt(), target.top ?: 0f)
            is LinkTarget.Uri -> leave(context, target.uri)
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
     */
    fun claimed(at: Offset): Boolean {
        if (inHand != Tool.Read || viewport.width == 0) return false
        val scrolled = pages.above(list, viewport.width, zoom)
        val land = pages.on(at.x - panX, at.y + scrolled, viewport.width, zoom) ?: return false
        val box = hit(links[land.page].orEmpty(), land.point, TAP_SLOP / land.scale) ?: return false
        follow(box.target)
        return true
    }

    // A bookmark is the same jump from further away: the panel that made it is gone by now, and
    // only the column knows how tall its rows are at the zoom in hand.
    LaunchedEffect(wanted, viewport) {
        val page = wanted ?: return@LaunchedEffect
        if (viewport.width == 0) return@LaunchedEffect
        goTo(page, 0f)
        onWent()
    }

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

    Box(
        Modifier
            .fillMaxSize()
            .clipToBounds()
            .onSizeChanged { viewport = it }
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
                            // The one place the column is laid out again, on a list that has not
                            // moved since the pinch began: where the reader was, plus what the
                            // fingers did to it, is where they have to be put back.
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
            },
    ) {
        LazyColumn(
            state = list,
            // Every drag goes through the gesture above, which is what lets one follow both axes.
            userScrollEnabled = false,
            modifier = Modifier
                // As wide as the zoom makes it, and — while a pinch is shrinking the layer — tall
                // enough that the rows to fill the screen are still composed.
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
                        // The settled screen in this page's own pixels: a page is exactly as wide
                        // as the column, so the only difference is where the page starts down it.
                        window = settled.window.translate(
                            0,
                            -pages.top(index, viewport.width, settled.zoom).roundToInt(),
                        ),
                        theme = theme,
                        tool = tool,
                        links = links,
                    )
                }
            }
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
}

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
    DisposableEffect(index) { onDispose { links.remove(index) } }

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
    }
}

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
    links.lastOrNull {
        at.x >= it.rect.left - slop && at.x <= it.rect.right + slop &&
            at.y >= it.rect.top - slop && at.y <= it.rect.bottom + slop
    }

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
