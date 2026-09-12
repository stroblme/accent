package io.github.stroblme.accent.ui

import android.net.Uri
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.gestures.detectDragGestures
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.graphics.luminance
import androidx.compose.ui.graphics.toArgb
import androidx.compose.ui.input.pointer.PointerType
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.unit.dp
import io.github.stroblme.accent.PdfModel
import io.github.stroblme.accent.ffi.InkStyle
import io.github.stroblme.accent.ffi.Point
import io.github.stroblme.accent.ffi.Theme
import kotlinx.coroutines.launch

/** What the pen is doing. A finger never draws: it moves the page. */
enum class Tool { Read, Pen, Highlighter, Eraser }

/** A PDF inside a vault: strokes are written back into the file it came from. */
@Composable
fun PdfScreen(path: String, onClose: () -> Unit, onMenu: () -> Unit) {
    var doc by remember(path) { mutableStateOf<PdfModel?>(null) }
    var failed by remember(path) { mutableStateOf<String?>(null) }
    LaunchedEffect(path) {
        runCatching { PdfModel.open(path) }
            .onSuccess { doc = it }
            .onFailure { failed = it.message ?: "This file could not be opened." }
    }
    DisposableEffect(path) { onDispose { doc?.close() } }
    Reader(doc, failed, onLeft = onMenu, leftLabel = "Files", onSave = { it.save() })
}

/** A PDF opened from somewhere else: there is no vault, so it is written back where it came from. */
@Composable
fun LoosePdfScreen(uri: Uri, onClose: () -> Unit) {
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
    Reader(doc, failed, onLeft = onClose, leftLabel = "Close", onSave = { model ->
        // No path on this side: the bytes go back through whatever handed them over.
        val bytes = model.bytes() ?: return@Reader Result.failure(Exception("Nothing to write"))
        runCatching {
            context.contentResolver.openOutputStream(uri, "wt")!!.use { it.write(bytes) }
        }
    })
}

@Composable
private fun Reader(
    doc: PdfModel?,
    failed: String?,
    onLeft: () -> Unit,
    leftLabel: String,
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

    Scaffold(snackbarHost = { SnackbarHost(snackbar) }) { padding ->
        Box(Modifier.fillMaxSize().padding(padding)) {
            Pages(doc, tool)
            PdfToolbar(
                tool = tool,
                onTool = { tool = it },
                canUndo = doc.canUndo,
                canRedo = doc.canRedo,
                dirty = doc.dirty,
                leftLabel = leftLabel,
                onLeft = onLeft,
                onUndo = { scope.launch { doc.undo() } },
                onRedo = { scope.launch { doc.redo() } },
                onSave = {
                    scope.launch {
                        onSave(doc).onFailure {
                            snackbar.showSnackbar(it.message ?: "This file could not be saved.")
                        }
                    }
                },
                modifier = Modifier.align(Alignment.BottomEnd).padding(Gutter),
            )
        }
    }
}

/**
 * The document as a column of pages.
 *
 * Each page paints one bitmap the core rendered at the width it is shown at. Tiles are what the
 * desktop does at high zoom; at a phone's width a page is already about one tile, so a page is
 * the unit here and the ceiling is the memory a long document's visible pages take.
 */
@Composable
private fun Pages(doc: PdfModel, tool: Tool) {
    val list = rememberLazyListState()
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
    var width by remember { mutableStateOf(0) }

    LazyColumn(
        state = list,
        modifier = Modifier.fillMaxSize().onSizeChanged { width = it.width },
        verticalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        items(doc.pageCount) { index ->
            val size = doc.sizes.getOrNull(index)
            if (size != null && width > 0) {
                Page(doc, index, size.width, size.height, width, theme, tool)
            }
        }
    }
}

@Composable
private fun Page(
    doc: PdfModel,
    index: Int,
    pageWidth: Float,
    pageHeight: Float,
    widthPx: Int,
    theme: Theme,
    tool: Tool,
) {
    val density = LocalDensity.current
    val scale = widthPx / pageWidth
    val heightPx = (pageHeight * scale).toInt()
    var bitmap by remember(index, widthPx, theme) { mutableStateOf<ImageBitmap?>(null) }
    var generation by remember(index) { mutableIntStateOf(0) }
    val scope = rememberCoroutineScope()
    val colors = MaterialTheme.colorScheme

    LaunchedEffect(index, widthPx, theme, generation) {
        bitmap = doc.page(index, scale, theme)
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
                                Tool.Eraser -> {
                                    points.zipWithNext { a, b ->
                                        doc.erase(index, a, b, ERASER_RADIUS, partial = false)
                                    }
                                }
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
                drawImage(image)
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
    dirty: Boolean,
    leftLabel: String,
    onLeft: () -> Unit,
    onUndo: () -> Unit,
    onRedo: () -> Unit,
    onSave: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Surface(
        modifier = modifier,
        shape = MaterialTheme.shapes.extraLarge,
        color = MaterialTheme.colorScheme.surface,
        tonalElevation = 0.dp,
        shadowElevation = 6.dp,
    ) {
        // The pill is as wide as it needs to be, up to the screen less its gutters, and scrolls
        // rather than clipping: eight labels do not fit a phone in portrait.
        Row(
            Modifier
                .horizontalScroll(rememberScrollState())
                .padding(horizontal = 8.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            TextButton(onClick = onLeft) { Text(leftLabel) }
            for (each in Tool.entries) {
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
            TextButton(onClick = onSave, enabled = dirty) { Text("Save") }
        }
    }
}
