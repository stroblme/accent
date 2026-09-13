package io.github.stroblme.accent.ui

import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.awaitEachGesture
import androidx.compose.foundation.gestures.awaitFirstDown
import androidx.compose.foundation.gestures.calculateCentroid
import androidx.compose.foundation.gestures.calculatePan
import androidx.compose.foundation.gestures.calculateZoom
import androidx.compose.foundation.layout.*
import androidx.compose.material3.*
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.luminance
import androidx.compose.ui.graphics.toArgb
import androidx.compose.ui.input.pointer.PointerEventPass
import androidx.compose.ui.input.pointer.PointerInputScope
import androidx.compose.ui.input.pointer.pointerInput
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

/** A colour as `0xRRGGBB`, which is how both the core and a stylesheet want one. */
fun Color.rgb(): UInt = (0xFFFFFF and toArgb()).toUInt()

/** Whether this is a colour to read light text off. */
fun Color.dark(): Boolean = luminance() < 0.5f

/**
 * Whether the chrome is up: the bar over a note, and the two buttons at the foot of the screen.
 *
 * The desktop fades its chrome while the reader types. A phone has almost none to fade, so the
 * rule becomes the one every reader on the platform uses — it goes when the content scrolls on and
 * comes back when it scrolls back or is tapped. What is counted is travel rather than the last
 * delta, so a finger resting on a page does not flicker it.
 */
class Chrome {
    var shown by mutableStateOf(true)
        private set

    private var travel = 0f

    /** [dy] is how far the content moved up the screen: positive while reading on. */
    fun scrolled(dy: Float) {
        travel = (travel + dy).coerceIn(-SLOP, SLOP)
        if (travel >= SLOP) shown = false
        if (travel <= -SLOP) shown = true
    }

    fun tapped() {
        shown = !shown
        travel = 0f
    }

    /** Something else decided: another note opened. */
    fun show() {
        shown = true
        travel = 0f
    }

    private companion object {
        /** How far the content has to move before the chrome follows it. */
        const val SLOP = 48f
    }
}

/**
 * A tap that puts the chrome up or takes it down, seen before anything else gets it.
 *
 * On the initial pass, because what is underneath may be a view rather than a composable: a
 * `WebView` consumes what its own scrolling used, and a consumed change is one no gesture on the
 * main pass can read. Nothing is consumed here either, so the tap still reaches whatever it was
 * aimed at.
 */
fun Modifier.onTap(chrome: Chrome): Modifier = pointerInput(chrome) {
    awaitEachGesture {
        val down = awaitFirstDown(requireUnconsumed = false, pass = PointerEventPass.Initial)
        var tap = true
        do {
            val event = awaitPointerEvent(PointerEventPass.Initial)
            // A second finger is a pinch, and anything past the slop is a scroll.
            if (event.changes.size > 1) tap = false
            val moved = event.changes.firstOrNull { it.id == down.id }?.let {
                (it.position - down.position).getDistance() > viewConfiguration.touchSlop
            }
            if (moved == true) tap = false
        } while (event.changes.any { it.pressed })
        if (tap) chrome.tapped()
    }
}

/** A floating button: what a phone has where the desktop has a header bar. */
@Composable
fun Pill(label: String, onClick: () -> Unit) {
    Surface(
        shape = MaterialTheme.shapes.extraLarge,
        color = MaterialTheme.colorScheme.surface,
        shadowElevation = 6.dp,
    ) {
        Text(
            label,
            style = MaterialTheme.typography.labelLarge,
            modifier = Modifier.clickable(onClick = onClick).padding(horizontal = 24.dp, vertical = 14.dp),
        )
    }
}

/** A screen's name, and the way out of it that Back also is. */
@Composable
fun ScreenBar(title: String, onClose: () -> Unit) {
    Row(
        Modifier.fillMaxWidth().padding(start = Gutter, end = 4.dp, top = 8.dp, bottom = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(title, style = MaterialTheme.typography.headlineSmall, modifier = Modifier.weight(1f))
        TextButton(onClick = onClose) { Text("Close") }
    }
}

/**
 * The query field, at the foot of the screen it belongs to.
 *
 * Below its own answers rather than above them: that is where the thumb is, and where the keyboard
 * leaves it when it comes up.
 */
@Composable
fun Field(
    value: String,
    onValue: (String) -> Unit,
    placeholder: String,
    modifier: Modifier = Modifier,
) {
    OutlinedTextField(
        value = value,
        onValueChange = onValue,
        placeholder = { Text(placeholder) },
        singleLine = true,
        modifier = modifier.fillMaxWidth().padding(Gutter),
    )
}

/**
 * Panning and pinching as one gesture, with the throw that follows it.
 *
 * One handler for both axes, because two — one per direction — is what makes a diagonal drag
 * pick a side and stick to it. [onGesture] is called with where the fingers are between them,
 * how far they moved and how much further apart they got, all at once; [onFling] with the
 * velocity they left behind.
 *
 * [onEnd] is called when the fingers come up, with the velocity they left behind: a drag ends in a
 * throw and a pinch in the one layout the gesture is worth, and only the caller knows which it
 * was.
 *
 * A gesture whose events something nearer the finger has already taken — the pen drawing on the
 * page — is dropped rather than fought over.
 */
suspend fun PointerInputScope.panZoom(
    onGesture: (centroid: Offset, pan: Offset, zoom: Float) -> Unit,
    onEnd: (Velocity) -> Unit,
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
        if (moving) onEnd(speed.calculateVelocity())
    }
}
