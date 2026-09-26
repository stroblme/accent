package io.github.stroblme.accent

import io.github.stroblme.accent.ffi.Backlink
import io.github.stroblme.accent.ffi.TagCount
import io.github.stroblme.accent.ui.listed
import io.github.stroblme.accent.ui.tagHeading
import io.github.stroblme.accent.ui.title
import org.junit.Assert.assertEquals
import org.junit.Test

/**
 * Browse's Tags and Backlinks pages, as far as they are not the core's: which notes a document's
 * backlinks are, what a row and a heading say, and which order a list keeps. The ranking is the
 * switcher's and tested there.
 */
class ListsTest {
    /** A note linking three times is one backlink, and the index's path order is kept. */
    @Test
    fun `a note is listed once however often it links`() {
        val links = listOf(
            Backlink("Journal/Monday.md", 10, 20),
            Backlink("Journal/Monday.md", 80, 90),
            Backlink("Projects/X.md", 5, 9),
        )
        assertEquals(listOf("Journal/Monday.md", "Projects/X.md"), linkingNotes(links))
    }

    @Test
    fun `a note is named as its bar names it, a tag with its count`() {
        assertEquals("Weekly review", title("Journal/2026/Weekly review.md"))
        assertEquals("paper.pdf", title("Papers/paper.pdf"))
        assertEquals("#meeting · 12", tagHeading(TagCount("meeting", 12)))
    }

    /**
     * No query keeps the list's own order, which for the tags is the core's by count, and all of
     * it; a query hands the list to the ranking.
     */
    @Test
    fun `the order is the list's own until there is a query`() {
        val byCount = List(300) { "tag$it" }
        val never = { _: String, _: List<String> -> error("ranked with no query") }
        assertEquals(byCount.indices.toList(), listed("", byCount, never))
        assertEquals(byCount.indices.toList(), listed("  ", byCount, never))
        assertEquals(listOf(2, 0), listed("t", byCount) { _, _ -> listOf(2, 0) })
    }
}
