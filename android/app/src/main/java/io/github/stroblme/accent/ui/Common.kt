package io.github.stroblme.accent.ui

import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.expandVertically
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.shrinkVertically
import androidx.compose.animation.core.animate
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.Orientation
import androidx.compose.foundation.gestures.draggable
import androidx.compose.foundation.gestures.rememberDraggableState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.gestures.awaitEachGesture
import androidx.compose.foundation.gestures.awaitFirstDown
import androidx.compose.foundation.gestures.calculateCentroid
import androidx.compose.foundation.gestures.calculatePan
import androidx.compose.foundation.gestures.calculateZoom
import androidx.compose.foundation.layout.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.luminance
import androidx.compose.ui.graphics.toArgb
import androidx.compose.ui.input.nestedscroll.NestedScrollConnection
import androidx.compose.ui.input.nestedscroll.NestedScrollSource
import androidx.compose.ui.input.nestedscroll.nestedScroll
import androidx.compose.ui.input.pointer.PointerEventPass
import androidx.compose.ui.input.pointer.PointerInputScope
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.input.pointer.positionChanged
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.input.pointer.util.VelocityTracker
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.unit.IntOffset
import androidx.compose.ui.unit.Velocity
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.launch
import kotlin.math.max
import kotlin.math.roundToInt

/** A row the whole width of the screen is tappable. */
fun Modifier.row(onClick: () -> Unit): Modifier = clickable(onClick = onClick)

/** List rows draw on the page, not on a card of their own. See MOBILE_DESIGN.md. */
@Composable
fun flatRow() = ListItemDefaults.colors(containerColor = MaterialTheme.colorScheme.surface)

/** The gutter every screen keeps at its sides. */
val Gutter: Dp = 16.dp

/**
 * The gap a document keeps from the bar above it and from the foot of the screen.
 *
 * The same on a note and on a PDF, which is the whole point of it: what is being read sits in the
 * same rectangle whichever of the two it is.
 */
val DocumentGap: Dp = 8.dp

/** A colour as `0xRRGGBB`, which is how both the core and a stylesheet want one. */
fun Color.rgb(): UInt = (0xFFFFFF and toArgb()).toUInt()

/** Whether this is a colour to read light text off. */
fun Color.dark(): Boolean = luminance() < 0.5f

/**
 * Whether the chrome is up: the bar over a note, and the Browse button at the foot of the screen.
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

/**
 * A floating button: what a phone has where the desktop has a header bar.
 *
 * Inverted against the page — dark on a light theme, light on a dark one — because it is the one
 * thing on the screen that is not the document, and a pale pill on a pale page is a pill nobody
 * sees. The one place the app spends contrast rather than colour.
 */
@Composable
fun Pill(label: String, onClick: () -> Unit) {
    Surface(
        shape = MaterialTheme.shapes.extraLarge,
        color = MaterialTheme.colorScheme.inverseSurface,
        contentColor = MaterialTheme.colorScheme.inverseOnSurface,
        shadowElevation = 6.dp,
    ) {
        Text(
            label,
            style = MaterialTheme.typography.labelLarge,
            modifier = Modifier.clickable(onClick = onClick).padding(horizontal = 32.dp, vertical = 18.dp),
        )
    }
}

/**
 * The bar over a document: what it is called, and the one thing that can be done to it.
 *
 * The same bar over a note and over a PDF, so that the viewport under it begins in the same place
 * in both. [enabled] is what a PDF has instead of a second bar of its own — the button is there,
 * and says so, until there is something for it to do.
 */
@Composable
fun DocumentBar(title: String, action: String, enabled: Boolean = true, onAction: () -> Unit) {
    Row(
        Modifier.fillMaxWidth().padding(start = Gutter, end = 4.dp, top = 4.dp, bottom = 4.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(
            title,
            style = MaterialTheme.typography.titleMedium,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
            modifier = Modifier.weight(1f),
        )
        TextButton(onClick = onAction, enabled = enabled) { Text(action) }
    }
}

/**
 * Chrome that goes and comes back without the content jumping.
 *
 * It takes its height with it rather than only its colour, so what is below slides up into the
 * space instead of being covered by nothing.
 */
@Composable
fun FadingBar(visible: Boolean, content: @Composable () -> Unit) {
    AnimatedVisibility(
        visible = visible,
        enter = fadeIn() + expandVertically(),
        exit = fadeOut() + shrinkVertically(),
    ) {
        content()
    }
}

/** A screen's name. The way out is Back, or a pull down; see [PullDownPanel]. */
@Composable
fun ScreenBar(title: String) {
    Text(
        title,
        style = MaterialTheme.typography.headlineSmall,
        modifier = Modifier.fillMaxWidth().padding(start = Gutter, end = Gutter, bottom = 8.dp),
    )
}

/** How far a panel has to be pulled before letting go closes it rather than putting it back. */
private val PullToClose: Dp = 96.dp

/**
 * A panel over the document that goes when it is pulled down.
 *
 * What is inside it scrolls first: only a drag the list cannot use — one with nothing left above
 * it — moves the panel, which is what makes this a pull rather than a gesture that fights the
 * content. Letting go past [PullToClose] closes it and anything less springs back. The handle at
 * the top says so, and can be dragged itself; Back still works, which is why there is no button.
 *
 * A drag that began by scrolling the list stops where the list does. Reaching the top of the files
 * is something a reader does on the way to the first of them, and it must not also be the thing
 * that takes the files away: closing is a second pull, from a standstill.
 */
@Composable
fun PullDownPanel(onClose: () -> Unit, content: @Composable ColumnScope.() -> Unit) {
    var pulled by remember { mutableFloatStateOf(0f) }
    val scope = rememberCoroutineScope()
    val close by rememberUpdatedState(onClose)
    val threshold = with(LocalDensity.current) { PullToClose.toPx() }

    fun release() {
        if (pulled >= threshold) close() else scope.launch { animate(pulled, 0f) { v, _ -> pulled = v } }
    }

    val nested = remember(threshold) {
        object : NestedScrollConnection {
            /** Whether the list has taken any of the drag in hand. Reset when the fingers lift. */
            private var scrolled = false

            /** Pulling back up puts the panel back before the list gets to move. */
            override fun onPreScroll(available: Offset, source: NestedScrollSource): Offset {
                if (available.y >= 0f || pulled <= 0f) return Offset.Zero
                val used = max(available.y, -pulled)
                pulled += used
                return Offset(0f, used)
            }

            /** What the list could not use, because it is already at its top. */
            override fun onPostScroll(
                consumed: Offset,
                available: Offset,
                source: NestedScrollSource,
            ): Offset {
                if (consumed.y != 0f) scrolled = true
                if (available.y <= 0f) return Offset.Zero
                // Not the tail of a scroll, and not a fling running on past the end.
                if (scrolled || source != NestedScrollSource.UserInput) return Offset.Zero
                pulled += available.y
                return Offset(0f, available.y)
            }

            /** The fingers are up: this drag is over, whatever it turned out to be. */
            override suspend fun onPreFling(available: Velocity): Velocity {
                scrolled = false
                if (pulled <= 0f) return Velocity.Zero
                release()
                return available
            }
        }
    }

    Column(
        Modifier
            .fillMaxSize()
            .offset { IntOffset(0, pulled.roundToInt()) }
            // A ground of its own: this lies over whatever is being read, which stays composed.
            .background(MaterialTheme.colorScheme.surface)
            .nestedScroll(nested),
    ) {
        Handle(
            Modifier.draggable(
                state = rememberDraggableState { pulled = (pulled + it).coerceAtLeast(0f) },
                orientation = Orientation.Vertical,
                onDragStopped = { release() },
            ),
        )
        content()
    }
}

/** The bar that says a panel can be pulled down. */
@Composable
private fun Handle(modifier: Modifier = Modifier) {
    Box(modifier.fillMaxWidth().padding(vertical = 12.dp), contentAlignment = Alignment.Center) {
        Box(
            Modifier
                .size(width = 32.dp, height = 4.dp)
                .background(MaterialTheme.colorScheme.onSurfaceVariant.copy(alpha = 0.4f), CircleShape),
        )
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
        shape = MaterialTheme.shapes.large,
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
