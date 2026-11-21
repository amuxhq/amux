package sh.amux.design

import androidx.compose.material3.ColorScheme
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Immutable
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.style.LineBreak
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

/**
 * The app's visual language, as immutable values.
 *
 * One rule outranks the rest: colour means attention. An agent with nothing
 * to say is drawn achromatically, so the eye lands on the one that does
 * without hunting for a badge — which is why there is a single chromatic
 * accent and every other structural token is a neutral grey.
 */
@Immutable
data class AmuxDesign(
    val name: String,
    val ground: Ramp,
    val raised: Ramp,
    val sunken: Ramp,
    val hairline: Ramp,
    val ink: Ramp,
    val inkMuted: Ramp,
    val inkFaint: Ramp,
    val accent: Ramp,
    val onAccent: Ramp,
    val added: Ramp,
    val removed: Ramp,
    val faces: Faces,
    val metrics: Metrics,
    val type: TypeScale,
    val surfaces: SurfacePolicy,
) {
    @Immutable
    data class Metrics(
        val cardRadius: Dp,
        val controlRadius: Dp,
        /** Radius of detached floating surfaces such as composer docks and pills. */
        val floatRadius: Dp,
        val rowPadding: Dp,
        val gutter: Dp,
        val feedGap: Dp,
        val hairline: Dp,
        val floatElevation: Dp = 3.dp,
        val scrimAlpha: Float = 0.25f,
    )

    /** How a raised surface separates itself from the ground behind it. */
    enum class Separation(val wireName: String) {
        /** A rule between rows, and no card container by default. */
        Rule("rule"),
        /** A filled neutral surface with a hairline border. */
        Card("card"),
    }

    @Immutable
    data class SurfacePolicy(
        val separation: Separation,
        val graduated: Boolean,
    )

    /**
     * Where a surface earns container framing even under rule separation.
     * Subject surfaces (agents, active items) read as objects; plain
     * configuration stays quiet.
     */
    enum class Prominence {
        Subject,
        Plain,
    }

    @Immutable
    data class TypeScale(
        val identifierIsMono: Boolean,
        val titleWeight: FontWeight,
        val titleSizeSp: Float,
        val bodySizeSp: Float,
        val tightTrackingSp: Float,
    )

    /**
     * Every colour token in canonical order, so token table audits and
     * catalogues stay in lockstep.
     */
    val colours: List<Pair<String, Ramp>>
        get() = listOf(
            "ground" to ground,
            "raised" to raised,
            "sunken" to sunken,
            "hairline" to hairline,
            "ink" to ink,
            "inkMuted" to inkMuted,
            "inkFaint" to inkFaint,
            "accent" to accent,
            "onAccent" to onAccent,
            "added" to added,
            "removed" to removed,
        )

    fun colors(appearance: Appearance): AmuxColors = AmuxColors(
        ground = ground.color(appearance),
        raised = raised.color(appearance),
        sunken = sunken.color(appearance),
        hairline = hairline.color(appearance),
        ink = ink.color(appearance),
        inkMuted = inkMuted.color(appearance),
        inkFaint = inkFaint.color(appearance),
        accent = accent.color(appearance),
        onAccent = onAccent.color(appearance),
        added = added.color(appearance),
        removed = removed.color(appearance),
    )

    /**
     * Maps amux's fixed neutral tokens and single chromatic accent onto a
     * Material 3 [ColorScheme]. Dynamic / wallpaper colour is never used, and
     * `surfaceTint` is kept transparent so elevated M3 containers stay neutral.
     */
    fun colorScheme(appearance: Appearance): ColorScheme {
        val c = colors(appearance)
        return when (appearance) {
            Appearance.Light -> lightColorScheme(
                primary = c.accent,
                onPrimary = c.onAccent,
                primaryContainer = c.sunken,
                onPrimaryContainer = c.accent,
                secondary = c.inkMuted,
                onSecondary = c.ground,
                secondaryContainer = c.sunken,
                onSecondaryContainer = c.ink,
                tertiary = c.inkMuted,
                onTertiary = c.ground,
                tertiaryContainer = c.sunken,
                onTertiaryContainer = c.ink,
                background = c.ground,
                onBackground = c.ink,
                surface = c.ground,
                onSurface = c.ink,
                surfaceVariant = c.sunken,
                onSurfaceVariant = c.inkMuted,
                surfaceTint = Color.Transparent,
                surfaceContainerLowest = c.ground,
                surfaceContainerLow = c.ground,
                surfaceContainer = c.raised,
                surfaceContainerHigh = c.sunken,
                surfaceContainerHighest = c.sunken,
                outline = c.hairline,
                outlineVariant = c.hairline,
                error = c.removed,
                onError = c.onAccent,
                errorContainer = c.sunken,
                onErrorContainer = c.removed,
                inverseSurface = c.ink,
                inverseOnSurface = c.ground,
                inversePrimary = accent.color(Appearance.Dark),
                scrim = Color.Black,
            )

            Appearance.Dark -> darkColorScheme(
                primary = c.accent,
                onPrimary = c.onAccent,
                primaryContainer = c.raised,
                onPrimaryContainer = c.accent,
                secondary = c.inkMuted,
                onSecondary = c.ground,
                secondaryContainer = c.raised,
                onSecondaryContainer = c.ink,
                tertiary = c.inkMuted,
                onTertiary = c.ground,
                tertiaryContainer = c.raised,
                onTertiaryContainer = c.ink,
                background = c.ground,
                onBackground = c.ink,
                surface = c.ground,
                onSurface = c.ink,
                surfaceVariant = c.sunken,
                onSurfaceVariant = c.inkMuted,
                surfaceTint = Color.Transparent,
                surfaceContainerLowest = c.ground,
                surfaceContainerLow = c.sunken,
                surfaceContainer = c.raised,
                surfaceContainerHigh = c.raised,
                surfaceContainerHighest = c.raised,
                outline = c.hairline,
                outlineVariant = c.hairline,
                error = c.removed,
                onError = c.ground,
                errorContainer = c.raised,
                onErrorContainer = c.removed,
                inverseSurface = c.ink,
                inverseOnSurface = c.ground,
                inversePrimary = accent.color(Appearance.Light),
                scrim = Color.Black,
            )
        }
    }

    fun spec(role: TextRole): FontSpec {
        val body = type.bodySizeSp
        return when (role) {
            TextRole.ScreenTitle -> FontSpec(
                family = faces.display,
                fontFamily = AmuxFonts.Sans,
                sizeSp = type.titleSizeSp,
                lineHeightSp = type.titleSizeSp + 6f,
                weight = type.titleWeight,
                trackingSp = type.tightTrackingSp,
            )

            TextRole.SectionTitle -> FontSpec(
                family = faces.body,
                fontFamily = AmuxFonts.Sans,
                sizeSp = 13f,
                lineHeightSp = 18f,
                weight = FontWeight.SemiBold,
            )

            TextRole.Identifier -> if (type.identifierIsMono) {
                FontSpec(
                    family = faces.mono,
                    fontFamily = AmuxFonts.Mono,
                    sizeSp = body - 0.5f,
                    lineHeightSp = body + 4.5f,
                    weight = FontWeight.Medium,
                )
            } else {
                FontSpec(
                    family = faces.body,
                    fontFamily = AmuxFonts.Sans,
                    sizeSp = body + 1f,
                    lineHeightSp = body + 6f,
                    weight = FontWeight.SemiBold,
                )
            }

            TextRole.Body -> FontSpec(
                family = faces.body,
                fontFamily = AmuxFonts.Sans,
                sizeSp = body,
                lineHeightSp = body + 6f,
                weight = FontWeight.Normal,
            )

            TextRole.BodyEmphasis -> FontSpec(
                family = faces.body,
                fontFamily = AmuxFonts.Sans,
                sizeSp = body,
                lineHeightSp = body + 6f,
                weight = FontWeight.SemiBold,
            )

            TextRole.Detail -> FontSpec(
                family = faces.body,
                fontFamily = AmuxFonts.Sans,
                sizeSp = body - 1f,
                lineHeightSp = body + 4f,
                weight = FontWeight.Normal,
            )

            TextRole.Caption -> FontSpec(
                family = faces.body,
                fontFamily = AmuxFonts.Sans,
                sizeSp = 13f,
                lineHeightSp = 18f,
                weight = FontWeight.Normal,
            )

            TextRole.Mono -> FontSpec(
                family = faces.mono,
                fontFamily = AmuxFonts.Mono,
                sizeSp = body - 2f,
                lineHeightSp = body + 4f,
                weight = FontWeight.Normal,
            )

            TextRole.MonoSmall -> FontSpec(
                family = faces.mono,
                fontFamily = AmuxFonts.Mono,
                sizeSp = body - 3.5f,
                lineHeightSp = body + 1f,
                weight = FontWeight.Normal,
            )
        }
    }

    val typography: AmuxTypography
        get() = AmuxTypography(
            // Headings break balanced, so a two-line title never ends on one word.
            screenTitle = spec(TextRole.ScreenTitle).toTextStyle().copy(lineBreak = LineBreak.Heading),
            sectionTitle = spec(TextRole.SectionTitle).toTextStyle(),
            identifier = spec(TextRole.Identifier).toTextStyle(),
            body = spec(TextRole.Body).toTextStyle(),
            bodyEmphasis = spec(TextRole.BodyEmphasis).toTextStyle(),
            detail = spec(TextRole.Detail).toTextStyle(),
            caption = spec(TextRole.Caption).toTextStyle(),
            mono = spec(TextRole.Mono).toTextStyle(),
            monoSmall = spec(TextRole.MonoSmall).toTextStyle(),
            badge = TextStyle(
                fontFamily = AmuxFonts.Sans,
                fontWeight = FontWeight.SemiBold,
                fontSize = 12.sp,
                lineHeight = 16.sp,
            ),
        )

    companion object {
        /**
         * The app's one skin: type-led, neutral surfaces with crisp hairlines,
         * a single azure attention accent, muted diff tones, and mono kept for
         * commands, paths, and diffs.
         */
        val App = AmuxDesign(
            name = "Studio",
            ground = Ramp(0xFFFFFF, 0x000000),
            raised = Ramp(0xFFFFFF, 0x1C1C1E),
            sunken = Ramp(0xF2F2F4, 0x161618),
            hairline = Ramp(0xDCDCE0, 0x38383A),
            ink = Ramp(0x1D1C1F, 0xF5F5F7),
            inkMuted = Ramp(0x5E5E66, 0x98989F),
            inkFaint = Ramp(0x8A8A93, 0x6E6E73),
            accent = Ramp(0x0071C2, 0x3B90DF),
            onAccent = Ramp(0xFFFFFF, 0xFFFFFF),
            added = Ramp(0x29854F, 0x70C78C),
            removed = Ramp(0xA8333D, 0xE68080),
            faces = Faces.Instrument,
            metrics = Metrics(
                cardRadius = 16.dp,
                controlRadius = 12.dp,
                floatRadius = 26.dp,
                rowPadding = 12.dp,
                gutter = 16.dp,
                feedGap = 16.dp,
                hairline = 1.dp,
                floatElevation = 3.dp,
                scrimAlpha = 0.25f,
            ),
            type = TypeScale(
                identifierIsMono = false,
                titleWeight = FontWeight.SemiBold,
                titleSizeSp = 26f,
                bodySizeSp = 16f,
                tightTrackingSp = -0.4f,
            ),
            surfaces = SurfacePolicy(
                separation = Separation.Rule,
                graduated = false,
            ),
        )
    }
}
