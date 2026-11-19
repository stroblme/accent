package io.github.stroblme.accent

import io.github.stroblme.accent.ffi.FileKind
import io.github.stroblme.accent.ffi.FileRow
import io.github.stroblme.accent.ui.RowMark
import io.github.stroblme.accent.ui.markOf
import org.junit.Assert.assertEquals
import org.junit.Test

/**
 * Browse's tree marks a link to nothing, a second path to a file and a row the index does not
 * hold as the desktop's does.
 */
class MarksTest {
    private fun row(
        kind: FileKind,
        id: Long = 1,
        dangling: Boolean = false,
        alias: Boolean = false,
    ) = FileRow(id, "a.md", kind, null, 0, 0, false, dangling, alias)

    @Test
    fun `a link to nothing is marked whatever its name makes it`() {
        assertEquals(RowMark.Dangling, markOf(row(FileKind.MARKDOWN, id = 0, dangling = true)))
        assertEquals(RowMark.Dangling, markOf(row(FileKind.DIR, id = 0, dangling = true)))
    }

    @Test
    fun `a second path to a file is marked and any other row is not`() {
        assertEquals(RowMark.Alias, markOf(row(FileKind.PDF, id = 0, alias = true)))
        assertEquals(RowMark.None, markOf(row(FileKind.MARKDOWN)))
    }

    @Test
    fun `a row the index does not hold is dimmed, a folder as a file`() {
        assertEquals(RowMark.Unindexed, markOf(row(FileKind.MARKDOWN, id = 0)))
        assertEquals(RowMark.Unindexed, markOf(row(FileKind.DIR, id = 0)))
    }
}
