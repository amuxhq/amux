package sh.amux.features

import androidx.compose.foundation.layout.Column
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Edit
import android.graphics.Bitmap
import android.graphics.Canvas
import android.view.ViewGroup
import androidx.activity.ComponentActivity
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.ui.platform.LocalLayoutDirection
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.unit.LayoutDirection
import kotlin.math.abs
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode
import sh.amux.design.AmuxGround
import sh.amux.design.AmuxTheme
import sh.amux.design.Appearance
import sh.amux.words.RailJoin

/**
 * The rail that joins a run of steps is drawn down one column, whether a step
 * sits at the top level or is set in under a subagent: a nested step carries
 * its parent's rail past its own glyph rather than starting one of its own.
 */
@RunWith(RobolectricTestRunner::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
@Config(sdk = [34], qualifiers = "w412dp-h915dp-xxhdpi")
class RunRailTest {
    @get:Rule
    val composeRule = createAndroidComposeRule<ComponentActivity>()

    @Test
    fun aNestedStepsRailRunsDownTheSameColumnAsItsParents() {
        assertRailRunsDownTheGlyphColumn(LayoutDirection.Ltr)
    }

    @Test
    fun rightToLeftTheRailRunsDownTheRightHandGlyphColumn() {
        assertRailRunsDownTheGlyphColumn(LayoutDirection.Rtl)
    }

    private fun assertRailRunsDownTheGlyphColumn(direction: LayoutDirection) {
        composeRule.setContent {
            CompositionLocalProvider(LocalLayoutDirection provides direction) {
                AmuxTheme(appearance = Appearance.Light) {
                    AmuxGround {
                        Column {
                            GridRow(
                                kindTag = "top",
                                glyph = Icons.Outlined.Edit,
                                rail = RailJoin(continues = true),
                                verb = "Edited",
                            )
                            GridRow(
                                kindTag = "nested",
                                glyph = Icons.Outlined.Edit,
                                rail = RailJoin(continues = true, nested = true),
                                verb = "Edited",
                            )
                        }
                    }
                }
            }
        }

        val top = railFromStartPx(ChatRowTags.kind("top"), direction)
        val nested = railFromStartPx(ChatRowTags.kind("nested"), direction)
        assertEquals("the rail must not jog sideways where nesting starts", top, nested)

        val expected = with(composeRule.density) { RunRail.x.toPx() }
        assertTrue(
            "the rail sits on the glyph column's centre, $expected px from the start edge, was $top px",
            abs(top - expected) <= with(composeRule.density) { RowGridMetrics.GlyphCell.toPx() } / 4,
        )
    }

    /**
     * How far the rail's ink beside [tag]'s row is from the screen's start
     * edge, where every row's outer edge sits, read in the gap under the
     * row's content, where the rail is the only thing drawn.
     */
    private fun railFromStartPx(tag: String, direction: LayoutDirection): Float {
        composeRule.waitForIdle()
        val root = composeRule.activity.findViewById<ViewGroup>(android.R.id.content).getChildAt(0)
        val picture = Bitmap.createBitmap(root.width, root.height, Bitmap.Config.ARGB_8888)
        composeRule.runOnUiThread { root.draw(Canvas(picture)) }
        val bounds = composeRule.onNodeWithTag(tag).fetchSemanticsNode().boundsInRoot
        val y = bounds.bottom.toInt() - with(composeRule.density) { (RowGridMetrics.InRunBottom / 2).roundToPx() }
        // Across the whole width: a nested row's own bounds begin after its
        // indent, and the rail it carries runs outside them.
        val inked = (0 until picture.width).filter { x ->
            val pixel = picture.getPixel(x, y)
            val alpha = (pixel ushr 24) and 0xFF
            val r = (pixel shr 16) and 0xFF
            val g = (pixel shr 8) and 0xFF
            val b = pixel and 0xFF
            alpha > 0 && r + g + b < 3 * 250
        }
        assertTrue("no rail drawn in $tag", inked.isNotEmpty())
        val centre = (inked.first() + inked.last()) / 2f
        return if (direction == LayoutDirection.Rtl) picture.width - 1 - centre else centre
    }
}
