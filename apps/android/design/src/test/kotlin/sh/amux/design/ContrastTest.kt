package sh.amux.design

import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.compositeOver
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

        }
    }

    /**
     * Every accent fill that carries a label (a pill, a button, a chosen
     * chip or question step, the account disc, New Agent) puts onAccent on
     * accentFill, which is opaque, so one pair covers every surface it sits
     * on.
     */
    @Test
    fun labelsOnAnAccentFillMeetWcagAa() {
        for (appearance in Appearance.entries) {
            val c = AmuxDesign.App.colors(appearance)
            val ratio = contrastRatio(c.onAccent, c.accentFill)
            assertTrue(
                "$appearance onAccent on accentFill must be >= 4.5:1 (AA), was ${"%.2f".format(ratio)}:1",
                ratio >= 4.5,
            )
        }
    }

    /** An accent fill is still a control's shape against the page around it. */
    @Test
    fun accentFillStandsOffEverySurface() {
        for (appearance in Appearance.entries) {
            val c = AmuxDesign.App.colors(appearance)
            for ((surfaceName, surface) in surfacesOf(c)) {
                val ratio = contrastRatio(c.accentFill, surface)
                assertTrue(
                    "$appearance accentFill on $surfaceName must be >= 3.0:1 (AA UI), was ${"%.2f".format(ratio)}:1",
                    ratio >= 3.0,
                )
            }
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
    fun codeColoursReadOnTheWellAndUnderEitherDiffTint() {
        for (appearance in Appearance.entries) {
            val c = AmuxDesign.App.colors(appearance)
            val grounds = listOf(
                "ground" to c.ground,
                "sunken" to c.sunken,
                "added tint" to c.addedTint.compositeOver(c.ground),
                "removed tint" to c.removedTint.compositeOver(c.ground),
            )
            val inks = listOf(
                "keyword" to c.syntax.keyword,
                "string" to c.syntax.string,
                "number" to c.syntax.number,
                "type" to c.syntax.type,
                "function" to c.syntax.function,
                "comment" to c.syntax.comment,
            )
            for ((groundName, ground) in grounds) {
                for ((inkName, ink) in inks) {
                    val ratio = contrastRatio(ink, ground)
                    assertTrue(
                        "$appearance $inkName on $groundName must be >= 4.5:1, was ${"%.2f".format(ratio)}:1",
                        ratio >= 4.5,
                    )
                }
            }
        }
    }

    @Test
    fun destructiveLabelMeetsWcagAaOnTheRemovedFill() {
        for (appearance in Appearance.entries) {
            val c = AmuxDesign.App.colors(appearance)
            val ratio = contrastRatio(c.onRemoved, c.removed)
            assertTrue(
                "$appearance onRemoved on removed must be >= 4.5:1 (AA), was ${"%.2f".format(ratio)}:1",
                ratio >= 4.5,
            )
        }
    }

    /**
     * In dark mode the chosen segment has no shadow to set it off, so its
     * thumb must read against the track by fill alone.
     */
    @Test
    fun darkChosenSegmentStandsOffItsTrack() {
        val c = AmuxDesign.App.colors(Appearance.Dark)
        val ratio = contrastRatio(c.selection, c.segmentTrack)
        assertTrue(
            "dark selection on segmentTrack must be >= 2.0:1, was ${"%.2f".format(ratio)}:1",
            ratio >= 2.0,
        )
    }

    @Test
    fun chosenSegmentLabelStaysReadableOnTheSelectionFill() {
        for (appearance in Appearance.entries) {
            val c = AmuxDesign.App.colors(appearance)
            val ratio = contrastRatio(c.ink, c.selection)
            assertTrue(
                "$appearance ink on selection must be >= 4.5:1 (AA), was ${"%.2f".format(ratio)}:1",
                ratio >= 4.5,
            )
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
