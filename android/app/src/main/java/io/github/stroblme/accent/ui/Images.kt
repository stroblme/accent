package io.github.stroblme.accent.ui

import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.util.LruCache
import android.util.Size
import android.webkit.WebResourceResponse
import android.webkit.WebView
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import io.github.stroblme.accent.ffi.Theme
import io.github.stroblme.accent.ffi.looksLikeDocument
import io.github.stroblme.accent.ffi.recolourImage
import io.github.stroblme.accent.ffi.recolourSvg
import java.io.ByteArrayInputStream
import java.io.ByteArrayOutputStream
import java.io.File
import java.nio.ByteBuffer
import kotlin.math.max

/**
 * What an image is to recolouring, which draws a scan, a plot, a diagram or a screenshot of text on
 * the dark page in a dark theme, as a PDF page is, and leaves a photo alone.
 */
enum class ImageKind {
    /** Decoded here, and recoloured when it reads as a document. */
    Raster,

    /** Always a drawing, and recoloured as a filter, so it stays vector. */
    Svg,

    /** Never recoloured: the decoder takes its first frame only, and an animation would stop. */
    Gif,
}

/** What an image file is to the recolouring rule, by its extension; null for anything else. */
fun imageKind(name: String): ImageKind? = when (name.substringAfterLast('.', "").lowercase()) {
    "png", "jpg", "jpeg", "webp", "bmp", "avif" -> ImageKind.Raster
    "svg" -> ImageKind.Svg
    "gif" -> ImageKind.Gif
    else -> null
}

/**
 * How an image is drawn: recoloured as a PDF page is ([pageTheme]) when it is a drawing — an SVG
 * always, a raster when it reads as a [document] — and as it is otherwise, which a photo is always
 * read as. [inverted] flips that either way. [document] costs a decode, so it is asked only in a
 * dark theme, the one place the answer changes anything.
 */
internal fun imageTheme(kind: ImageKind, dark: Boolean, inverted: Boolean, document: () -> Boolean): Theme =
    when (kind) {
        ImageKind.Gif -> Theme.Plain
        ImageKind.Svg -> pageTheme(dark, inverted)
        ImageKind.Raster -> pageTheme(dark && document(), inverted)
    }

/**
 * The images the reader has inverted by hand — a long press on one in a note, Invert on the image
 * screen — by path on this device. Kept for as long as the app's process runs and nowhere else: a
 * figure that came out wrong is put right for this reading, not written into the vault.
 */
object Inverted {
    var files by mutableStateOf(emptySet<String>())
        private set

    fun toggle(path: String) {
        files = if (path in files) files - path else files + path
    }
}

/**
 * [file] as a rendered page is to show it in a [dark] theme or a light one: as it is on disk, or
 * recoloured. A file that cannot be read is refused rather than thrown out of the WebView's thread.
 *
 * Both screens that show an image draw it in a WebView — a note in its page, the image screen on
 * its own — and both come through here, so one figure looks the same in either. Which image reads
 * as a document, and the remap itself, are the core's (`accent_core::recolour`), so it looks the
 * same on the desktop too; the decoding is the platform's.
 *
 * Called on the WebView's own loading thread, never the main one: a verdict can be a decode, and a
 * recolour always is.
 */
fun served(file: File, dark: Boolean): WebResourceResponse = runCatching {
    val kind = imageKind(file.name) ?: return@runCatching asItIs(file, null)
    when (val theme = imageTheme(kind, dark, file.path in Inverted.files) { document(file) }) {
        Theme.Plain -> asItIs(file, kind)
        is Theme.Recolour -> when (kind) {
            ImageKind.Svg -> recolourSvg(file.readText(), theme)
                ?.let { WebResourceResponse(SVG, "utf-8", ByteArrayInputStream(it.toByteArray())) }
            else -> recoloured(file, theme)
        } ?: asItIs(file, kind)
    }
}.getOrElse { blocked() }

/**
 * The file's own bytes. An SVG is named as one, which is the only way a WebView draws it: a raster
 * is recognised by its first bytes, a drawing is not.
 */
private fun asItIs(file: File, kind: ImageKind?) =
    WebResourceResponse(if (kind == ImageKind.Svg) SVG else null, null, file.inputStream())

/** A request refused: nothing but the vault's own files loads in a rendered page. */
internal fun blocked() = WebResourceResponse(null, null, null)

/**
 * Clear the WebViews' memory cache when what it holds was served in another palette, or under
 * another set of inverted files. Called before every page load.
 *
 * The cache is the app's rather than the view's, and a page loaded again takes an image it already
 * holds from there: without this a note would come back from a theme change, or an Invert, with
 * its images as they were. Main thread only, as every load is.
 */
fun WebView.freshen(dark: Boolean) {
    val now = dark to Inverted.files
    if (servedUnder.let { it != null && it != now }) clearCache(false)
    servedUnder = now
}

/** The palette and the inverted files the images in the WebViews' cache were served under. */
private var servedUnder: Pair<Boolean, Set<String>>? = null

/**
 * What [document] concluded, by [verdictKey]: a verdict is a decode, and a note is loaded again on
 * every palette change. Only the answer is kept, not the pixels, so the cache costs nothing.
 */
private val verdicts by lazy { LruCache<String, Boolean>(VERDICTS) }

/** A file as [verdicts] knows it: one written again is a different image. */
internal fun verdictKey(file: File): String = "${file.path}|${file.lastModified()}|${file.length()}"

/**
 * Whether [file] reads as a document, from a copy decoded small: the core samples about 65 000
 * pixels whatever it is given, so a long side of [CLASSIFIED_SIDE] gets the verdict a full decode
 * would at a fraction of the copying.
 *
 * Past [MAX_PIXELS] the image is not looked at, as the core leaves one alone — decoded small here,
 * it would otherwise be measured.
 */
private fun document(file: File): Boolean {
    val key = verdictKey(file)
    verdicts.get(key)?.let { return it }
    val size = bounds(file)
    val small = size?.takeIf { it.width.toLong() * it.height <= MAX_PIXELS }
        ?.let { decode(file, it, CLASSIFIED_SIDE) }
    val verdict = small != null &&
        looksLikeDocument(small.rgba(), small.width.toUInt(), small.height.toUInt())
    small?.recycle()
    verdicts.put(key, verdict)
    return verdict
}

/**
 * [file] recoloured onto [theme], as a PNG: straight alpha in and out, so a transparent figure keeps
 * its transparency. Decoded at a long side of [RECOLOURED_SIDE] at most, which keeps a large scan's
 * copies bounded. Null when the platform cannot decode it into RGBA8, and it is then served as it is.
 */
private fun recoloured(file: File, theme: Theme): WebResourceResponse? {
    val bitmap = bounds(file)?.let { decode(file, it, RECOLOURED_SIDE) } ?: return null
    bitmap.copyPixelsFromBuffer(ByteBuffer.wrap(recolourImage(bitmap.rgba(), theme)))
    val png = ByteArrayOutputStream()
    bitmap.compress(Bitmap.CompressFormat.PNG, 100, png)
    bitmap.recycle()
    return WebResourceResponse("image/png", null, ByteArrayInputStream(png.toByteArray()))
}

/** The image's size, read off its header alone; null for a file the platform cannot decode. */
private fun bounds(file: File): Size? {
    val options = BitmapFactory.Options().apply { inJustDecodeBounds = true }
    BitmapFactory.decodeFile(file.path, options)
    return Size(options.outWidth, options.outHeight).takeIf { it.width > 0 && it.height > 0 }
}

/**
 * [file] decoded with its long side brought down to at most [limit], as straight-alpha RGBA8 —
 * what the core reads and writes. Not premultiplied: a half-transparent pixel's colour would be
 * multiplied into its alpha before the core saw it, and could not be taken back out exactly.
 */
private fun decode(file: File, size: Size, limit: Int): Bitmap? {
    val options = BitmapFactory.Options().apply {
        inSampleSize = sampleSize(max(size.width, size.height), limit)
        inPreferredConfig = Bitmap.Config.ARGB_8888
        inPremultiplied = false
        inMutable = true
    }
    // A 16-bit PNG may still come back in half floats, which is not what the core reads.
    return BitmapFactory.decodeFile(file.path, options)?.takeIf { it.config == Bitmap.Config.ARGB_8888 }
}

/** The power of two `inSampleSize` takes to bring a [long] side down to at most [limit]. */
internal fun sampleSize(long: Int, limit: Int): Int {
    var n = 1
    while (long / n > limit) n *= 2
    return n
}

/**
 * The pixels in memory order, which for `ARGB_8888` is R, G, B, A — the core's layout — copied
 * without conversion, so a bitmap decoded without premultiplying hands over straight alpha.
 */
private fun Bitmap.rgba(): ByteArray = ByteBuffer.allocate(byteCount).also { copyPixelsToBuffer(it) }.array()

private const val SVG = "image/svg+xml"

/** The long side an image is classified at: 512 × 512 is four times what the core samples. */
private const val CLASSIFIED_SIDE = 512

/** The long side an image is recoloured at: sharp across a phone at any density, 16 MB decoded. */
private const val RECOLOURED_SIDE = 2048

/** The core's `recolour::MAX_PIXELS`: past it an image is left alone without being looked at. */
private const val MAX_PIXELS = 64_000_000L

/** How many verdicts are kept: a vault's worth of figures, at a few dozen bytes each. */
private const val VERDICTS = 1024
