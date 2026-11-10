package io.github.stroblme.accent

import io.github.stroblme.accent.ffi.Sort
import org.junit.Assert.assertEquals
import org.junit.Test

/** The sort a vault was left in, as it is read back. */
class SortTest {
    @Test
    fun `a stored sort comes back, and one not offered here is name A to Z`() {
        for (sort in Sorts) assertEquals(sort, sortNamed(sort.name))
        assertEquals(Sort.NAME_ASC, sortNamed(null))
        assertEquals(Sort.NAME_ASC, sortNamed(Sort.CREATED_NEW.name))
        assertEquals(Sort.NAME_ASC, sortNamed("by-colour"))
    }
}
