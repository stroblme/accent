package io.github.stroblme.accent.ui

import android.net.Uri
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
import androidx.compose.ui.graphics.luminance
import androidx.compose.ui.graphics.toArgb
import androidx.compose.ui.input.pointer.PointerType
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.unit.IntSize
import kotlin.math.roundToInt
import androidx.compose.ui.unit.dp
import io.github.stroblme.accent.PdfModel
import io.github.stroblme.accent.ffi.InkStyle
import io.github.stroblme.accent.ffi.Point
import io.github.stroblme.accent.ffi.Theme
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
fun PdfScreen(path: String) {
    var doc by remember(path) { mutableStateOf<PdfModel?>(null) }
    var failed by remember(path) { mutableStateOf<String?>(null) }
    LaunchedEffect(path) {
        runCatching { PdfModel.open(path) }
            .onSuccess { doc = it }
            .onFailure { failed = it.message ?: "This file could not be opened." }
    }
    DisposableEffect(path) { onDispose { doc?.close() } }
    Reader(doc, failed) { it.save() }
}

/** A PDF opened from somewhere else: there is no vault, so it is written back where it came from. */
@Composable
fun LoosePdfScreen(uri: Uri) {
    val context = LocalContext.current
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
    Reader(doc, failed) { model ->
        // No path on this side: the bytes go back through whatever handed them over.
        val bytes = model.bytes() ?: return@Reader Result.failure(Exception("Nothing to write"))
        runCatching {
            context.contentResolver.openOutputStream(uri, "wt")!!.use { it.write(bytes) }
        }
    }
}

@Composable
private fun Reader(
    doc: PdfModel?,
    failed: String?,
    onSave: suspend (PdfModel) -> Result<Unit>,
) {
    val scope = rememberCoroutineScope()
    var tool by remember { mutableStateOf(Tool.Read) }
    val snackbar = remember { SnackbarHostState() }

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

    // Strokes are written back a second after the last one, the way the desktop does it and the
    // way an edited note does: there is no Save to forget.
    LaunchedEffect(doc.revision) {
        if (!doc.dirty) return@LaunchedEffect
        delay(INK_SAVE_MS)
        onSave(doc).onFailure {
            snackbar.showSnackbar(it.message ?: "This file could not be saved.")
        }
    }

    Scaffold(snackbarHost = { SnackbarHost(snackbar) }) { padding ->
        Box(Modifier.fillMaxSize().padding(padding)) {
            Pages(doc, tool)
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
}

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
private fun Pages(doc: PdfModel, tool: Tool) {
    val density = LocalDensity.current
    val list = rememberLazyListState()
    val scope = rememberCoroutineScope()
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
    /** The zoom the bitmaps were drawn at. It follows the fingers once they stop. */
    var drawn by remember { mutableFloatStateOf(1f) }
    /** How much further apart the fingers have got since the pinch began; 1 while none is on. */
    var live by remember { mutableFloatStateOf(1f) }
    /** Where they were between them when it began, and how far they have moved since. */
    var pivot by remember { mutableStateOf(Offset.Zero) }
    var shift by remember { mutableStateOf(Offset.Zero) }
    LaunchedEffect(zoom) {
        delay(RESHARPEN_MS)
        drawn = zoom
    }

    val gap = with(density) { PAGE_GAP.toPx() }
    val pages = remember(doc) { Pagination(doc, gap) }

    Box(
        Modifier
            .fillMaxSize()
            .clipToBounds()
            .onSizeChanged { viewport = it }
            .pointerInput(doc, viewport) {
                val decay = exponentialDecay<Float>()
                panZoom(
                    onGesture = { centroid, pan, step ->
                        if (live == 1f && step == 1f) {
                            // One finger: the column scrolls and the pages slide, both at once.
                            list.dispatchRawDelta(-pan.y)
                            panX = holdXAt(panX + pan.x, viewport.width, zoom)
                        } else {
                            // Two: the layer below carries all of it until they are lifted.
                            if (live == 1f) pivot = centroid
                            live = (live * step).coerceIn(MIN_ZOOM / zoom, MAX_ZOOM / zoom)
                            shift += pan
                        }
                    },
                    onEnd = { velocity ->
                        if (live != 1f) {
                            // The one place the column is laid out again, on a list that has not
                            // moved since the pinch began: where the reader was, plus what the
                            // fingers did to it, is where they have to be put back.
                            val above = pages.above(list, viewport.width, zoom)
                            val down = anchor(above, pivot.y, live, shift.y)
                            val across = anchor(-panX, pivot.x, live, shift.x)
                            zoom = (zoom * live).coerceIn(MIN_ZOOM, MAX_ZOOM)
                            panX = holdXAt(-across, viewport.width, zoom)
                            val (page, into) = pages.at(down, viewport.width, zoom)
                            list.requestScrollToItem(page, into)
                            live = 1f
                            shift = Offset.Zero
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
                // Required, not plain: the box would otherwise hold the column to the width of
                // the screen while the pages grew taller, which is a page squeezed sideways.
                .requiredWidth(with(density) { (viewport.width * zoom).toDp() })
                // A pinch that shrinks the layer shows more of the column than the screen holds,
                // so the column is made that much taller for as long as it lasts and the rows to
                // fill it are composed.
                .requiredHeight(with(density) { (viewport.height / live.coerceAtMost(1f)).toDp() })
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
                        renderPx = (viewport.width * drawn).toInt(),
                        theme = theme,
                        tool = tool,
                    )
                }
            }
        }
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
private class Pagination(private val doc: PdfModel, private val gap: Float) {
    private fun height(index: Int, width: Int, zoom: Float): Float {
        val size = doc.sizes.getOrNull(index) ?: return gap
        return size.height * (width * zoom / size.width) + gap
    }

    /** How much of the document is above the top of the screen, in pixels at [zoom]. */
    fun above(list: LazyListState, width: Int, zoom: Float): Float {
        var y = 0f
        for (i in 0 until list.firstVisibleItemIndex) y += height(i, width, zoom)
        return y + list.firstVisibleItemScrollOffset
    }

    /** The page, and the offset into it, that [y] pixels from the start lands on at [zoom]. */
    fun at(y: Float, width: Int, zoom: Float): Pair<Int, Int> {
        var left = y.coerceAtLeast(0f)
        for (i in 0 until doc.pageCount) {
            val h = height(i, width, zoom)
            if (left < h) return i to left.roundToInt()
            left -= h
        }
        return maxOf(doc.pageCount - 1, 0) to 0
    }
}

private val PAGE_GAP = 8.dp
private const val MIN_ZOOM = 1f
private const val MAX_ZOOM = 6f

/** How long the fingers rest before the page is drawn again at the zoom they left it at. */
private const val RESHARPEN_MS = 180L

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
    theme: Theme,
    tool: Tool,
) {
    val density = LocalDensity.current
    val scale = shownPx / pageWidth
    val heightPx = (pageHeight * scale).toInt()
    var bitmap by remember(index, theme) { mutableStateOf<ImageBitmap?>(null) }
    var generation by remember(index) { mutableIntStateOf(0) }
    val scope = rememberCoroutineScope()
    val colors = MaterialTheme.colorScheme

    LaunchedEffect(index, renderPx, theme, generation) {
        bitmap = doc.page(index, renderPx / pageWidth, theme)
    }

    // What is being drawn right now, in view pixels, before the core has it.
    val wet = remember { mutableStateListOf<Offset>() }

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
        bitmap?.let { image ->
            Canvas(Modifier.fillMaxSize()) {
                // Stretched to the size it is shown at rather than drawn 1:1, so a pinch moves
                // the page with the fingers and the sharper render catches up afterwards.
                drawImage(
                    image = image,
                    srcSize = IntSize(image.width, image.height),
                    dstSize = IntSize(size.width.toInt(), size.height.toInt()),
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

private fun Color.rgb(): UInt = (0xFFFFFF and toArgb()).toUInt()

private fun Color.dark(): Boolean = luminance() < 0.5f

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
