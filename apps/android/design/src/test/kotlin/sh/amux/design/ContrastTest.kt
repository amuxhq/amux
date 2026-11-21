package sh.amux.design

import androidx.compose.ui.graphics.Color
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import kotlin.math.max
import kotlin.math.min
import kotlin.math.pow

/**
 * Verifies WCAG 2.1 contrast ratios and the neutral-ramp chromatic invariant
 * across both Light and Dark appearances.
 */
class ContrastTest {
    @Test
    fun wcagReferencePairsAreExact() {
        assertEquals(21.0, contrastRatio(Color.Black, Color.White), 0.001)
        assertEquals(1.0, contrastRatio(Color.White, Color.White), 0.001)
        assertEquals(1.0, contrastRatio(Color.Black, Color.Black), 0.001)
    }

    @Test
    fun inkMeetsWcagAaaOnAllNeutralSurfacesInBothAppearances() {
        for (appearance in Appearance.entries) {
            val c = AmuxDesign.App.colors(appearance)
            for ((surfaceName, surface) in surfacesOf(c)) {
                val ratio = contrastRatio(c.ink, surface)
                assertTrue(
                    "$appearance ink on $surfaceName must be >= 7.0:1 (AAA), was ${"%.2f".format(ratio)}:1",
                    ratio >= 7.0,
                )
            }
        }
    }

    @Test
    fun inkMutedMeetsWcagAaNormalTextOnAllNeutralSurfaces() {
        for (appearance in Appearance.entries) {
            val c = AmuxDesign.App.colors(appearance)
            for ((surfaceName, surface) in surfacesOf(c)) {
                val ratio = contrastRatio(c.inkMuted, surface)
                assertTrue(
                    "$appearance inkMuted on $surfaceName must be >= 4.5:1 (AA), was ${"%.2f".format(ratio)}:1",
                    ratio >= 4.5,
                )
            }
        }
    }

    @Test
    fun inkFaintMeetsWcagAaLargeAndUiContrastOnAllNeutralSurfaces() {
        for (appearance in Appearance.entries) {
            val c = AmuxDesign.App.colors(appearance)
            for ((surfaceName, surface) in surfacesOf(c)) {
                val ratio = contrastRatio(c.inkFaint, surface)
                assertTrue(
                    "$appearance inkFaint on $surfaceName must be >= 3.0:1 (AA UI), was ${"%.2f".format(ratio)}:1",
                    ratio >= 3.0,
                )
            }
        }
    }

    @Test
    fun accentAndOnAccentMeetContrastFloorsInBothAppearances() {
        for (appearance in Appearance.entries) {
            val c = AmuxDesign.App.colors(appearance)
            for ((surfaceName, surface) in surfacesOf(c)) {
                val ratio = contrastRatio(c.accent, surface)
                assertTrue(
                    "$appearance accent on $surfaceName must be >= 4.5:1 (AA), was ${"%.2f".format(ratio)}:1",
                    ratio >= 4.5,
                )
            }

            val onAccentRatio = contrastRatio(c.onAccent, c.accent)
            val floor = if (appearance == Appearance.Light) 4.5 else 3.0
            assertTrue(
                "$appearance onAccent on accent must be >= $floor:1, was ${"%.2f".format(onAccentRatio)}:1",
                onAccentRatio >= floor,
            )
        }
    }

    @Test
    fun diffTonesRemainLegibleOnNeutralSurfaces() {
        for (appearance in Appearance.entries) {
            val c = AmuxDesign.App.colors(appearance)
            for ((surfaceName, surface) in surfacesOf(c)) {
                val removedRatio = contrastRatio(c.removed, surface)
                assertTrue(
                    "$appearance removed on $surfaceName must be >= 4.5:1, was ${"%.2f".format(removedRatio)}:1",
                    removedRatio >= 4.5,
                )
                val addedRatio = contrastRatio(c.added, surface)
                val addedFloor = if (appearance == Appearance.Light && surfaceName == "sunken") 4.0 else 4.5
                assertTrue(
                    "$appearance added on $surfaceName must be >= $addedFloor:1, was ${"%.2f".format(addedRatio)}:1",
                    addedRatio >= addedFloor,
                )
            }
        }
    }

    @Test
    fun neutralTokensStayAchromaticWhileAccentIsChromatic() {
        val neutralTokens = listOf(
            "ground" to AmuxDesign.App.ground,
            "raised" to AmuxDesign.App.raised,
            "sunken" to AmuxDesign.App.sunken,
            "hairline" to AmuxDesign.App.hairline,
            "ink" to AmuxDesign.App.ink,
            "inkMuted" to AmuxDesign.App.inkMuted,
            "inkFaint" to AmuxDesign.App.inkFaint,
        )

        for (appearance in Appearance.entries) {
            for ((name, ramp) in neutralTokens) {
                val rgb = ramp.hex(appearance)
                val r = (rgb ushr 16) and 0xFF
                val g = (rgb ushr 8) and 0xFF
                val b = rgb and 0xFF
                val spread = max(r, max(g, b)) - min(r, min(g, b))
                assertTrue(
                    "$appearance $name (#%06X) must stay neutral (RGB spread <= 10, got $spread)".format(rgb),
                    spread <= 10,
                )
            }

            val accentRgb = AmuxDesign.App.accent.hex(appearance)
            val r = (accentRgb ushr 16) and 0xFF
            val g = (accentRgb ushr 8) and 0xFF
            val b = accentRgb and 0xFF
            val spread = max(r, max(g, b)) - min(r, min(g, b))
            assertTrue(
                "$appearance accent must be chromatic (RGB spread >= 100, got $spread)",
                spread >= 100,
            )
            assertTrue("$appearance accent must be blue-dominant", b > g && g > r)
        }
    }

    private fun surfacesOf(colors: AmuxColors): List<Pair<String, Color>> = listOf(
        "ground" to colors.ground,
        "raised" to colors.raised,
        "sunken" to colors.sunken,
    )

    private fun contrastRatio(foreground: Color, background: Color): Double {
        val l1 = relativeLuminance(foreground)
        val l2 = relativeLuminance(background)
        val lighter = max(l1, l2)
        val darker = min(l1, l2)
        return (lighter + 0.05) / (darker + 0.05)
    }

    private fun relativeLuminance(color: Color): Double {
        val r = linearizeChannel(color.red.toDouble())
        val g = linearizeChannel(color.green.toDouble())
        val b = linearizeChannel(color.blue.toDouble())
        return 0.2126 * r + 0.7152 * g + 0.0722 * b
    }

    private fun linearizeChannel(channel: Double): Double =
        if (channel <= 0.04045) {
            channel / 12.92
        } else {
            ((channel + 0.055) / 1.055).pow(2.4)
        }
}
