package io.github.stroblme.accent

import io.github.stroblme.accent.ffi.NoteAlias
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/**
 * Switcher corpus order determines ranking ties among indexed files, ignored files, links, and aliases.
 */
class CorpusTest {
    private val corpus = Corpus(
        files = listOf("Projects/Real Name.md", "Nick notes.md"),
        // The index holds one of these already, and its row is the indexed one.
        ignored = listOf("build/Deep Note.md", "Nick notes.md"),
        missing = listOf("Nowhere/Ghost.md"),
        aliases = listOf(
            NoteAlias(name = "Nickname", relPath = "Projects/Real Name.md"),
            NoteAlias(name = "Nickname", relPath = "Nick notes.md"),
        ),
    )

    @Test
    fun `ignored notes rank behind the files and aliases behind the paths`() {
        assertEquals(
            listOf(
                "Projects/Real Name.md",
                "Nick notes.md",
                "build/Deep Note.md",
                "Nowhere/Ghost.md",
                "Nickname",
                "Nickname",
            ),
            corpus.names,
        )
        assertEquals(Corpus.Row("Real Name.md", "Projects/Real Name.md"), corpus.row(0))
        assertEquals(Corpus.Row("Deep Note.md", "build/Deep Note.md", ignored = true), corpus.row(2))
        assertEquals(Corpus.Row("Ghost.md", "Nowhere/Ghost.md", unwritten = true), corpus.row(3))
        // One alias on two notes is two rows, each opening its own.
        assertEquals(Corpus.Row("Nickname", "Projects/Real Name.md"), corpus.row(4))
        assertEquals(Corpus.Row("Nickname", "Nick notes.md"), corpus.row(5))
    }

    /**
     * Coalesce requests during a name load into one follow-up load, including ignored folders if requested.
     */
    @Test
    fun `a load asked for again while it runs runs once more after it`() {
        val reload = Reload()
        assertEquals(true, reload.ask(walk = true))
        assertEquals(false, reload.ask(walk = false))
        assertEquals(false, reload.ask(walk = true))
        assertEquals(false, reload.ask(walk = false))
        assertEquals(true, reload.next())
        assertNull(reload.next())
        // Idle again: the next asking starts at once, and alone it is followed by nothing.
        assertEquals(true, reload.ask(walk = false))
        assertNull(reload.next())
    }

    @Test
    fun `a recent note is found among the paths and never among the aliases`() {
        assertEquals(1, corpus.find("Nick notes.md"))
        assertEquals(2, corpus.find("build/Deep Note.md"))
        assertEquals(3, corpus.find("Nowhere/Ghost.md"))
        assertNull(corpus.find("Nickname"))
    }
}
