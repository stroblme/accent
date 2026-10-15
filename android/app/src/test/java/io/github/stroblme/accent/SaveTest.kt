package io.github.stroblme.accent

import io.github.stroblme.accent.ffi.AccentException
import io.github.stroblme.accent.ffi.Etag
import io.github.stroblme.accent.ffi.Note
import org.junit.Assert.assertEquals
import org.junit.Assert.assertThrows
import org.junit.Test

/** [saveOver] treats a changed etag with identical text as no conflict. */
class SaveTest {
    private val read = Etag(1, 7uL, 1uL)
    private val touched = Etag(2, 7uL, 1uL)

    /** Gate a disk file at [etag] containing [text] as the core does. */
    private class Disk(var text: String, var etag: Etag) {
        fun save(text: String, expected: Etag?): Etag {
            if (expected != null && expected != etag) throw AccentException.ChangedOnDisk(etag)
            this.text = text
            etag = etag.copy(mtimeNs = etag.mtimeNs + 1)
            return etag
        }
    }

    @Test
    fun `a file only touched since it was read is written over`() {
        val disk = Disk("on disk", touched)
        saveOver(read, "on disk", { Note(disk.text, disk.etag) }) { disk.save("typed", it) }
        assertEquals("typed", disk.text)
    }

    @Test
    fun `a file somebody else wrote is refused`() {
        val disk = Disk("theirs", touched)
        assertThrows(AccentException.ChangedOnDisk::class.java) {
            saveOver(read, "on disk", { Note(disk.text, disk.etag) }) { disk.save("typed", it) }
        }
        assertEquals("theirs", disk.text)
    }
}
