package io.github.stroblme.accent.ui

import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.util.LruCache
import android.util.Size
import android.webkit.WebResourceResponse
import android.webkit.WebView
import io.github.stroblme.accent.OpenImage
import io.github.stroblme.accent.ffi.Theme
import io.github.stroblme.accent.ffi.looksLikeDocument
import io.github.stroblme.accent.ffi.recolourImage
import io.github.stroblme.accent.ffi.recolourSvg
import io.github.stroblme.accent.ffi.uprightImage
import java.io.ByteArrayInputStream
import java.io.ByteArrayOutputStream
import java.io.File
import java.nio.ByteBuffer
import kotlin.math.max

/** Image classification: documents and screenshots are recoloured for dark pages; photos are not. */
enum class ImageKind {
    /** Decoded here, and recoloured when it reads as a document. */
    Raster,

    /** Always a drawing, and recoloured as a filter, so it stays vector. */
    Svg,

    /** Animated images are never recoloured because decoding one frame would stop animation. */
    Animation,
}

/** Classify supported image extensions; mark animated files as [ImageKind.Animation]. */
fun imageKind(name: String): ImageKind? = when (name.substringAfterLast('.', "").lowercase()) {
    "png", "jpg", "jpeg", "webp", "gif", "bmp", "avif" -> ImageKind.Raster
    "svg" -> ImageKind.Svg
    else -> null
}

/**
 * Choose page theming for SVGs and document-like rasters; leave photos unchanged.
 * [inverted] swaps any image to the opposite page theme.
 */
internal fun imageTheme(kind: ImageKind, dark: Boolean, inverted: Boolean, document: () -> Boolean): Theme =
    when (kind) {
        ImageKind.Animation -> Theme.Plain
        ImageKind.Svg -> pageTheme(dark, inverted)
        ImageKind.Raster ->
            if (inverted) pageTheme(dark, inverted = true) else pageTheme(dark && document())
    }

/**
 * Serve [image] in the selected theme with [inverted] overrides, blocking unreadable files.
 * The core recolours documents; Android decodes them. Call on WebView's loading thread.
 */
fun served(
    image: OpenImage,
    dark: Boolean,
    inverted: Set<String> = Inverted.files,
): WebResourceResponse = runCatching {
    val file = File(image.path)
    val byName = imageKind(file.name) ?: return@runCatching asItIs(file, null)
    val kind = if (image.animated) ImageKind.Animation else byName
    when (val theme = imageTheme(kind, dark, file.path in inverted) { document(file) }) {
        Theme.Plain -> asItIs(file, kind)
        is Theme.Recolour -> when (kind) {
            ImageKind.Svg -> recolourSvg(file.readText(), theme, fill = !dark)
                ?.let { WebResourceResponse(SVG, "utf-8", ByteArrayInputStream(it.toByteArray())) }
            else -> recoloured(file, theme, fill = !dark)
        } ?: asItIs(file, kind)
    }
}.getOrElse { blocked() }

/** Serve original image bytes, retaining the SVG MIME type required by WebView. */
private fun asItIs(file: File, kind: ImageKind?) =
    WebResourceResponse(if (kind == ImageKind.Svg) SVG else null, null, file.inputStream())

/** A request refused: nothing but the vault's own files loads in a rendered page. */
internal fun blocked() = WebResourceResponse(null, null, null)

/** Clear cached images when theme or [inverted] paths change before a page loads. */
fun WebView.freshen(dark: Boolean, inverted: Set<String> = Inverted.files) {
    val now = dark to inverted
    if (servedUnder.let { it != null && it != now }) clearCache(false)
    servedUnder = now
}

/** The palette and the inverted files the images in the WebViews' cache were served under. */
private var servedUnder: Pair<Boolean, Set<String>>? = null

/** Cache only [document] verdicts by [verdictKey], avoiding repeated image decodes. */
private val verdicts by lazy { LruCache<String, Boolean>(VERDICTS) }

/** A file as [verdicts] knows it: one written again is a different image. */
internal fun verdictKey(file: File): String = "${file.path}|${file.lastModified()}|${file.length()}"

/** Classify [file] from a bounded decode, skipping images beyond [MAX_PIXELS]. */
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
 * Recolour [file] as PNG on [theme], bounding decode size and applying EXIF orientation.
 * Return null if RGBA8 decoding fails so the caller can serve the original.
 */
private fun recoloured(file: File, theme: Theme, fill: Boolean): WebResourceResponse? {
    val bitmap = bounds(file)?.let { decode(file, it, RECOLOURED_SIDE) } ?: return null
    val turned = uprightImage(bitmap.rgba(), bitmap.width.toUInt(), bitmap.height.toUInt(), file.path)
    bitmap.reconfigure(turned.width.toInt(), turned.height.toInt(), Bitmap.Config.ARGB_8888)
    bitmap.copyPixelsFromBuffer(ByteBuffer.wrap(recolourImage(turned.rgba, theme, fill)))
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

/** Decode [file] to straight-alpha RGBA8 with long side at most [limit]. */
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

/** `ARGB_8888` bytes are RGBA on this device, matching the core layout without conversion. */
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
