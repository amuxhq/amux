package sh.amux.design

import androidx.compose.runtime.Immutable
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.luminance

/**
 * One colour token, stated once as 24-bit sRGB hex values for both appearances
 * and resolved where it is drawn.
 *
 * The raw hex values stay on the token so the entire palette can be audited
 * and pinned as data.
 */
@Immutable
data class Ramp(
    val light: Int,
    val dark: Int,
) {
    init {
        require((light and 0x00FFFFFF) == light) {
            "light must be a 24-bit RGB value, got 0x${light.toUInt().toString(16)}"
        }
        require((dark and 0x00FFFFFF) == dark) {
            "dark must be a 24-bit RGB value, got 0x${dark.toUInt().toString(16)}"
        }
    }

    fun hex(appearance: Appearance): Int = when (appearance) {
        Appearance.Light -> light
        Appearance.Dark -> dark
    }

    fun hexString(appearance: Appearance): String =
        "#%06X".format(hex(appearance))

    fun color(appearance: Appearance): Color =
        rgbToColor(hex(appearance))

    companion object {
        internal fun rgbToColor(rgb: Int): Color {
            val red = (rgb ushr 16) and 0xFF
            val green = (rgb ushr 8) and 0xFF
            val blue = rgb and 0xFF
            return Color(red = red, green = green, blue = blue, alpha = 0xFF)
        }
    }
}

/**
 * Resolved colour tokens for a single active [Appearance].
 */
@Immutable
data class AmuxColors(
    val ground: Color,
    val raised: Color,
    val sunken: Color,
    val hairline: Color,
    val ink: Color,
    val inkMuted: Color,
    val inkFaint: Color,
    val accent: Color,
    val onAccent: Color,
    val added: Color,
    val removed: Color,
    val syntax: SyntaxColors,
) {
    /** Subtle line-highlight backdrop for added diff lines. */
    val addedTint: Color
        get() = added.copy(alpha = 0.12f)

    /** Subtle line-highlight backdrop for removed diff lines. */
    val removedTint: Color
        get() = removed.copy(alpha = 0.12f)

    private val dark: Boolean
        get() = ground.luminance() < 0.5f

    /**
     * The label on a [removed] fill. White on the dark-mode red is under 3:1,
     * so a destructive button takes the page's own ground for its label, as
     * the primary button does on its ink fill: white on the deep light-mode
     * red, black on the pale dark-mode one, both past 6:1.
     */
    val onRemoved: Color
        get() = ground

    /**
     * The accent as a fill that carries a label in [onAccent]: a pill, a
     * button, a chosen chip, the account disc. The dark-mode accent is pale
     * enough to read as text on black, which leaves white on it at 3.4:1, so
     * a fill takes a deeper blue there (white on it 5.0:1). Where the accent
     * is the colour of text or a glyph on a surface, use [accent].
     */
    val accentFill: Color
        get() = if (dark) DarkAccentFill else accent

    /**
     * The thumb on the chosen segment of a segmented choice, as iOS's
     * segmented control draws it: white in light mode, where a shadow and an
     * edge set it off the track, and in dark mode a grey clearly lighter
     * than the track, which is how it reads there without a shadow.
     */
    val selection: Color
        get() = if (dark) DarkSegmentThumb else raised

    /** The track a segmented choice's segments sit in. */
    val segmentTrack: Color
        get() = if (dark) DarkSegmentTrack else sunken

    /**
     * A top bar or sheet header once content has scrolled under it: one step
     * off the page instead of a hairline under the bar.
     */
    val barScrolled: Color
        get() = if (dark) raised else sunken
}

/** [Syntax] resolved for one appearance. */
@Immutable
data class SyntaxColors(
    val keyword: Color,
    val string: Color,
    val number: Color,
    val type: Color,
    val function: Color,
    val comment: Color,
)

/** The dark-mode [AmuxColors.accentFill]; see there. */
private val DarkAccentFill = Ramp.rgbToColor(0x2272BD)

/** iOS's dark segmented-control thumb. */
private val DarkSegmentThumb = Ramp.rgbToColor(0x636366)

/** iOS's dark segmented-control track. */
private val DarkSegmentTrack = Ramp.rgbToColor(0x2C2C2E)
