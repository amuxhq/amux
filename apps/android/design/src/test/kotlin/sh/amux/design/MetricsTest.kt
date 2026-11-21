package sh.amux.design

import androidx.compose.animation.core.SnapSpec
import androidx.compose.animation.core.TweenSpec
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.size
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.test.getUnclippedBoundsInRoot
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.height
import androidx.compose.ui.unit.width
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class MetricsTest {
    @get:Rule
    val composeRule = createComposeRule()

    @Test
    fun thumbTargetMinSizeMeetsAndroidAccessibilityFloor() {
        assertTrue(
            "Android minimum touch target must be at least 48.dp, was ${ThumbTarget.MinSize}",
            ThumbTarget.MinSize >= 48.dp,
        )
        assertEquals(48.dp, ThumbTarget.MinSize)
    }

    @Test
    fun thumbTargetModifierEnforcesAtLeast48DpBounds() {
        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light) {
                Box(
                    modifier = Modifier
                        .testTag("small-control")
                        .thumbTarget(minWidth = 20.dp, minHeight = 20.dp),
                    contentAlignment = Alignment.Center,
                ) {
                    Box(modifier = Modifier.size(16.dp))
                }
            }
        }

        val bounds = composeRule.onNodeWithTag("small-control").getUnclippedBoundsInRoot()
        assertTrue(
            "thumbTarget width must be >= 48.dp, was ${bounds.width}",
            bounds.width >= 48.dp,
        )
        assertTrue(
            "thumbTarget height must be >= 48.dp, was ${bounds.height}",
            bounds.height >= 48.dp,
        )
    }

    @Test
    fun spacingAndMetricScalesAreStrictlyOrdered() {
        assertTrue(Space.xxs < Space.xs)
        assertTrue(Space.xs < Space.s)
        assertTrue(Space.s < Space.m)
        assertTrue(Space.m < Space.l)
        assertTrue(Space.l < Space.xl)
        assertTrue(Space.xl < Space.xxl)
        assertEquals(Space.l, Space.margin)

        val metrics = AmuxDesign.App.metrics
        assertEquals(1.dp, metrics.hairline)
        assertTrue(metrics.controlRadius < metrics.cardRadius)
        assertTrue(metrics.cardRadius < metrics.floatRadius)
    }

    @Test
    fun motionRespectsReducedMotionAndPhotographedFlags() {
        val standard = Motion.standardSpec<Float>(reducedMotion = false)
        assertTrue(standard is TweenSpec)
        assertEquals(280, (standard as TweenSpec).durationMillis)

        val quick = Motion.quickSpec<Float>(reducedMotion = false)
        assertTrue(quick is TweenSpec)
        assertEquals(180, (quick as TweenSpec).durationMillis)

        assertTrue(Motion.standardSpec<Float>(reducedMotion = true) is SnapSpec)
        assertTrue(Motion.quickSpec<Float>(reducedMotion = true) is SnapSpec)

        var reducedByDefault = true
        var reducedWhenPhotographed = false
        var reducedWhenFlagged = false

        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, reducedMotion = false, photographed = false) {
                reducedByDefault = Motion.isReducedMotion()
            }
            AmuxTheme(appearance = Appearance.Light, reducedMotion = false, photographed = true) {
                reducedWhenPhotographed = Motion.isReducedMotion()
            }
            AmuxTheme(appearance = Appearance.Light, reducedMotion = true, photographed = false) {
                reducedWhenFlagged = Motion.isReducedMotion()
            }
        }

        assertFalse(reducedByDefault)
        assertTrue(reducedWhenPhotographed)
        assertTrue(reducedWhenFlagged)
    }
}
