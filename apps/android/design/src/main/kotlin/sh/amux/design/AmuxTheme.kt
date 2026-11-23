package sh.amux.design

import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Shapes
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.ReadOnlyComposable
import androidx.compose.runtime.remember
import androidx.compose.runtime.staticCompositionLocalOf

val LocalAmuxDesign = staticCompositionLocalOf { AmuxDesign.App }
val LocalAmuxColors = staticCompositionLocalOf { AmuxDesign.App.colors(Appearance.Light) }
val LocalAmuxTypography = staticCompositionLocalOf { AmuxDesign.App.typography }
val LocalAmuxAppearance = staticCompositionLocalOf { Appearance.Light }

/**
 * Accessors for the active amux design tokens inside [AmuxTheme].
 */
object AmuxTheme {
    val design: AmuxDesign
        @Composable
        @ReadOnlyComposable
        get() = LocalAmuxDesign.current

    val colors: AmuxColors
        @Composable
        @ReadOnlyComposable
        get() = LocalAmuxColors.current

    val typography: AmuxTypography
        @Composable
        @ReadOnlyComposable
        get() = LocalAmuxTypography.current

    val appearance: Appearance
        @Composable
        @ReadOnlyComposable
        get() = LocalAmuxAppearance.current

    val metrics: AmuxDesign.Metrics
        @Composable
        @ReadOnlyComposable
        get() = LocalAmuxDesign.current.metrics
}

/**
 * Root theme for amux on Android.
 *
 * Maps amux's fixed neutral light/dark palette and bundled typography onto
 * Material 3 and exposes the complete [AmuxDesign] object via [LocalAmuxDesign].
 * Material You Dynamic Color is intentionally never read so neutral surfaces
 * remain strictly achromatic and colour is reserved for attention.
 */
@Composable
fun AmuxTheme(
    appearanceMode: AppearanceMode = AppearanceMode.System,
    design: AmuxDesign = AmuxDesign.App,
    reducedMotion: Boolean? = null,
    photographed: Boolean = false,
    content: @Composable () -> Unit,
) {
    val resolved = appearanceMode.resolve(systemDark = isSystemInDarkTheme())
    AmuxTheme(
        appearance = resolved,
        design = design,
        reducedMotion = reducedMotion,
        photographed = photographed,
        content = content,
    )
}

/**
 * Root theme pinned to an explicit [Appearance] (used by previews, storybook,
 * and tests).
 */
@Composable
fun AmuxTheme(
    appearance: Appearance,
    design: AmuxDesign = AmuxDesign.App,
    reducedMotion: Boolean? = null,
    photographed: Boolean = false,
    content: @Composable () -> Unit,
) {
    val colors = remember(design, appearance) { design.colors(appearance) }
    val colorScheme = remember(design, appearance) { design.colorScheme(appearance) }
    val typography = remember(design) { design.typography }
    val materialTypography = remember(typography) { typography.toMaterialTypography() }
    val shapes = remember(design) {
        Shapes(
            extraSmall = RoundedCornerShape(Space.xs),
            small = RoundedCornerShape(Space.s),
            medium = RoundedCornerShape(design.metrics.controlRadius),
            large = RoundedCornerShape(design.metrics.cardRadius),
            extraLarge = RoundedCornerShape(design.metrics.floatRadius),
        )
    }

    CompositionLocalProvider(
        LocalAmuxDesign provides design,
        LocalAmuxColors provides colors,
        LocalAmuxTypography provides typography,
        LocalAmuxAppearance provides appearance,
        LocalAmuxReducedMotion provides reducedMotion,
        LocalAmuxPhotographed provides photographed,
    ) {
        MaterialTheme(
            colorScheme = colorScheme,
            typography = materialTypography,
            shapes = shapes,
            content = content,
        )
    }
}

/**
 * Convenience overload accepting a boolean [darkTheme] flag.
 */
@Composable
fun AmuxTheme(
    darkTheme: Boolean,
    design: AmuxDesign = AmuxDesign.App,
    reducedMotion: Boolean? = null,
    photographed: Boolean = false,
    content: @Composable () -> Unit,
) {
    AmuxTheme(
        appearance = if (darkTheme) Appearance.Dark else Appearance.Light,
        design = design,
        reducedMotion = reducedMotion,
        photographed = photographed,
        content = content,
    )
}
