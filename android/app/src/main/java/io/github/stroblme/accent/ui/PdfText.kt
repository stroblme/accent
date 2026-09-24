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
