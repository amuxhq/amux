package sh.amux.design

import androidx.compose.ui.graphics.toArgb
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.TextUnitType
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Pins every light and dark colour token, metric, and type role against the
 * shared amux design specification.
 */
class TokenTableTest {
    @Test
    fun everyColourTokenMatchesAmuxDesignRamp() {
        val expected = listOf(
            Triple("ground", 0xFFFFFF, 0x000000),
            Triple("raised", 0xFFFFFF, 0x1C1C1E),
            Triple("sunken", 0xF2F2F4, 0x161618),
            Triple("hairline", 0xDCDCE0, 0x38383A),
            Triple("ink", 0x1D1C1F, 0xF5F5F7),
            Triple("inkMuted", 0x5E5E66, 0x98989F),
            Triple("inkFaint", 0x8A8A93, 0x6E6E73),
            Triple("accent", 0x0071C2, 0x3B90DF),
            Triple("onAccent", 0xFFFFFF, 0xFFFFFF),
            Triple("added", 0x29854F, 0x70C78C),
            Triple("removed", 0xA8333D, 0xE68080),
            Triple("keyword", 0x873A9A, 0xD49CE6),
            Triple("string", 0x964220, 0xE89F7A),
            Triple("number", 0x755600, 0xD9B65C),
            Triple("type", 0x1A636C, 0x72C3CC),
            Triple("function", 0x4A4FA3, 0xA8ACF2),
            Triple("comment", 0x5A5A61, 0x9B9BA2),
        )

        val actual = AmuxDesign.App.colours
        assertEquals(expected.size, actual.size)

        for ((index, exp) in expected.withIndex()) {
            val (name, expectedLight, expectedDark) = exp
            val (actualName, ramp) = actual[index]
            assertEquals("token name at index $index", name, actualName)
            assertEquals("$name light hex", expectedLight, ramp.hex(Appearance.Light))
            assertEquals("$name dark hex", expectedDark, ramp.hex(Appearance.Dark))
            assertEquals(
                "$name light sRGB ARGB",
                0xFF000000.toInt() or expectedLight,
                ramp.color(Appearance.Light).toArgb(),
            )
            assertEquals(
                "$name dark sRGB ARGB",
                0xFF000000.toInt() or expectedDark,
                ramp.color(Appearance.Dark).toArgb(),
            )
        }
    }

    @Test
    fun resolvedTokenTableMatchesPinnedSpecification() {
        val expected = """
            design Studio
            separation rule graduated false

            colour           light    dark
            ground          #FFFFFF  #000000
            raised          #FFFFFF  #1C1C1E
            sunken          #F2F2F4  #161618
            hairline        #DCDCE0  #38383A
            ink             #1D1C1F  #F5F5F7
            inkMuted        #5E5E66  #98989F
            inkFaint        #8A8A93  #6E6E73
            accent          #0071C2  #3B90DF
            onAccent        #FFFFFF  #FFFFFF
            added           #29854F  #70C78C
            removed         #A8333D  #E68080
            keyword         #873A9A  #D49CE6
            string          #964220  #E89F7A
            number          #755600  #D9B65C
            type            #1A636C  #72C3CC
            function        #4A4FA3  #A8ACF2
            comment         #5A5A61  #9B9BA2

            metric           value
            cardRadius      16
            controlRadius   12
            floatRadius     26
            rowPadding      12
            gutter          16
            feedGap         16
            hairline        1

            role             face                size  weight
            screenTitle     Instrument Sans     26    semibold  tracking -0.4
            sectionTitle    Instrument Sans     13    semibold
            identifier      Instrument Sans     17    semibold
            body            Instrument Sans     16    regular
            bodyEmphasis    Instrument Sans     16    semibold
            detail          Instrument Sans     15    regular
            caption         Instrument Sans     13    regular
            mono            Geist Mono          14    regular
            monoSmall       Geist Mono          12.5  regular
        """.trimIndent() + "\n"

        assertEquals(expected, renderTable(AmuxDesign.App))
    }

    @Test
    fun everyTypeRoleUsesScalableSpUnits() {
        for (role in TextRole.entries) {
            val spec = AmuxDesign.App.spec(role)
            assertFalse("${role.tokenName} family must not be blank", spec.family.isBlank())
            assertTrue("${role.tokenName} size must be positive", spec.sizeSp > 0f)
            assertTrue("${role.tokenName} lineHeight >= size", spec.lineHeightSp >= spec.sizeSp)

            val style = spec.toTextStyle()
            assertEquals(
                "${role.tokenName} fontSize must use sp for system font scaling",
                TextUnitType.Sp,
                style.fontSize.type,
            )
            assertEquals(
                "${role.tokenName} lineHeight must use sp",
                TextUnitType.Sp,
                style.lineHeight.type,
            )
            assertEquals(
                "${role.tokenName} letterSpacing must use sp",
                TextUnitType.Sp,
                style.letterSpacing.type,
            )
        }
    }

    @Test
    fun appearanceAndAppearanceModeResolveExpectedValues() {
        assertEquals(listOf("light", "dark"), Appearance.entries.map { it.wireName })
        assertFalse(Appearance.Light.isDark)
        assertTrue(Appearance.Dark.isDark)

        assertEquals(Appearance.Light, AppearanceMode.System.resolve(systemDark = false))
        assertEquals(Appearance.Dark, AppearanceMode.System.resolve(systemDark = true))
        assertEquals(Appearance.Light, AppearanceMode.Light.resolve(systemDark = true))
        assertEquals(Appearance.Dark, AppearanceMode.Dark.resolve(systemDark = false))
    }

    @Test
    fun diffTintsDeriveFromAddedAndRemovedTokens() {
        for (appearance in Appearance.entries) {
            val colors = AmuxDesign.App.colors(appearance)
            assertEquals(colors.added.copy(alpha = 0.12f), colors.addedTint)
            assertEquals(colors.removed.copy(alpha = 0.12f), colors.removedTint)
        }
    }

    @Test(expected = IllegalArgumentException::class)
    fun rampRejectsOutOfRangeRgbValues() {
        Ramp(light = 0x1FFFFFF, dark = 0x000000)
    }

    private fun renderTable(design: AmuxDesign): String {
        val lines = mutableListOf(
            "design ${design.name}",
            "separation ${design.surfaces.separation.wireName} graduated ${design.surfaces.graduated}",
            "",
            "colour           light    dark",
        )
        for ((name, ramp) in design.colours) {
            lines.add(
                "${pad(name, 16)}${ramp.hexString(Appearance.Light)}  ${ramp.hexString(Appearance.Dark)}",
            )
        }
        val m = design.metrics
        lines += listOf(
            "",
            "metric           value",
            "${pad("cardRadius", 16)}${formatNumber(m.cardRadius.value)}",
            "${pad("controlRadius", 16)}${formatNumber(m.controlRadius.value)}",
            "${pad("floatRadius", 16)}${formatNumber(m.floatRadius.value)}",
            "${pad("rowPadding", 16)}${formatNumber(m.rowPadding.value)}",
            "${pad("gutter", 16)}${formatNumber(m.gutter.value)}",
            "${pad("feedGap", 16)}${formatNumber(m.feedGap.value)}",
            "${pad("hairline", 16)}${formatNumber(m.hairline.value)}",
            "",
            "role             face                size  weight",
        )
        for (role in TextRole.entries) {
            val spec = design.spec(role)
            val trackingSuffix = if (spec.trackingSp == 0f) {
                ""
            } else {
                "  tracking ${formatNumber(spec.trackingSp)}"
            }
            val weightText = weightName(spec.weight)
            val line = pad(role.tokenName, 16) +
                pad(spec.family, 20) +
                pad(formatNumber(spec.sizeSp), 6) +
                weightText +
                trackingSuffix
            lines.add(line)
        }
        return lines.joinToString("\n") + "\n"
    }

    private fun pad(text: String, width: Int): String =
        if (text.length >= width) "$text " else text + " ".repeat(width - text.length)

    private fun formatNumber(value: Float): String {
        val rounded = kotlin.math.round(value)
        return if (kotlin.math.abs(value - rounded) < 1e-4f) {
            rounded.toInt().toString()
        } else {
            value.toString()
        }
    }

    private fun weightName(weight: FontWeight): String = when (weight) {
        FontWeight.Normal -> "regular"
        FontWeight.Medium -> "medium"
        FontWeight.SemiBold -> "semibold"
        FontWeight.Bold -> "bold"
        else -> "w${weight.weight}"
    }
}
