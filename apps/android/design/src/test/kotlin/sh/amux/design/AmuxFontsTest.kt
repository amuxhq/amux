package sh.amux.design

import androidx.compose.foundation.layout.Column
import androidx.compose.material3.ColorScheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.Typography
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithText
import androidx.test.core.app.ApplicationProvider
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import java.nio.ByteBuffer
import java.nio.ByteOrder

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class AmuxFontsTest {
    @get:Rule
    val composeRule = createComposeRule()

    @Test
    fun bundledFontBinariesAreValidOpenTypeTrueTypeFiles() {
        val resources = ApplicationProvider.getApplicationContext<android.content.Context>().resources
        val expectedTrueTypeSfnt = 0x00010000
        val expectedOpenTypeCffSfnt = 0x4F54544F // 'OTTO'

        for (resId in AmuxFonts.bundledFontResources) {
            val bytes = resources.openRawResource(resId).use { it.readBytes() }
            assertTrue("Font resource $resId must be non-empty", bytes.size > 10_000)

            val buffer = ByteBuffer.wrap(bytes).order(ByteOrder.BIG_ENDIAN)
            val sfntVersion = buffer.int
            val numTables = buffer.short.toInt() and 0xFFFF

            assertTrue(
                "Font resource $resId has invalid sfnt magic 0x${sfntVersion.toUInt().toString(16)}",
                sfntVersion == expectedTrueTypeSfnt || sfntVersion == expectedOpenTypeCffSfnt,
            )
            assertTrue("Font resource $resId must declare OpenType tables", numTables >= 8)

            val tableTags = mutableSetOf<String>()
            for (i in 0 until numTables) {
                val offset = 12 + i * 16
                val tag = String(bytes, offset, 4, Charsets.US_ASCII)
                tableTags.add(tag)
            }
            assertTrue("Font $resId must include head table", "head" in tableTags)
            assertTrue("Font $resId must include cmap table", "cmap" in tableTags)
            assertTrue("Font $resId must include fvar variable axis table", "fvar" in tableTags)
        }
    }

    @Test
    fun amuxThemeLightSetsLocalAmuxDesignAndFixedNeutralColorScheme() {
        var capturedDesign: AmuxDesign? = null
        var capturedColors: AmuxColors? = null
        var capturedAppearance: Appearance? = null
        var capturedScheme: ColorScheme? = null
        var capturedTypography: Typography? = null

        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light) {
                capturedDesign = LocalAmuxDesign.current
                capturedColors = LocalAmuxColors.current
                capturedAppearance = LocalAmuxAppearance.current
                capturedScheme = MaterialTheme.colorScheme
                capturedTypography = MaterialTheme.typography
                AmuxGround {
                    Column {
                        AmuxPanel { Text("Panel light") }
                        AmuxChrome { Text("Chrome light") }
                        AmuxSurface(always = true) { Text("Surface light") }
                        AmuxHairline()
                    }
                }
            }
        }

        composeRule.onNodeWithText("Panel light").assertIsDisplayed()
        composeRule.onNodeWithText("Chrome light").assertIsDisplayed()
        composeRule.onNodeWithText("Surface light").assertIsDisplayed()

        assertEquals(AmuxDesign.App, capturedDesign)
        assertEquals(Appearance.Light, capturedAppearance)
        val expectedColors = AmuxDesign.App.colors(Appearance.Light)
        assertEquals(expectedColors, capturedColors)

        val scheme = capturedScheme
        assertNotNull(scheme)
        assertEquals(expectedColors.ground, scheme!!.background)
        assertEquals(expectedColors.ground, scheme.surface)
        assertEquals(expectedColors.raised, scheme.surfaceContainer)
        assertEquals(expectedColors.sunken, scheme.surfaceVariant)
        assertEquals(Color.Transparent, scheme.surfaceTint)
        assertEquals(expectedColors.ink, scheme.onBackground)
        assertEquals(expectedColors.ink, scheme.onSurface)
        assertEquals(expectedColors.inkMuted, scheme.onSurfaceVariant)
        assertEquals(expectedColors.hairline, scheme.outline)
        assertEquals(expectedColors.accent, scheme.primary)
        assertEquals(expectedColors.onAccent, scheme.onPrimary)
        assertEquals(expectedColors.removed, scheme.error)

        val typography = capturedTypography!!
        assertEquals(AmuxFonts.Sans, typography.displayLarge.fontFamily)
        assertEquals(AmuxFonts.Sans, typography.titleLarge.fontFamily)
        assertEquals(AmuxFonts.Sans, typography.bodyLarge.fontFamily)
        assertEquals(AmuxFonts.Sans, typography.labelLarge.fontFamily)
    }

    @Test
    fun amuxThemeDarkSetsLocalAmuxDesignAndFixedDarkColorScheme() {
        var capturedAppearance: Appearance? = null
        var capturedColors: AmuxColors? = null
        var capturedScheme: ColorScheme? = null

        composeRule.setContent {
            AmuxTheme(appearanceMode = AppearanceMode.Dark) {
                capturedAppearance = AmuxTheme.appearance
                capturedColors = AmuxTheme.colors
                capturedScheme = MaterialTheme.colorScheme
                AmuxGround {
                    AmuxPanel { Text("Panel dark") }
                }
            }
        }

        composeRule.onNodeWithText("Panel dark").assertIsDisplayed()

        assertEquals(Appearance.Dark, capturedAppearance)
        val expectedColors = AmuxDesign.App.colors(Appearance.Dark)
        assertEquals(expectedColors, capturedColors)

        val scheme = capturedScheme!!
        assertEquals(expectedColors.ground, scheme.background)
        assertEquals(expectedColors.ground, scheme.surface)
        assertEquals(expectedColors.raised, scheme.surfaceContainer)
        assertEquals(expectedColors.sunken, scheme.surfaceVariant)
        assertEquals(Color.Transparent, scheme.surfaceTint)
        assertEquals(expectedColors.ink, scheme.onBackground)
        assertEquals(expectedColors.ink, scheme.onSurface)
        assertEquals(expectedColors.hairline, scheme.outline)
        assertEquals(expectedColors.accent, scheme.primary)
        assertEquals(expectedColors.onAccent, scheme.onPrimary)
        assertEquals(expectedColors.removed, scheme.error)
    }
}
