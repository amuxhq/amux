package sh.amux.design

import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxScope
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.LocalContentColor
import androidx.compose.material3.Surface
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Shape
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp

/**
 * The app's neutral backdrop. Draws a flat [AmuxColors.ground] fill (or a
 * subtle vertical gradient to [AmuxColors.sunken] if the skin enables it) and
 * establishes [AmuxColors.ink] as the default content colour.
 */
@Composable
fun AmuxGround(
    modifier: Modifier = Modifier,
    content: @Composable BoxScope.() -> Unit,
) {
    val design = AmuxTheme.design
    val colors = AmuxTheme.colors
    val backgroundModifier = if (design.surfaces.graduated) {
        Modifier.background(Brush.verticalGradient(listOf(colors.ground, colors.sunken)))
    } else {
        Modifier.background(colors.ground)
    }

    CompositionLocalProvider(LocalContentColor provides colors.ink) {
        Box(
            modifier = modifier
                .fillMaxSize()
                .then(backgroundModifier),
            content = content,
        )
    }
}

/**
 * Policy-driven surface container. Under [AmuxDesign.Separation.Card],
 * [AmuxDesign.Prominence.Subject], or [always] = true, draws a raised tonal
 * neutral panel with a crisp `1.dp` hairline border; otherwise keeps the
 * content flush on the ground.
 */
@Composable
fun AmuxSurface(
    modifier: Modifier = Modifier,
    shape: Shape = RoundedCornerShape(AmuxTheme.metrics.cardRadius),
    prominence: AmuxDesign.Prominence = AmuxDesign.Prominence.Plain,
    always: Boolean = false,
    color: Color = AmuxTheme.colors.raised,
    borderColor: Color = AmuxTheme.colors.hairline,
    borderWidth: Dp = AmuxTheme.metrics.hairline,
    content: @Composable () -> Unit,
) {
    val design = AmuxTheme.design
    val colors = AmuxTheme.colors
    val framed = always ||
        prominence == AmuxDesign.Prominence.Subject ||
        design.surfaces.separation == AmuxDesign.Separation.Card

    if (framed) {
        Surface(
            modifier = modifier,
            shape = shape,
            color = color,
            contentColor = colors.ink,
            tonalElevation = 0.dp,
            shadowElevation = 0.dp,
            border = BorderStroke(borderWidth, borderColor),
            content = content,
        )
    } else {
        CompositionLocalProvider(LocalContentColor provides colors.ink) {
            Box(modifier = modifier) {
                content()
            }
        }
    }
}

/**
 * Crisp neutral panel / card for readable content (asks, banners, grouped
 * rows): solid [AmuxColors.raised] fill + `1.dp` [AmuxColors.hairline] border
 * and zero translucent blur.
 */
@Composable
fun AmuxPanel(
    modifier: Modifier = Modifier,
    shape: Shape = RoundedCornerShape(AmuxTheme.metrics.cardRadius),
    color: Color = AmuxTheme.colors.raised,
    borderColor: Color = AmuxTheme.colors.hairline,
    borderWidth: Dp = AmuxTheme.metrics.hairline,
    content: @Composable () -> Unit,
) {
    val colors = AmuxTheme.colors
    Surface(
        modifier = modifier,
        shape = shape,
        color = color,
        contentColor = colors.ink,
        tonalElevation = 0.dp,
        shadowElevation = 0.dp,
        border = BorderStroke(borderWidth, borderColor),
        content = content,
    )
}

/**
 * Floating chrome helper (composer dock, pills, floating action bars, chips):
 * solid [AmuxColors.raised] fill + `1.dp` [AmuxColors.hairline] border +
 * subtle shadow elevation, replacing iOS translucent glass with an Android-native
 * tonal neutral surface.
 */
@Composable
fun AmuxChrome(
    modifier: Modifier = Modifier,
    shape: Shape = RoundedCornerShape(AmuxTheme.metrics.floatRadius),
    color: Color = AmuxTheme.colors.raised,
    borderColor: Color = AmuxTheme.colors.hairline,
    borderWidth: Dp = AmuxTheme.metrics.hairline,
    elevation: Dp = AmuxTheme.metrics.floatElevation,
    content: @Composable () -> Unit,
) {
    val colors = AmuxTheme.colors
    Surface(
        modifier = modifier,
        shape = shape,
        color = color,
        contentColor = colors.ink,
        tonalElevation = 0.dp,
        shadowElevation = elevation,
        border = BorderStroke(borderWidth, borderColor),
        content = content,
    )
}

/**
 * Crisp `1.dp` horizontal hairline separator drawn in [AmuxColors.hairline].
 */
@Composable
fun AmuxHairline(
    modifier: Modifier = Modifier,
    color: Color = AmuxTheme.colors.hairline,
) {
    HorizontalDivider(
        modifier = modifier,
        thickness = AmuxTheme.metrics.hairline,
        color = color,
    )
}

/**
 * Uniform dimming scrim behind sheets and dialogs.
 */
@Composable
fun AmuxScrim(
    modifier: Modifier = Modifier,
    alpha: Float = AmuxTheme.metrics.scrimAlpha,
) {
    Box(
        modifier = modifier
            .fillMaxSize()
            .background(Color.Black.copy(alpha = alpha)),
    )
}
