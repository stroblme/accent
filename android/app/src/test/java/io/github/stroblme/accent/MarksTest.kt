package io.github.stroblme.accent

import io.github.stroblme.accent.ffi.FileKind
import io.github.stroblme.accent.ffi.FileRow
import io.github.stroblme.accent.ui.RowMark
import io.github.stroblme.accent.ui.markOf
import org.junit.Assert.assertEquals
import org.junit.Test

/** Browse's tree marks a link to nothing and a second path to a file as the desktop's does. */
class MarksTest {
    private fun row(kind: FileKind, dangling: Boolean = false, alias: Boolean = false) =
        FileRow(0, "a.md", kind, null, 0, 0, false, dangling, alias)

    @Test
    fun `a link to nothing is marked whatever its name makes it`() {
        assertEquals(RowMark.Dangling, markOf(row(FileKind.MARKDOWN, dangling = true)))
        assertEquals(RowMark.Dangling, markOf(row(FileKind.DIR, dangling = true)))
    }

    @Test
    fun `a second path to a file is marked and any other row is not`() {
        assertEquals(RowMark.Alias, markOf(row(FileKind.PDF, alias = true)))
        assertEquals(RowMark.None, markOf(row(FileKind.MARKDOWN)))
    }
}
