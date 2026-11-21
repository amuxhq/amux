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
import androidx.compose.runtime.ReadOnlyComposable
import androidx.compose.runtime.compositionLocalOf
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Shape
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp

/**
 * How far up the surface the current content sits on is: the screen's
 * ground, a raised sheet or dialog, or a sunken card inside one. Cards and
 * text fields read it so they step one surface away from what is behind them
 * instead of each screen picking a fill. It is a level rather than a colour
 * because in light mode the ground and a raised sheet are the same white.
 */
enum class Backdrop { Ground, Raised, Sunken }

val LocalAmuxBackdrop = compositionLocalOf { Backdrop.Ground }

/** The fill of the surface the current content sits on. */
val AmuxTheme.backdrop: Color
    @Composable
    @ReadOnlyComposable
    get() = when (LocalAmuxBackdrop.current) {
        Backdrop.Ground -> colors.ground
        Backdrop.Raised -> colors.raised
        Backdrop.Sunken -> colors.sunken
    }

/**
 * The fill of a card on the current backdrop: [AmuxColors.raised] on the
 * ground, and [AmuxColors.sunken] on anything already raised, so a card in a
 * sheet does not vanish into the sheet in dark mode.
 */
val AmuxTheme.cardFill: Color
    @Composable
    @ReadOnlyComposable
    get() = if (LocalAmuxBackdrop.current == Backdrop.Ground) colors.raised else colors.sunken

/** Makes [level] the backdrop of [content]. */
@Composable
fun ProvideAmuxBackdrop(level: Backdrop, content: @Composable () -> Unit) {
    CompositionLocalProvider(LocalAmuxBackdrop provides level, content = content)
}

/** The backdrop level a container filled with [color] gives its content. */
@Composable
@ReadOnlyComposable
internal fun backdropOf(color: Color): Backdrop =
    if (color == AmuxTheme.colors.sunken) Backdrop.Sunken else Backdrop.Raised

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

    CompositionLocalProvider(
        LocalContentColor provides colors.ink,
        LocalAmuxBackdrop provides Backdrop.Ground,
    ) {
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
    color: Color = AmuxTheme.cardFill,
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
        ) {
            ProvideAmuxBackdrop(backdropOf(color), content)
        }
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
 * rows): a solid fill one surface away from its backdrop ([AmuxTheme.cardFill])
 * and zero translucent blur. On the ground it carries a `1.dp`
 * [AmuxColors.hairline] border; on a raised sheet the sunken fill alone sets
 * it apart, as iOS draws it.
 */
@Composable
fun AmuxPanel(
    modifier: Modifier = Modifier,
    shape: Shape = RoundedCornerShape(AmuxTheme.metrics.cardRadius),
    color: Color = AmuxTheme.cardFill,
    borderColor: Color = if (color == AmuxTheme.colors.sunken) Color.Transparent else AmuxTheme.colors.hairline,
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
        border = if (borderColor.alpha > 0f) BorderStroke(borderWidth, borderColor) else null,
    ) {
        ProvideAmuxBackdrop(backdropOf(color), content)
    }
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
    ) {
        ProvideAmuxBackdrop(backdropOf(color), content)
    }
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
