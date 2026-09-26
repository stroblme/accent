package io.github.stroblme.accent.ui

import android.os.Build
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.ColorScheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Shapes
import androidx.compose.material3.Typography
import androidx.compose.material3.dynamicDarkColorScheme
import androidx.compose.material3.dynamicLightColorScheme
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.compositeOver
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import io.github.stroblme.accent.ffi.Theme

/**
 * One accent, taken from the system; the page and the ink on it, taken from the desktop.
 *
 * Material You answers the same question GNOME's accent does — what colour is this device — so the
 * app asks it for `primary` and the roles that exist to carry it, and for nothing else. What a note
 * is read off is the desktop's view instead, white paper or libadwaita's dark shade with
 * libadwaita's text on it, so the two apps are recognisably one editor. Material's tonal elevation
 * goes the same way: every container tone is flattened onto that page, because a note wants paper
 * to sit on, not a stack of cards. See MOBILE_DESIGN.md.
 */
@Composable
fun AccentTheme(dark: Boolean = isSystemInDarkTheme(), content: @Composable () -> Unit) {
    val context = LocalContext.current
    val scheme = when {
        Build.VERSION.SDK_INT >= Build.VERSION_CODES.S ->
            if (dark) dynamicDarkColorScheme(context) else dynamicLightColorScheme(context)
        dark -> darkColorScheme()
        else -> lightColorScheme()
    }
    MaterialTheme(
        colorScheme = scheme.flattened(dark),
        shapes = AccentShapes,
        typography = AccentTypography,
        content = content,
    )
}

/**
 * One radius scale, and nothing rounds itself.
 *
 * Material's own starts at 4 dp, which on a flat page reads as a rectangle somebody failed to
 * round. Everything here is softer and steps by the same 4 / 8 the spacing does, so a field, a
 * chip and a floating button are recognisably the same family. `extraLarge` is past half the
 * height of anything it is used on, which is what makes a pill a pill.
 */
private val AccentShapes = Shapes(
    extraSmall = RoundedCornerShape(8.dp),
    small = RoundedCornerShape(12.dp),
    medium = RoundedCornerShape(16.dp),
    large = RoundedCornerShape(20.dp),
    extraLarge = RoundedCornerShape(28.dp),
)

/**
 * The desktop's `--view-bg-color` and the text libadwaita puts on it, both halves of both pairs.
 *
 * Copied from `apps/gtk/src/theme.rs` (`VIEW_LIGHT`/`VIEW_DARK` and the `*_TEXT` pair beside them)
 * rather than re-derived, so the two apps cannot drift apart. [InkLight] is pre-composited there
 * for the same reason a rendered page needs it opaque: libadwaita's light `--view-fg-color` is
 * `RGB(0 0 6 / 80%)`, and over white that is `#333338`.
 */
private val PageLight = Color(0xFFFFFFFF)
private val PageDark = Color(0xFF1D1D20)
private val InkLight = Color(0xFF333338)
private val InkDark = Color(0xFFEBEBEB)

/**
 * The accent kept, everything under it replaced: one page, one ink, no cards, no elevation.
 *
 * Material You derives every role from the wallpaper, the page and the text on it included, and
 * beside the desktop's view the result reads cream. So the page is written onto `surface`, onto
 * every container tone — which is what flattens the elevation — and onto `background`, and the ink
 * onto `onSurface` and `onBackground`. `onSurfaceVariant`, `outline` and `outlineVariant` have no
 * desktop counterpart and are that same ink thinned over that same page: 55% for secondary text,
 * then 40% for a border and 15% for a hairline. The 55% is libadwaita's own `.dim-label`, taken for
 * parity rather than for contrast and at a known cost — `#8f8f92` on white is 3.2:1, short of WCAG
 * AA's 4.5 for body text, where 70% would have cleared it. The desktop's number wins because every
 * other colour here is already the desktop's literal value; the dark page's `#8e8e8f` is 5.1:1 and
 * clears it anyway. `scrim` is black in both modes, a dimmed screen being an absence of light
 * rather than a colour of its own.
 *
 * `inverseSurface` and `inverseOnSurface` are the *other* mode's pair, which is what keeps the
 * Browse pill (and the snackbar, which reads the same two roles) dark on a light theme and light on
 * a dark one — see [Pill].
 *
 * Left to the system by decision, not by oversight, because it is the accent and the app has no
 * answer of its own: `primary` with its container and `on-` roles, `inversePrimary`, the secondary
 * and tertiary families, and `error`. `surfaceTint` stays too and is never spent — it only shows
 * through tonal elevation, and nothing here asks for any.
 */
internal fun ColorScheme.flattened(dark: Boolean): ColorScheme {
    val page = if (dark) PageDark else PageLight
    val ink = if (dark) InkDark else InkLight
    return copy(
        surface = page,
        surfaceContainerLowest = page,
        surfaceContainerLow = page,
        surfaceContainer = page,
        surfaceContainerHigh = page,
        surfaceContainerHighest = page,
        surfaceVariant = page,
        surfaceBright = page,
        surfaceDim = page,
        background = page,
        onSurface = ink,
        onBackground = ink,
        onSurfaceVariant = ink.copy(alpha = 0.55f).compositeOver(page),
        outline = ink.copy(alpha = 0.4f).compositeOver(page),
        outlineVariant = ink.copy(alpha = 0.15f).compositeOver(page),
        scrim = Color.Black,
        inverseSurface = if (dark) PageLight else PageDark,
        inverseOnSurface = if (dark) InkLight else InkDark,
    )
}

/**
 * What a document is recoloured onto: a PDF page, and an image that reads as one ([imageTheme]).
 *
 * In a dark theme its paper lands on the dark page and its ink on the dark ink, each pixel keeping
 * its own chroma, as the desktop does it; a light theme leaves it alone. [inverted] is the reader's
 * hand on one file, the desktop's Invert: a light theme then recolours it onto the dark page, and a
 * dark one shows it as it is. Always the dark pair, because the light one is the paper a document
 * already has.
 */
internal fun pageTheme(dark: Boolean, inverted: Boolean = false): Theme =
    if (dark != inverted) Theme.Recolour(PageDark.rgb(), InkDark.rgb()) else Theme.Plain

/**
 * The files the reader has inverted by hand ([pageTheme]'s `inverted`) — a PDF or an image, by
 * Invert in its bar or a long press on the image in a note — by path on this device, or by address
 * for a PDF from another app. Kept for as long as the app's process runs and nowhere else: a page
 * or a figure that came out wrong is put right for this reading, not written into the vault.
 */
object Inverted {
    var files by mutableStateOf(emptySet<String>())
        private set

    fun toggle(key: String) {
        files = if (key in files) files - key else files + key
    }
}

/**
 * How strongly what the reader does to a PDF page is painted over it, in the accent: the highlight
 * a note's link makes, the text selection, a find's matches and the one stepped to. The desktop's
 * own numbers (`apps/gtk/src/theme.rs`), so a page reads the same on both.
 */
internal const val HIGHLIGHT_ALPHA = 0.2f
internal const val SELECTION_ALPHA = 0.35f
internal const val MARK_ALPHA = 0.3f
internal const val CURRENT_MARK_ALPHA = 0.6f

/**
 * The sides of a conflict block git left in a note, as the desktop's editor and preview tint them
 * (`diff::tint`, `conflict::tints`): VS Code's green for the current side and blue for the incoming
 * one, each 65 % hue to 35 % ink, so it darkens on the light page and lightens on the dark one, over
 * the page at 16 % and under a caption at 35 %; a diff3 base is the ink alone at half of each. The
 * hues are the desktop's weights, kept as floats because a [Color] would round them to a byte.
 */
private val CurrentHue = floatArrayOf(0.15f, 0.70f, 0.35f)
private val IncomingHue = floatArrayOf(0.20f, 0.50f, 0.90f)
private const val HUE_MIX = 0.65f
private const val SIDE_ALPHA = 0.16f
private const val CAPTION_ALPHA = 0.35f

/** Each side's tint and its caption's — current, base, incoming — over a page inked in [ink]. */
internal fun conflictTints(ink: Color): List<Pair<Color, Color>> {
    fun tint(hue: FloatArray, alpha: Float) = Color(
        hue[0] * HUE_MIX + ink.red * (1 - HUE_MIX),
        hue[1] * HUE_MIX + ink.green * (1 - HUE_MIX),
        hue[2] * HUE_MIX + ink.blue * (1 - HUE_MIX),
        alpha,
    )
    fun side(hue: FloatArray) = tint(hue, SIDE_ALPHA) to tint(hue, CAPTION_ALPHA)
    val base = ink.copy(alpha = SIDE_ALPHA / 2) to ink.copy(alpha = CAPTION_ALPHA / 2)
    return listOf(side(CurrentHue), base, side(IncomingHue))
}

/**
 * Hierarchy by size and weight, not by colour or rule. The body is the system's own size, which
 * is what the reader set; the rest is measured against it.
 */
private val AccentTypography = Typography().let { base ->
    base.copy(
        headlineSmall = base.headlineSmall.copy(fontWeight = FontWeight.SemiBold, fontSize = 24.sp),
        titleMedium = base.titleMedium.copy(fontWeight = FontWeight.Medium),
        bodyLarge = base.bodyLarge.copy(fontSize = 16.sp, lineHeight = 26.sp),
        labelMedium = base.labelMedium.copy(fontWeight = FontWeight.Normal),
    )
}
