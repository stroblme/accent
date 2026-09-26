package io.github.stroblme.accent.ui

import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.expandVertically
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.shrinkVertically
import androidx.compose.animation.core.FastOutLinearInEasing
import androidx.compose.animation.core.FastOutSlowInEasing
import androidx.compose.animation.core.FiniteAnimationSpec
import androidx.compose.animation.core.LinearOutSlowInEasing
import androidx.compose.animation.core.animate
import androidx.compose.animation.core.tween
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
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
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
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.semantics.semantics
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
 * The gap a document keeps from the top of its frame and from the foot of the screen.
 *
 * The same on a note and on a PDF, which is the whole point of it: what is being read sits in the
 * same rectangle whichever of the two it is.
 */
val DocumentGap: Dp = 8.dp

/**
 * The motion a surface arrives with, leaves with, and steps sideways with.
 *
 * Two durations and three curves. 200 ms for something arriving and 150 ms for something leaving
 * or moving across — Material's short-4 and short-3, the fast end of its own scale, because this
 * is a reading app and a transition that has to be waited for is worse than no transition at all.
 * What arrives decelerates into place and what leaves accelerates away, which is Material's
 * asymmetry and its reason: arriving is watched, leaving is not. A step sideways does both, since
 * it is one surface travelling rather than a new one showing up.
 *
 * Nothing here asks about reduced motion and nothing needs to: Compose scales every animation by
 * the platform's animator duration scale, so a device with animations turned off gets all of this
 * at once.
 */
fun <T> arriving(): FiniteAnimationSpec<T> = tween(200, easing = LinearOutSlowInEasing)

fun <T> leaving(): FiniteAnimationSpec<T> = tween(150, easing = FastOutLinearInEasing)

fun <T> stepping(): FiniteAnimationSpec<T> = tween(150, easing = FastOutSlowInEasing)

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
 *
 * A tap and nothing else. A press held past the platform's long-press time belongs to whatever is
 * under the finger — a selection in the rendered note — so the wait for the fingers to come up is
 * given exactly that long and the chrome stays where it was.
 *
 * [claimed] is asked first, and is told where the tap landed: a surface with something of its own
 * at that point — a link on a PDF page — takes the tap by answering true, and the chrome does not
 * move. It is only ever asked about a gesture that turned out to be a tap, so it may act on what it
 * finds rather than answer and wait to be called again.
 *
 * [onTapped] is whatever else the surface wants the same tap to mean: a rendered note takes its
 * search highlight off with it. Both are captured once, along with the gesture, so what either
 * reads has to be state it can read again rather than a value it closed over.
 */
fun Modifier.onTap(
    chrome: Chrome,
    claimed: (Offset) -> Boolean = { false },
    onTapped: () -> Unit = {},
): Modifier = pointerInput(chrome) {
    awaitEachGesture {
        val down = awaitFirstDown(requireUnconsumed = false, pass = PointerEventPass.Initial)
        val tapped = withTimeoutOrNull(viewConfiguration.longPressTimeoutMillis) {
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
            tap
        }
        if (tapped == true) {
            // Asked before the chrome moves, which is the whole of why it is here: a link has to be
            // able to take a tap that would otherwise have been spent putting the bar up.
            if (claimed(down.position)) return@awaitEachGesture
            chrome.tapped()
            onTapped()
        }
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
 * The bar over a document: what it is called, and what can be done to it — text buttons, at its
 * end.
 *
 * The same bar over a note and over a PDF, so that the viewport under it begins in the same place
 * in both.
 */
@Composable
fun DocumentBar(title: String, actions: @Composable RowScope.() -> Unit) {
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
        actions()
    }
}

/**
 * A bar action that stays on until it is pressed again — Invert — drawn as the bar's other text
 * buttons are, and filled with the accent while it is on. Its label says what it does, not whether
 * it is doing it, so the fill is what says that.
 */
@Composable
fun BarToggle(label: String, on: Boolean, onClick: () -> Unit, enabled: Boolean = true) {
    val colors = MaterialTheme.colorScheme
    TextButton(
        onClick = onClick,
        enabled = enabled,
        colors = if (on) {
            ButtonDefaults.textButtonColors(containerColor = colors.primary, contentColor = colors.onPrimary)
        } else {
            ButtonDefaults.textButtonColors()
        },
        modifier = Modifier.semantics { selected = on },
    ) {
        Text(label)
    }
}

/**
 * Chrome that goes and comes back. It lies over the document ([DocumentFrame]), so nothing under
 * it moves when it does.
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

/**
 * A document, with its [bar] lying over the top of it rather than above it. A note and a PDF both.
 *
 * A document is read at a scroll offset, and a PDF at a zoom as well. A bar that takes its space
 * from the layout gives the document a different height every time it comes and goes, and a
 * document laid out again is text that moves — the one thing the reader who tapped for the bar did
 * not ask for. Here the content keeps the whole rectangle whether the bar is up or not, so a tap
 * changes only what is drawn on top.
 *
 * What that costs is the head of the document, which the bar covers while it is up and which
 * cannot be scrolled out from under it, the document being at its top already: a PDF's page
 * margin, but a note's first line. The way to it is the tap that raised the bar, since the same
 * tap takes it away. A strip of the bar's height reserved at the top of the document would buy
 * those lines with a permanent gap on a screen whose chrome is down most of the time.
 *
 * [content] is told how tall the bar is, for the one surface that has to keep clear of it: the
 * editor, whose bar never goes.
 */
@Composable
fun DocumentFrame(
    barShown: Boolean,
    bar: @Composable () -> Unit,
    modifier: Modifier = Modifier,
    content: @Composable BoxScope.(bar: Dp) -> Unit,
) {
    val density = LocalDensity.current
    // Kept when the bar goes: the height it had is the height it comes back with.
    var height by remember { mutableStateOf(0.dp) }
    Box(modifier.fillMaxSize()) {
        Box(Modifier.fillMaxSize().padding(vertical = DocumentGap)) { content(height) }
        FadingBar(visible = barShown) {
            // Opaque, and where a pointer stops: over the document the bar needs a ground of its
            // own to be read off, and a press it let through would be a press the document reads
            // at the point it landed — a link hidden behind the title would be followed by a tap
            // on the title.
            Box(
                Modifier
                    .background(MaterialTheme.colorScheme.surface)
                    .stopsHere()
                    .onSizeChanged { height = with(density) { it.height.toDp() } },
            ) {
                bar()
            }
        }
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
 * Where a pointer event stops.
 *
 * A panel lies over what is being read rather than replacing it, and a background is paint rather
 * than a target: with no gesture of its own the blank half of the panel is never hit at all and
 * the press reaches the document below it, so a tap beside a file row toggles the note's chrome.
 * Nothing is consumed, so the rows, the chips and the field still get their own.
 *
 * The other use is a bar lying over a document: there the point is the opposite one — what is
 * underneath must *not* get the press, or a tap on the title would be read as a tap on the page.
 */
fun Modifier.stopsHere(): Modifier = pointerInput(Unit) {
    awaitEachGesture { awaitFirstDown(requireUnconsumed = false) }
}

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
                // A drag something took sideways is a page turn, not a pull. What it hands on is
                // only what a thumb spills crossing the screen, and a panel that followed it would
                // be a panel that moves when the reader changes tab.
                if (consumed.x != 0f) return Offset.Zero
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
            .stopsHere()
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
 * A document's own find: the page in front, where Browse's Search is every note in the vault. A
 * note's and a PDF's alike.
 *
 * At the foot of the screen, which is where every query field in this app is and where the
 * keyboard leaves the thumb. It is the one piece of chrome that does not go while the keyboard is
 * up, because the keyboard is what it is for — the Browse pill goes instead, so the vault's search
 * and the page's find are never on the screen together. It takes its space from the document
 * rather than floating over it: a bar over the last lines would cover the match it had just found.
 *
 * Back is the way out, as it is out of the panel. The arrows are disabled rather than absent while
 * there is nothing to step through, and [count] — "3/12", or "None" — is the only thing that says a
 * word is not there at all: everything else about a find that matches nothing looks like a find
 * that has not scrolled yet.
 */
@Composable
fun FindBar(
    query: String,
    onQuery: (String) -> Unit,
    placeholder: String,
    count: String,
    canStep: Boolean,
    onStep: (forward: Boolean) -> Unit,
) {
    val focus = remember { FocusRequester() }
    LaunchedEffect(Unit) { focus.requestFocus() }
    Row(
        Modifier.fillMaxWidth().padding(end = 4.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Field(
            value = query,
            onValue = onQuery,
            placeholder = placeholder,
            modifier = Modifier.weight(1f).focusRequester(focus),
        )
        Text(
            count,
            style = MaterialTheme.typography.labelMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        TextButton(onClick = { onStep(false) }, enabled = canStep) { Text("▴") }
        TextButton(onClick = { onStep(true) }, enabled = canStep) { Text("▾") }
    }
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
