package io.github.stroblme.accent

import io.github.stroblme.accent.ffi.NoteAlias
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/**
 * The switcher's corpus: what is ranked, in which order, and what each place in it stands for.
 *
 * The ranking itself is the core's and tested there; its ties go to the earlier place, so the
 * order here is what puts a file ahead of a note only linked to and a path ahead of an alias.
 */
class CorpusTest {
    private val corpus = Corpus(
        files = listOf("Projects/Real Name.md", "Nick notes.md"),
        missing = listOf("Nowhere/Ghost.md"),
        aliases = listOf(
            NoteAlias(name = "Nickname", relPath = "Projects/Real Name.md"),
            NoteAlias(name = "Nickname", relPath = "Nick notes.md"),
        ),
    )

    @Test
    fun `aliases rank behind the paths and open the note that carries them`() {
        assertEquals(
            listOf(
                "Projects/Real Name.md",
                "Nick notes.md",
                "Nowhere/Ghost.md",
                "Nickname",
                "Nickname",
            ),
            corpus.names,
        )
        assertEquals(Corpus.Row("Real Name.md", "Projects/Real Name.md"), corpus.row(0))
        assertEquals(Corpus.Row("Ghost.md", "Nowhere/Ghost.md", unwritten = true), corpus.row(2))
        // One alias on two notes is two rows, each opening its own.
        assertEquals(Corpus.Row("Nickname", "Projects/Real Name.md"), corpus.row(3))
        assertEquals(Corpus.Row("Nickname", "Nick notes.md"), corpus.row(4))
    }

    @Test
    fun `a recent note is found among the paths and never among the aliases`() {
        assertEquals(1, corpus.find("Nick notes.md"))
        assertEquals(2, corpus.find("Nowhere/Ghost.md"))
        assertNull(corpus.find("Nickname"))
    }
}
