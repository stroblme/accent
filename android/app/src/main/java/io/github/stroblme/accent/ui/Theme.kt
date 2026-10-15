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

/** Use the system accent with desktop page and text colours; flatten container tones. */
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
 * Use one corner-radius scale for fields, chips and floating controls. `extraLarge` makes pills.
 */
private val AccentShapes = Shapes(
    extraSmall = RoundedCornerShape(8.dp),
    small = RoundedCornerShape(12.dp),
    medium = RoundedCornerShape(16.dp),
    large = RoundedCornerShape(20.dp),
    extraLarge = RoundedCornerShape(28.dp),
)

/** Desktop light and dark page and text colours copied from `apps/gtk/src/theme.rs`. */
internal val PageLight = Color(0xFFFFFFFF)
private val PageDark = Color(0xFF1D1D20)
internal val InkLight = Color(0xFF333338)
private val InkDark = Color(0xFFEBEBEB)

/**
 * Flatten Material surfaces to desktop page colours. Secondary text uses 55% ink to match
 * libadwaita's `.dim-label`; on white, its 3.2:1 contrast is below WCAG AA for body text.
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

/** Theme for PDF pages and document-like images ([imageTheme]). */
internal fun pageTheme(dark: Boolean, inverted: Boolean = false): Theme =
    if (dark != inverted) Theme.Recolour(PageDark.rgb(), InkDark.rgb()) else Theme.Plain

/** Paths manually inverted by the reader through the bar or an image long press. */
object Inverted {
    var files by mutableStateOf(emptySet<String>())
        private set

    fun toggle(key: String) {
        files = if (key in files) files - key else files + key
    }
}

/** Accent opacity for PDF highlights, selections, and find matches. */
internal const val HIGHLIGHT_ALPHA = 0.2f
internal const val SELECTION_ALPHA = 0.35f
internal const val MARK_ALPHA = 0.3f
internal const val CURRENT_MARK_ALPHA = 0.6f

/** Desktop green and blue conflict tints mixed with the current ink. */
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

/** Typography hierarchy by size and weight, based on the device body size. */
private val AccentTypography = Typography().let { base ->
    base.copy(
        headlineSmall = base.headlineSmall.copy(fontWeight = FontWeight.SemiBold, fontSize = 24.sp),
        titleMedium = base.titleMedium.copy(fontWeight = FontWeight.Medium),
        bodyLarge = base.bodyLarge.copy(fontSize = 16.sp, lineHeight = 26.sp),
        labelMedium = base.labelMedium.copy(fontWeight = FontWeight.Normal),
    )
}
