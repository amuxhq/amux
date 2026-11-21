package sh.amux.design

import androidx.compose.runtime.Immutable
import androidx.compose.ui.graphics.Color

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
) {
    /** Subtle line-highlight backdrop for added diff lines. */
    val addedTint: Color
        get() = added.copy(alpha = 0.12f)

    /** Subtle line-highlight backdrop for removed diff lines. */
    val removedTint: Color
        get() = removed.copy(alpha = 0.12f)
}
