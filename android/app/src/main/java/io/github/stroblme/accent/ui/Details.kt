package io.github.stroblme.accent.ui

import android.graphics.BitmapFactory
import android.net.Uri
import android.provider.DocumentsContract
import android.provider.OpenableColumns
import android.text.format.Formatter
import android.util.Xml
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import android.content.Context
import io.github.stroblme.accent.PdfModel
import io.github.stroblme.accent.ffi.Etag
import io.github.stroblme.accent.ffi.analyzeUtf16
import java.text.DateFormat
import java.io.File
import java.util.Date
import java.util.Locale
import org.xmlpull.v1.XmlPullParser
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import kotlin.math.abs
import kotlin.math.roundToInt

data class DetailGroup(val title: String, val facts: List<Pair<String, String>>)

/** The document bar's secondary actions. */
@Composable
fun DetailsMenu(onDetails: () -> Unit) {
    var expanded by remember { mutableStateOf(false) }
    Box {
        TextButton(
            onClick = { expanded = true },
            modifier = Modifier.semantics { contentDescription = "More" },
        ) { Text("⋮") }
        DropdownMenu(expanded = expanded, onDismissRequest = { expanded = false }) {
            DropdownMenuItem(text = { Text("Details") }, onClick = {
                expanded = false
                onDetails()
            })
        }
    }
}

/** Read facts only while open; file stat and PDF metadata can block. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun DetailsSheet(onDismiss: () -> Unit, load: suspend () -> List<DetailGroup>) {
    var groups by remember { mutableStateOf<List<DetailGroup>?>(null) }
    LaunchedEffect(Unit) { groups = load() }
    ModalBottomSheet(onDismissRequest = onDismiss) {
        Column(Modifier.fillMaxWidth().verticalScroll(rememberScrollState()).padding(bottom = 24.dp)) {
            Text("Details", style = MaterialTheme.typography.titleLarge, modifier = Modifier.padding(Gutter))
            if (groups == null) {
                Box(Modifier.fillMaxWidth().padding(24.dp), contentAlignment = Alignment.Center) {
                    CircularProgressIndicator()
                }
            }
            groups.orEmpty().forEach { group ->
                Text(
                    group.title,
                    style = MaterialTheme.typography.titleSmall,
                    color = MaterialTheme.colorScheme.primary,
                    modifier = Modifier.padding(horizontal = Gutter, vertical = 8.dp),
                )
                group.facts.forEach { (name, value) ->
                    Row(Modifier.fillMaxWidth().padding(horizontal = Gutter, vertical = 10.dp)) {
                        Text(name, modifier = Modifier.weight(1f), color = MaterialTheme.colorScheme.onSurfaceVariant)
                        Text(value, modifier = Modifier.weight(2f))
                    }
                }
            }
        }
    }
}

fun fileDetails(etag: Etag?, context: Context): List<DetailGroup> =
    fileGroup(context, etag?.size?.toLong(), etag?.mtimeNs?.div(1_000_000))

private fun fileGroup(context: Context, size: Long?, modifiedMs: Long?): List<DetailGroup> {
    val facts = mutableListOf<Pair<String, String>>()
    if (size != null && size >= 0) facts += "Size" to Formatter.formatShortFileSize(context, size)
    if (modifiedMs != null && modifiedMs > 0) facts += "Modified" to date(modifiedMs / 1000)
    return if (facts.isEmpty()) emptyList() else listOf(DetailGroup("File", facts))
}

/** A PDF handed in by another app may expose size and modified time through its provider. */
suspend fun uriDetails(context: Context, uri: Uri): List<DetailGroup> = withContext(Dispatchers.IO) {
    if (uri.scheme == "file") {
        val file = File(uri.path.orEmpty())
        return@withContext if (file.isFile) fileGroup(context, file.length(), file.lastModified())
        else emptyList()
    }
    val stamp = runCatching {
        context.contentResolver.query(uri, null, null, null, null)?.use { cursor ->
            if (!cursor.moveToFirst()) return@use null
            fun value(name: String): Long? {
                val index = cursor.getColumnIndex(name)
                return if (index >= 0 && !cursor.isNull(index)) cursor.getLong(index) else null
            }
            value(OpenableColumns.SIZE) to value(DocumentsContract.Document.COLUMN_LAST_MODIFIED)
        }
    }.getOrNull()
    fileGroup(context, stamp?.first, stamp?.second)
}

/** Words split on whitespace, Unicode characters, and lines as the desktop counts them. */
internal fun noteCounts(text: String): Triple<Int, Int, Int> {
    var words = 0
    var inWord = false
    text.forEach { char ->
        if (char.isWhitespace()) inWord = false
        else if (!inWord) { words++; inWord = true }
    }
    val characters = text.codePointCount(0, text.length)
    val lines = text.count { it == '\n' } + 1 - if (text.endsWith('\n')) 1 else 0
    return Triple(words, characters, lines)
}

fun noteDetails(text: String): DetailGroup {
    val (words, characters, lines) = noteCounts(text)
    val links = analyzeUtf16(text).links.size
    return DetailGroup("Note", listOf(
        "Words" to words.toString(),
        "Characters" to characters.toString(),
        "Lines" to lines.toString(),
        "Links" to links.toString(),
    ))
}

/** Paper names agree with the desktop for the two common sizes; others show millimetres. */
internal fun paperSize(width: Float, height: Float): String {
    val (short, long) = if (width <= height) width to height else height to width
    fun near(a: Float, b: Float) = abs(a - b) <= 1f
    return when {
        near(short, 595f) && near(long, 842f) -> "A4"
        near(short, 612f) && near(long, 792f) -> "US Letter"
        else -> "${(width * 25.4f / 72f).roundToInt()} × ${(height * 25.4f / 72f).roundToInt()} mm"
    }
}

suspend fun pdfDetails(pdf: PdfModel): DetailGroup {
    val info = pdf.info()
    val facts = mutableListOf("Pages" to pdf.pageCount.toString())
    pdf.sizes.firstOrNull()?.let { facts += "Page Size" to paperSize(it.width, it.height) }
    listOf(
        "Title" to info.title,
        "Author" to info.author,
        "Subject" to info.subject,
        "Keywords" to info.keywords,
        "Application" to info.creator,
        "Producer" to info.producer,
        "Created" to info.created?.let(::date),
        "PDF Version" to info.version,
    ).forEach { (name, value) -> if (value != null) facts += name to value }
    return DetailGroup("PDF", facts)
}

fun imageDetails(path: String): DetailGroup {
    val options = BitmapFactory.Options().apply { inJustDecodeBounds = true }
    BitmapFactory.decodeFile(path, options)
    val facts = mutableListOf<Pair<String, String>>()
    val dimensions = if (options.outWidth > 0 && options.outHeight > 0) {
        options.outWidth to options.outHeight
    } else if (path.endsWith(".svg", ignoreCase = true)) {
        svgDimensions(path)
    } else null
    if (dimensions != null) {
        facts += "Dimensions" to "${dimensions.first} × ${dimensions.second}"
    }
    path.substringAfterLast('.', "").takeIf { it.isNotEmpty() }?.let {
        facts += "Format" to it.uppercase(Locale.ROOT)
    }
    return DetailGroup("Image", facts)
}

/** SVG unitless and px sizes are pixels; a viewBox supplies a dimensionless SVG's size. */
internal fun svgExtent(width: String?, height: String?, viewBox: String?): Pair<Int, Int>? {
    val px = Regex("\\s*(\\d+(?:\\.\\d+)?)(?:px)?\\s*")
    fun positive(value: Float?) = value?.takeIf { it.isFinite() && it > 0f }?.roundToInt()
    val w = positive(width?.let { px.matchEntire(it)?.groupValues?.get(1)?.toFloatOrNull() })
    val h = positive(height?.let { px.matchEntire(it)?.groupValues?.get(1)?.toFloatOrNull() })
    if (w != null && h != null) return w to h
    val box = viewBox?.trim()?.split(Regex("[\\s,]+")) ?: return null
    if (box.size != 4) return null
    val boxW = box[2].toFloatOrNull()?.takeIf { it.isFinite() && it > 0f } ?: return null
    val boxH = box[3].toFloatOrNull()?.takeIf { it.isFinite() && it > 0f } ?: return null
    return when {
        w != null -> w to (positive(w * boxH / boxW) ?: return null)
        h != null -> (positive(h * boxW / boxH) ?: return null) to h
        else -> (positive(boxW) ?: return null) to (positive(boxH) ?: return null)
    }
}

private fun svgDimensions(path: String): Pair<Int, Int>? = runCatching {
    File(path).inputStream().use { stream ->
        val xml = Xml.newPullParser()
        xml.setInput(stream, null)
        while (xml.eventType != XmlPullParser.START_TAG && xml.eventType != XmlPullParser.END_DOCUMENT) {
            xml.next()
        }
        if (xml.name != "svg") null else svgExtent(
            xml.getAttributeValue(null, "width"),
            xml.getAttributeValue(null, "height"),
            xml.getAttributeValue(null, "viewBox"),
        )
    }
}.getOrNull()

private fun date(seconds: Long): String = DateFormat.getDateTimeInstance().format(Date(seconds * 1000))
