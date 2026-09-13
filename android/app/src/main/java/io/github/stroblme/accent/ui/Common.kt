package io.github.stroblme.accent.ui

import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.awaitEachGesture
import androidx.compose.foundation.gestures.awaitFirstDown
import androidx.compose.foundation.gestures.calculateZoom
import androidx.compose.material3.ListItemDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.input.pointer.PointerEventPass
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.input.pointer.positionChange
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp

/** A row the whole width of the screen is tappable. */
fun Modifier.row(onClick: () -> Unit): Modifier = clickable(onClick = onClick)

/** List rows draw on the page, not on a card of their own. See MOBILE_DESIGN.md. */
@Composable
fun flatRow() = ListItemDefaults.colors(containerColor = MaterialTheme.colorScheme.surface)

/** The gutter every screen keeps at its sides. */
val Gutter: Dp = 16.dp

/**
 * A two-finger pinch, and nothing else.
 *
 * It watches the initial pass, so it sees the event before the list under it does, but consumes
 * only while two fingers are down — one finger still scrolls the pages and still draws.
 */
fun Modifier.pinch(onZoom: (Float) -> Unit): Modifier = pointerInput(Unit) {
    awaitEachGesture {
        awaitFirstDown(requireUnconsumed = false, pass = PointerEventPass.Initial)
        do {
            val event = awaitPointerEvent(PointerEventPass.Initial)
            if (event.changes.count { it.pressed } >= 2) {
                val zoom = event.calculateZoom()
                if (zoom != 1f) {
                    onZoom(zoom)
                    event.changes.forEach { it.consume() }
                }
            }
        } while (event.changes.any { it.pressed })
    }
}

/**
 * A downward drag that starts at the top of the content, which is what opens the switcher.
 *
 * Not a pull-to-refresh: that gesture means "fetch again" everywhere else, and here the answer
 * to a pull is a list of notes. [atTop] says whether the content under the finger has anywhere
 * left to scroll; when it has not, a drag past [threshold] pixels calls [onPull].
 */
fun Modifier.pullDown(atTop: () -> Boolean, threshold: Float = 180f, onPull: () -> Unit): Modifier =
    pointerInput(Unit) {
        awaitEachGesture {
            awaitFirstDown(requireUnconsumed = false)
            if (!atTop()) return@awaitEachGesture
            var travelled = 0f
            var fired = false
            do {
                val event = awaitPointerEvent()
                val change = event.changes.firstOrNull() ?: break
                travelled += change.positionChange().y
                if (!fired && travelled > threshold && atTop()) {
                    fired = true
                    onPull()
                }
            } while (event.changes.any { it.pressed })
        }
    }
