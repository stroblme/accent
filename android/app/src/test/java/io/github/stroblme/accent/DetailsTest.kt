package io.github.stroblme.accent

import io.github.stroblme.accent.ui.noteCounts
import io.github.stroblme.accent.ui.paperSize
import io.github.stroblme.accent.ui.svgExtent
import org.junit.Assert.assertEquals
import org.junit.Test

class DetailsTest {
    @Test fun noteCountsMatchTheDesktop() {
        assertEquals(Triple(2, 6, 1), noteCounts("one 😀\n"))
        assertEquals(Triple(0, 0, 1), noteCounts(""))
    }

    @Test fun pageSizeNamesAndDimensions() {
        assertEquals("A4", paperSize(595f, 842f))
        assertEquals("US Letter", paperSize(792f, 612f))
        assertEquals("100 × 200 mm", paperSize(283.46f, 566.93f))
    }

    @Test fun svgUsesItsSizeOrViewBox() {
        assertEquals(120 to 80, svgExtent("120px", "80", null))
        assertEquals(100 to 200, svgExtent("100%", null, "0 0 100 200"))
        assertEquals(100 to 50, svgExtent("100", null, "0 0 200 100"))
        assertEquals(200 to 100, svgExtent(null, "100", "0 0 200 100"))
        assertEquals(null, svgExtent(null, null, "0 0 0 200"))
    }
}
