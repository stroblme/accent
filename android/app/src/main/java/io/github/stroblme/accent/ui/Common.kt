package io.github.stroblme.accent.ui

import android.content.Context
import android.content.Intent
import android.net.Uri
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

/** Shared top and bottom inset for note and PDF content. */
val DocumentGap: Dp = 8.dp

/**
 * Transition timing for arrival, exit, and sideways navigation, respecting system motion settings.
 */
fun <T> arriving(): FiniteAnimationSpec<T> = tween(200, easing = LinearOutSlowInEasing)

fun <T> leaving(): FiniteAnimationSpec<T> = tween(150, easing = FastOutLinearInEasing)

fun <T> stepping(): FiniteAnimationSpec<T> = tween(150, easing = FastOutSlowInEasing)

/** A colour as `0xRRGGBB`, which is how both the core and a stylesheet want one. */
fun Color.rgb(): UInt = (0xFFFFFF and toArgb()).toUInt()

/** Whether this is a colour to read light text off. */
fun Color.dark(): Boolean = luminance() < 0.5f

/** Visibility of the document bar and Browse button. */
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

/** Toggle chrome on an unclaimed tap without consuming the tap from the underlying view. */
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

/** Floating action styled with inverted page contrast. */
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

/** Shared document bar with title and trailing actions for notes and PDFs. */
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

/** Toggle action whose accent fill indicates its active state. */
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

/** Chrome overlays the document, preserving its position when shown or hidden. */
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

/** Overlay [bar] without moving the document; pass its height to [content] for clearance. */
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
 * Mark only the panel surface as a pointer target while allowing child controls and outside taps.
 */
fun Modifier.stopsHere(): Modifier = pointerInput(Unit) {
    awaitEachGesture { awaitFirstDown(requireUnconsumed = false) }
}

/** Close a panel after a pull beyond [PullToClose]; let its content scroll first. */
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

/** Place the query field below results, near the thumb and above the keyboard. */
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

/** Find bar for the open document, above the keyboard and outside its scroll viewport. */
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
 * Report combined pan and zoom through [onGesture], stopping flings on [onDown] and
 * reporting release velocity through [onEnd].
 */
suspend fun PointerInputScope.panZoom(
    onDown: () -> Unit,
    onGesture: (centroid: Offset, pan: Offset, zoom: Float) -> Unit,
    onEnd: (Velocity) -> Unit,
) {
    awaitEachGesture {
        val speed = VelocityTracker()
        var moving = false
        awaitFirstDown(requireUnconsumed = false)
        onDown()
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

/** Hand [uri] to whichever app opens it, saying through [onSay] when none does. */
internal fun launchUri(context: Context, uri: String, onSay: (String) -> Unit) {
    runCatching { context.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(uri))) }
        .onFailure { onSay("No app can open this link") }
}
