package io.github.stroblme.accent

import io.github.stroblme.accent.ffi.NoteAlias
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/**
 * The switcher's corpus: what is ranked, in which order, and what each place in it stands for.
 *
 * The ranking itself is the core's and tested there; its ties go to the earlier place, so the
 * order here is what puts an indexed file ahead of one in a gitignored folder, a file ahead of a
 * note only linked to, and a path ahead of an alias.
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

    @Test
    fun `a recent note is found among the paths and never among the aliases`() {
        assertEquals(1, corpus.find("Nick notes.md"))
        assertEquals(2, corpus.find("build/Deep Note.md"))
        assertEquals(3, corpus.find("Nowhere/Ghost.md"))
        assertNull(corpus.find("Nickname"))
    }
}
