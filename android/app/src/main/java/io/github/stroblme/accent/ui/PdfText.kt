package io.github.stroblme.accent.ui

import io.github.stroblme.accent.ffi.Glyph
import io.github.stroblme.accent.ffi.Rect

/*
 * The text on a PDF's pages, as a selection sees it: runs of glyphs in the order pdfium reads
 * them, which is also the order the core's selection links count in. Plain arithmetic over the
 * core's glyphs, so it is tested without a device.
 */

/** One end of a selection: glyph [glyph] of page [page]. */
internal data class Caret(val page: Int, val glyph: Int) : Comparable<Caret> {
    override fun compareTo(other: Caret) = compareValuesBy(this, other, Caret::page, Caret::glyph)
}

/** A run of glyphs from [from] to [to], both included, in document order. */
internal data class Selection(val from: Caret, val to: Caret) {
    companion object {
        /** The run between two ends, whichever way round they were made. */
        fun between(a: Caret, b: Caret) = if (a <= b) Selection(a, b) else Selection(b, a)
    }
}

/** What a selection covers of one page: glyphs [start] until [end], the boxes and the text. */
internal data class Piece(
    val page: Int,
    val start: Int,
    val end: Int,
    val boxes: List<Rect>,
    val text: String,
)

/**
 * What a selection covers, page by page, or `null` while a page it crosses has not had its glyphs
 * read. Each page between the ends gives all of its glyphs, and the two at the ends give from or
 * up to the caret — the desktop's rule, which is what lets a selection run over a page break.
 */
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

/** The smallest box holding both. */
internal fun Rect.union(other: Rect) = Rect(
    minOf(left, other.left),
    minOf(top, other.top),
    maxOf(right, other.right),
    maxOf(bottom, other.bottom),
)

/** One match of a find: the page it is on, and the box it covers there. */
internal data class Hit(val page: Int, val box: Rect)

/**
 * What a find has found so far, in page order, and which of it the reader is on: [at], `null`
 * until the first has been gone to.
 */
internal data class Found(val hits: List<Hit> = emptyList(), val at: Int? = null) {
    /**
     * With one page's matches added in page order, [at] kept on the match it was on. A find walks
     * round from the page being read, so the pages before it arrive last and go in front.
     */
    fun plus(page: Int, boxes: List<Rect>): Found {
        if (boxes.isEmpty()) return this
        val i = hits.indexOfFirst { it.page > page }.let { if (it < 0) hits.size else it }
        val added = boxes.map { Hit(page, it) }
        val kept = at?.let { if (it >= i) it + added.size else it }
        return Found(hits.subList(0, i) + added + hits.subList(i, hits.size), kept)
    }

    /** The first match at or after [page]: the one a find lands on first. */
    fun from(page: Int): Int? = hits.indexOfFirst { it.page >= page }.takeIf { it >= 0 }

    /** The next match, or the one before, round the ends of the document. */
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
     * "3/12", as a note's find says it — or nothing while the pages are still being searched with
     * nothing found yet, and "None" once they all have been ([done]).
     */
    fun count(done: Boolean): String = when {
        hits.isEmpty() -> if (done) "None" else ""
        at == null -> "${hits.size}"
        else -> "${at + 1}/${hits.size}"
    }
}
