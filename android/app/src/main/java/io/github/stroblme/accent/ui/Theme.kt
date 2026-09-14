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
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

/**
 * One accent, taken from the system, and one flat surface under everything.
 *
 * Material You answers the same question GNOME's accent does — what colour is this device — so
 * the app asks it and writes no palette of its own beyond the page it lays everything on. What it
 * does override is Material's tonal elevation: every container tone is flattened onto that page,
 * because a note wants paper to sit on, not a stack of cards. See MOBILE_DESIGN.md.
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
 * Every container tone collapsed onto the surface: one background, no cards, no elevation.
 *
 * In light mode that one surface is paper white, which is the desktop's `--view-bg-color` and the
 * colour a page of text has always been. Material You tints its own surface towards the wallpaper
 * and the result reads cream beside the desktop, so this is the one role the app takes off the
 * system — the accent, the text and the inverted pill still come from it. Dark mode keeps the
 * shade it was given: the argument for white is paper, and it does not run the other way.
 */
internal fun ColorScheme.flattened(dark: Boolean): ColorScheme {
    val page = if (dark) surface else Color.White
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
    )
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
