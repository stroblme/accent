package io.github.stroblme.accent.ui

import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.isSpecified
import androidx.compose.ui.geometry.lerp
import io.github.stroblme.accent.ffi.Glyph
import io.github.stroblme.accent.ffi.Point
import io.github.stroblme.accent.ffi.Rect
import kotlin.math.abs

/*
 * The text on a PDF's pages, as a selection sees it: runs of glyphs in the order pdfium reads
 * them, which is also the order the core's selection links count in. Plain arithmetic over the
 * core's glyphs, so it is tested without a device.
 */

/** Selection endpoint at [glyph] on [page]. */
internal data class Caret(val page: Int, val glyph: Int) : Comparable<Caret> {
    override fun compareTo(other: Caret) = compareValuesBy(this, other, Caret::page, Caret::glyph)
}

/** Glyph run from [from] through [to], in document order. */
internal data class Selection(val from: Caret, val to: Caret) {
    companion object {
        /** Order two selection endpoints into a glyph run. */
        fun between(a: Caret, b: Caret) = if (a <= b) Selection(a, b) else Selection(b, a)
    }
}

/** Selected glyphs, boxes, and text on one page, from [start] up to [end]. */
internal data class Piece(
    val page: Int,
    val start: Int,
    val end: Int,
    val boxes: List<Rect>,
    val text: String,
)

/** Return selected glyphs per page, or `null` while a spanned page has no glyph data. */
internal fun Selection.pieces(glyphs: Map<Int, List<Glyph>>): List<Piece>? {
    val out = mutableListOf<Piece>()
    for (page in from.page..to.page) {
        val on = glyphs[page] ?: return null
        val lo = if (page == from.page) from.glyph else 0
        val hi = minOf(if (page == to.page) to.glyph else on.lastIndex, on.lastIndex)
        if (lo > hi) continue
        val picked = on.subList(lo, hi + 1)
        out += Piece(
            page,
            lo,
            hi + 1,
            // A glyph with no box of its own — a space between words — would paint as a dot.
            picked.map { it.rect }.filter { it.right > it.left && it.bottom > it.top },
            picked.joinToString("") { it.ch },
        )
    }
    return out
}

/** Smallest box containing both rectangles. */
internal fun Rect.union(other: Rect) = Rect(
    minOf(left, other.left),
    minOf(top, other.top),
    maxOf(right, other.right),
    maxOf(bottom, other.bottom),
)

/** Find match and its page-local box. */
internal data class Hit(val page: Int, val box: Rect)

/** Find matches in page order and current index [at], if selected. */
internal data class Found(val hits: List<Hit> = emptyList(), val at: Int? = null) {
    /** Add page matches in order while preserving the current match as earlier pages arrive. */
    fun plus(page: Int, boxes: List<Rect>): Found {
        if (boxes.isEmpty()) return this
        val i = hits.indexOfFirst { it.page > page }.let { if (it < 0) hits.size else it }
        val added = boxes.map { Hit(page, it) }
        val kept = at?.let { if (it >= i) it + added.size else it }
        return Found(hits.subList(0, i) + added + hits.subList(i, hits.size), kept)
    }

    /** First match on or after [page]. */
    fun from(page: Int): Int? = hits.indexOfFirst { it.page >= page }.takeIf { it >= 0 }

    /** Step forward or backward through matches, wrapping at document ends. */
    fun step(forward: Boolean): Found {
        if (hits.isEmpty()) return this
        val n = hits.size
        val next = when (val now = at) {
            null -> if (forward) 0 else n - 1
            else -> if (forward) (now + 1) % n else (now + n - 1) % n
        }
        return copy(at = next)
    }

    /**
     * Format match progress, leaving it blank during an unfinished empty search and showing "None"
     * when [done].
     */
    fun count(done: Boolean): String = when {
        hits.isEmpty() -> if (done) "None" else ""
        at == null -> "${hits.size}"
        else -> "${at + 1}/${hits.size}"
    }
}

/**
 * Find the glyph containing a point, or the nearest glyph on its line; return `null` for no text.
 */
internal fun nearest(glyphs: List<Glyph>, at: Point): Int? {
    var best: Int? = null
    var bestDistance = Float.MAX_VALUE
    for ((i, glyph) in glyphs.withIndex()) {
        val r = glyph.rect
        if (at.x in r.left..r.right && at.y in r.top..r.bottom) return i
        // Distance to the box, nothing along an axis the point already lies within.
        val dx = maxOf(r.left - at.x, 0f, at.x - r.right)
        val dy = maxOf(r.top - at.y, 0f, at.y - r.bottom)
        val distance = dx * dx + dy * dy
        if (distance < bestDistance) {
            bestDistance = distance
            best = i
        }
    }
    return best
}

/**
 * Where a loupe magnifies for the end at glyph [at] under a finger [x] across the page, both in
 * points, as the platform's text magnifier does (`Editor.obtainMagnifierShowCoordinates`): the
 * middle of the end's line, at the finger's x kept within the line, and nothing once the finger is
 * more than [reach] past it. A space is taken at the glyph before it, having no box of its own.
 */
internal fun loupeAt(glyphs: List<Glyph>, at: Int, x: Float, reach: Float): Point? {
    fun boxed(i: Int) = glyphs[i].rect.let { it.right > it.left && it.bottom > it.top }
    val end = (minOf(at, glyphs.lastIndex) downTo 0).firstOrNull(::boxed) ?: return null
    val box = glyphs[end].rect
    // The line runs on either side while the boxes share its height, past the spaces between.
    fun onLine(i: Int) = !boxed(i) || glyphs[i].rect.run { top < box.bottom && box.top < bottom }
    var lo = end
    var hi = end
    while (lo > 0 && onLine(lo - 1)) lo--
    while (hi < glyphs.lastIndex && onLine(hi + 1)) hi++
    val line = (lo..hi).filter(::boxed).map { glyphs[it].rect }
    val kept = x.coerceIn(line.minOf { it.left }, line.maxOf { it.right })
    if (abs(x - kept) > reach) return null
    return Point(kept, (box.top + box.bottom) / 2)
}

/**
 * Whether the loupe, last asked to be at [was], slides to [to] rather than going straight there:
 * onto another line, as the platform's text magnifier does (`Editor.MagnifierMotionAnimator`),
 * but not as it comes up or along a line, where it keeps to the finger.
 */
internal fun slides(was: Offset, to: Offset) = was.isSpecified && to.isSpecified && was.y != to.y

/**
 * Where a loupe sliding from [from] onto [to] is once it has gone [done] of the way: on the
 * straight line toward where the end is now, so it follows the finger along the new line meanwhile.
 */
internal fun slid(from: Offset, to: Offset, done: Float): Offset =
    if (from.isSpecified && to.isSpecified) lerp(from, to, done) else to

/** Select the alphanumeric word around glyph [at], or that glyph alone. */
internal fun wordAt(glyphs: List<Glyph>, at: Int): IntRange {
    fun letter(i: Int) = glyphs[i].ch.firstOrNull()?.isLetterOrDigit() == true
    if (!letter(at)) return at..at
    var lo = at
    var hi = at
    while (lo > 0 && letter(lo - 1)) lo--
    while (hi < glyphs.lastIndex && letter(hi + 1)) hi++
    return lo..hi
}

/** Extend the pressed word toward [caret] without dropping the original word. */
internal fun Selection.grownTo(caret: Caret): Selection = when {
    caret < from -> Selection(caret, to)
    caret > to -> Selection(from, caret)
    else -> this
}

/** Join selected text with a line break at each page boundary. */
internal fun List<Piece>.text(): String = joinToString("\n") { it.text }
