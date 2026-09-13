package io.github.stroblme.accent.ui

import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.awaitEachGesture
import androidx.compose.foundation.gestures.awaitFirstDown
import androidx.compose.foundation.gestures.calculateCentroid
import androidx.compose.foundation.gestures.calculatePan
import androidx.compose.foundation.gestures.calculateZoom
import androidx.compose.material3.ListItemDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.input.pointer.PointerInputScope
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.input.pointer.positionChange
import androidx.compose.ui.input.pointer.positionChanged
import androidx.compose.ui.input.pointer.util.VelocityTracker
import androidx.compose.ui.unit.Velocity
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
 * Panning and pinching as one gesture, with the throw that follows it.
 *
 * One handler for both axes, because two — one per direction — is what makes a diagonal drag
 * pick a side and stick to it. [onGesture] is called with where the fingers are between them,
 * how far they moved and how much further apart they got, all at once; [onFling] with the
 * velocity they left behind.
 *
 * A gesture whose events something nearer the finger has already taken — the pen drawing on the
 * page — is dropped rather than fought over.
 */
suspend fun PointerInputScope.panZoom(
    onGesture: (centroid: Offset, pan: Offset, zoom: Float) -> Unit,
    onFling: (Velocity) -> Unit,
) {
    awaitEachGesture {
        val speed = VelocityTracker()
        var moving = false
        awaitFirstDown(requireUnconsumed = false)
        do {
            val event = awaitPointerEvent()
            if (event.changes.any { it.isConsumed }) return@awaitEachGesture
            val zoom = event.calculateZoom()
            val pan = event.calculatePan()
            if (!moving && (zoom != 1f || pan.getDistance() > viewConfiguration.touchSlop)) {
                moving = true
            }
            if (moving) {
                val centroid = event.calculateCentroid(useCurrent = true)
                if (centroid != Offset.Unspecified) onGesture(centroid, pan, zoom)
                event.changes.forEach { if (it.positionChanged()) it.consume() }
                event.changes.firstOrNull { it.pressed }
                    ?.let { speed.addPosition(it.uptimeMillis, it.position) }
            }
        } while (event.changes.any { it.pressed })
        if (moving) onFling(speed.calculateVelocity())
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
