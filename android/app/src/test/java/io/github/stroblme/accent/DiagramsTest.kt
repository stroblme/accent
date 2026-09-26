package io.github.stroblme.accent

import androidx.compose.ui.graphics.Color
import io.github.stroblme.accent.ui.diagrams
import io.github.stroblme.accent.ui.page
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Which rendered note runs scripts, and that those scripts are only ever the app's own. Whether
 * mermaid then draws is the WebView's and wants a device.
 */
class DiagramsTest {
    private val fence = "<pre><code class=\"language-mermaid\">graph TD\n  A --&gt; B\n</code></pre>"

    /** As pulldown-cmark renders a fence: its language as the code's class, and nothing else. */
    @Test
    fun `only a mermaid fence is a diagram`() {
        assertTrue(diagrams(fence))
        assertFalse(diagrams("<pre><code class=\"language-python\">print()</code></pre>"))
        assertFalse("prose naming it", diagrams("<p>a language-mermaid fence</p>"))
        val quoted = "<code>&lt;code class=&quot;language-mermaid&quot;&gt;</code>"
        assertFalse("inline code quoting it", diagrams(quoted))
    }

    /** The note's own `<script>` would need the page's nonce, which it cannot know. */
    @Test
    fun `every script on a diagram page carries the policy's nonce, and a plain page has none`() {
        val html = page(fence + "<script>alert(1)</script>", Color.Black, Color.White, Color.Blue)
        val nonce = Regex("script-src 'nonce-([^']+)'").find(html)!!.groupValues[1]
        val head = html.substringBefore("<body>")
        assertEquals(2, Regex("<script nonce=\"$nonce\"").findAll(head).count())
        assertEquals(2, Regex("<script").findAll(head).count())
        assertTrue("a fresh nonce per page", nonce !in page(fence, Color.Black, Color.White, Color.Blue))

        assertFalse(page("<p>text</p>", Color.Black, Color.White, Color.Blue).contains("<script"))
    }
}
