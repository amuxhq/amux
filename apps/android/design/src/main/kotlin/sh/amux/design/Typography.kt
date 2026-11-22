package sh.amux.design

import androidx.compose.material3.Typography
import androidx.compose.runtime.Immutable
import androidx.compose.ui.text.ExperimentalTextApi
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.Font
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontVariation
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.sp

/**
 * Bundled open-licensed variable typefaces: Instrument Sans for UI/prose and
 * Geist Mono for commands, paths, diffs, and technical identifiers.
 *
 * Each weight maps explicitly to the variable `wght` axis so Android renders
 * true variable weights instead of synthesizing faux bold.
 */
@OptIn(ExperimentalTextApi::class)
object AmuxFonts {
    val Sans: FontFamily = FontFamily(
        Font(
            resId = R.font.instrument_sans,
            weight = FontWeight.Normal,
            variationSettings = FontVariation.Settings(
                FontVariation.weight(FontWeight.Normal.weight),
            ),
        ),
        Font(
            resId = R.font.instrument_sans,
            weight = FontWeight.Medium,
            variationSettings = FontVariation.Settings(
                FontVariation.weight(FontWeight.Medium.weight),
            ),
        ),
        Font(
            resId = R.font.instrument_sans,
            weight = FontWeight.SemiBold,
            variationSettings = FontVariation.Settings(
                FontVariation.weight(FontWeight.SemiBold.weight),
            ),
        ),
        Font(
            resId = R.font.instrument_sans,
            weight = FontWeight.Bold,
            variationSettings = FontVariation.Settings(
                FontVariation.weight(FontWeight.Bold.weight),
            ),
        ),
    )

    val Mono: FontFamily = FontFamily(
        Font(
            resId = R.font.geist_mono,
            weight = FontWeight.Normal,
            variationSettings = FontVariation.Settings(
                FontVariation.weight(FontWeight.Normal.weight),
            ),
        ),
        Font(
            resId = R.font.geist_mono,
            weight = FontWeight.Medium,
            variationSettings = FontVariation.Settings(
                FontVariation.weight(FontWeight.Medium.weight),
            ),
        ),
        Font(
            resId = R.font.geist_mono,
            weight = FontWeight.SemiBold,
            variationSettings = FontVariation.Settings(
                FontVariation.weight(FontWeight.SemiBold.weight),
            ),
        ),
        Font(
            resId = R.font.geist_mono,
            weight = FontWeight.Bold,
            variationSettings = FontVariation.Settings(
                FontVariation.weight(FontWeight.Bold.weight),
            ),
        ),
    )

    val bundledFontResources: List<Int> = listOf(
        R.font.instrument_sans,
        R.font.geist_mono,
    )
}

/**
 * Typeface family names carried by a design skin.
 */
@Immutable
data class Faces(
    val display: String,
    val body: String,
    val mono: String,
) {
    companion object {
        val Instrument = Faces(
            display = "Instrument Sans",
            body = "Instrument Sans",
            mono = "Geist Mono",
        )
    }
}

/**
 * Semantic type roles matching the shared amux design table.
 */
enum class TextRole(val tokenName: String) {
    ScreenTitle("screenTitle"),
    SectionTitle("sectionTitle"),
    Identifier("identifier"),
    Body("body"),
    BodyEmphasis("bodyEmphasis"),
    Detail("detail"),
    Caption("caption"),
    Mono("mono"),
    MonoSmall("monoSmall"),
}

/**
 * A resolved type role specification. All sizes are expressed in scalable
 * pixels (`sp`) so Android system font scaling applies natively.
 */
@Immutable
data class FontSpec(
    val family: String,
    val fontFamily: FontFamily,
    val sizeSp: Float,
    val lineHeightSp: Float,
    val weight: FontWeight,
    val trackingSp: Float = 0f,
) {
    fun toTextStyle(): TextStyle = TextStyle(
        fontFamily = fontFamily,
        fontWeight = weight,
        fontSize = sizeSp.sp,
        lineHeight = lineHeightSp.sp,
        letterSpacing = trackingSp.sp,
    )
}

/**
 * Semantic typography styles exposed via [AmuxTheme.typography] and mapped
 * onto Material 3 [Typography].
 */
@Immutable
data class AmuxTypography(
    val screenTitle: TextStyle,
    val sectionTitle: TextStyle,
    val identifier: TextStyle,
    val body: TextStyle,
    val bodyEmphasis: TextStyle,
    val detail: TextStyle,
    val caption: TextStyle,
    val mono: TextStyle,
    val monoSmall: TextStyle,
    val badge: TextStyle,
) {
    /** Alias for [identifier] when styling agent or host list row titles. */
    val rowTitle: TextStyle
        get() = identifier

    /**
     * Maps amux's bundled type scale onto Material 3 [Typography] so standard
     * Material components inherit Instrument Sans and never leak system Roboto.
     */
    fun toMaterialTypography(): Typography = Typography(
        displayLarge = screenTitle.copy(fontSize = 34.sp, lineHeight = 40.sp),
        displayMedium = screenTitle.copy(fontSize = 30.sp, lineHeight = 36.sp),
        displaySmall = screenTitle,
        headlineLarge = screenTitle,
        headlineMedium = screenTitle.copy(fontSize = 22.sp, lineHeight = 28.sp),
        headlineSmall = screenTitle.copy(fontSize = 20.sp, lineHeight = 26.sp),
        titleLarge = screenTitle.copy(fontSize = 20.sp, lineHeight = 26.sp),
        titleMedium = identifier,
        titleSmall = bodyEmphasis.copy(fontSize = 14.sp, lineHeight = 20.sp),
        bodyLarge = body,
        bodyMedium = detail,
        bodySmall = caption,
        labelLarge = bodyEmphasis.copy(fontSize = 14.sp, lineHeight = 20.sp),
        labelMedium = sectionTitle,
        labelSmall = badge,
    )
}
