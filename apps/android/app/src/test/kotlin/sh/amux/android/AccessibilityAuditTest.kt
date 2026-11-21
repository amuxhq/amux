package sh.amux.android

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.requiredSize
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.wrapContentWidth
import androidx.compose.material3.Text
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.getValue
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clipToBounds
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.SemanticsActions
import androidx.compose.ui.semantics.SemanticsNode
import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.semantics.getOrNull
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onRoot
import androidx.compose.ui.unit.Density
import androidx.compose.ui.unit.dp
import kotlin.math.max
import kotlin.math.min
import kotlin.math.pow
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode
import sh.amux.design.AmuxColors
import sh.amux.design.AmuxDesign
import sh.amux.design.AmuxTheme
import sh.amux.design.Appearance
import sh.amux.design.ThumbTarget
import sh.amux.testsupport.Stories
import sh.amux.testsupport.Story

/**
 * Automated accessibility audit sweeping every registered Android [Stories] screen
 * in both Light and Dark appearances (including 1.5x large-text stories) at the
 * Pixel 9 portrait viewport (412x915dp).
 *
 * Verifies:
 * - Every interactive node exposes a non-empty TalkBack label (`ContentDescription`,
 *   `Text`, or `EditableText`).
 * - Every interactive node meets Android's 48.dp minimum touch target in both width
 *   and height.
 * - No visible interactive node has zero width/height or clips outside the viewport
 *   horizontally.
 * - Every screen exposes heading semantics for TalkBack heading navigation.
 * - Light and Dark design tokens satisfy WCAG contrast floors.
 */
@RunWith(RobolectricTestRunner::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
@Config(sdk = [34], qualifiers = "w412dp-h915dp-xxhdpi")
class AccessibilityAuditTest {
    @get:Rule
    val composeRule = createComposeRule()

    private data class AuditSlot(
        val story: Story,
        val appearance: Appearance,
    )

    private data class AuditViolation(
        val storyId: String,
        val appearance: Appearance,
        val rule: String,
        val detail: String,
    ) {
        override fun toString(): String = "[$storyId / ${appearance.wireName}] $rule: $detail"
    }

    @Test
    fun allStoriesPassAccessibilityAudit() {
        val filter = System.getProperty("amux.accessibility.filter")
        val selected = Stories.matching(filter)
        assertTrue("No stories matched filter '$filter'", selected.isNotEmpty())

        var activeSlot by mutableStateOf<AuditSlot?>(null)

        composeRule.setContent {
            val slot = activeSlot
            if (slot != null) {
                val baseDensity = LocalDensity.current
                val scaledDensity = Density(
                    density = baseDensity.density,
                    fontScale = baseDensity.fontScale * slot.story.fontScale,
                )
                key(slot.story.id, slot.appearance) {
                    CompositionLocalProvider(LocalDensity provides scaledDensity) {
                        AmuxTheme(
                            appearance = slot.appearance,
                            photographed = true,
                        ) {
                            Box(modifier = Modifier.fillMaxSize()) {
                                slot.story.content()
                            }
                        }
                    }
                }
            }
        }

        val violations = mutableListOf<AuditViolation>()

        for (story in selected) {
            for (appearance in listOf(Appearance.Light, Appearance.Dark)) {
                activeSlot = AuditSlot(story = story, appearance = appearance)
                composeRule.waitForIdle()

                val root = composeRule.onRoot(useUnmergedTree = false).fetchSemanticsNode()
                val density = composeRule.density.density
                val viewportWidthPx = root.boundsInRoot.width
                val viewportHeightPx = root.boundsInRoot.height

                auditTree(
                    storyId = story.id,
                    appearance = appearance,
                    root = root,
                    density = density,
                    viewportWidthPx = viewportWidthPx,
                    viewportHeightPx = viewportHeightPx,
                    violations = violations,
                )
            }
        }

        if (violations.isNotEmpty()) {
            val summary = buildString {
                appendLine("Accessibility audit failed with ${violations.size} violation(s):")
                for (violation in violations) {
                    appendLine("  - $violation")
                }
            }
            assertTrue(summary, false)
        }
    }

    @Test
    fun themeContrastTokensMeetWcagFloors() {
        for (appearance in Appearance.entries) {
            val colors = AmuxDesign.App.colors(appearance)
            for ((surfaceName, surface) in surfacesOf(colors)) {
                val inkRatio = contrastRatio(colors.ink, surface)
                assertTrue(
                    "$appearance ink on $surfaceName must be >= 7.0:1, was ${"%.2f".format(inkRatio)}:1",
                    inkRatio >= 7.0,
                )
                val mutedRatio = contrastRatio(colors.inkMuted, surface)
                assertTrue(
                    "$appearance inkMuted on $surfaceName must be >= 4.5:1, was ${"%.2f".format(mutedRatio)}:1",
                    mutedRatio >= 4.5,
                )
                val faintRatio = contrastRatio(colors.inkFaint, surface)
                assertTrue(
                    "$appearance inkFaint on $surfaceName must be >= 3.0:1, was ${"%.2f".format(faintRatio)}:1",
                    faintRatio >= 3.0,
                )
                val accentRatio = contrastRatio(colors.accent, surface)
                assertTrue(
                    "$appearance accent on $surfaceName must be >= 4.5:1, was ${"%.2f".format(accentRatio)}:1",
                    accentRatio >= 4.5,
                )
                val removedRatio = contrastRatio(colors.removed, surface)
                assertTrue(
                    "$appearance removed on $surfaceName must be >= 4.5:1, was ${"%.2f".format(removedRatio)}:1",
                    removedRatio >= 4.5,
                )
                val addedRatio = contrastRatio(colors.added, surface)
                val addedFloor = if (appearance == Appearance.Light && surfaceName == "sunken") 4.0 else 4.5
                assertTrue(
                    "$appearance added on $surfaceName must be >= $addedFloor:1, was ${"%.2f".format(addedRatio)}:1",
                    addedRatio >= addedFloor,
                )
            }
            val onAccentRatio = contrastRatio(colors.onAccent, colors.accent)
            val onAccentFloor = if (appearance == Appearance.Light) 4.5 else 3.0
            assertTrue(
                "$appearance onAccent on accent must be >= $onAccentFloor:1, was ${"%.2f".format(onAccentRatio)}:1",
                onAccentRatio >= onAccentFloor,
            )
        }
    }

    @Test
    fun clippedNonScrollableControlFailsHorizontalClipCheck() {
        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                Box(modifier = Modifier.fillMaxSize().clipToBounds()) {
                    Row(
                        modifier = Modifier.wrapContentWidth(
                            align = Alignment.Start,
                            unbounded = true,
                        ),
                    ) {
                        Text(
                            text = "Heading",
                            modifier = Modifier
                                .width(390.dp)
                                .semantics { heading() },
                        )
                        Box(
                            modifier = Modifier
                                .requiredSize(width = 80.dp, height = 48.dp)
                                .clickable(onClick = {})
                                .testTag("clipped.action"),
                        ) {
                            Text("Overflow")
                        }
                    }
                }
            }
        }
        composeRule.waitForIdle()

        val root = composeRule.onRoot(useUnmergedTree = false).fetchSemanticsNode()
        val violations = mutableListOf<AuditViolation>()
        auditTree(
            storyId = "synthetic.clip",
            appearance = Appearance.Light,
            root = root,
            density = composeRule.density.density,
            viewportWidthPx = root.boundsInRoot.width,
            viewportHeightPx = root.boundsInRoot.height,
            violations = violations,
        )
        assertTrue(
            "Expected horizontal-clip violation for clipped non-scrollable control, got: $violations",
            violations.any { it.rule == "horizontal-clip" && it.detail.contains("clipped.action") },
        )
    }

    private fun auditTree(
        storyId: String,
        appearance: Appearance,
        root: SemanticsNode,
        density: Float,
        viewportWidthPx: Float,
        viewportHeightPx: Float,
        violations: MutableList<AuditViolation>,
    ) {
        val minTouchDp = ThumbTarget.MinSize.value - TOUCH_TARGET_TOLERANCE_DP
        var headingCount = 0

        fun walk(node: SemanticsNode, inHorizontalScroll: Boolean) {
            val config = node.config
            if (config.getOrNull(SemanticsProperties.Heading) != null) {
                headingCount += 1
            }

            if (isInteractive(node)) {
                val layoutSize = node.size
                val bounds = node.boundsInRoot
                val label = accessibleLabel(node)
                val descriptor = describeNode(node, label)

                // 1. Non-empty TalkBack label.
                if (label.isBlank()) {
                    violations += AuditViolation(
                        storyId = storyId,
                        appearance = appearance,
                        rule = "talkback-label",
                        detail = "Interactive node $descriptor has no ContentDescription, Text, or EditableText",
                    )
                }

                // 2. Zero-size interactive node check against unclipped layout size
                //    (boundsInRoot collapses to 0x0 for items scrolled outside a scroll viewport).
                if (layoutSize.width <= 0 || layoutSize.height <= 0) {
                    violations += AuditViolation(
                        storyId = storyId,
                        appearance = appearance,
                        rule = "zero-size",
                        detail = "Interactive node $descriptor has zero layout size (${layoutSize.width}x${layoutSize.height} px)",
                    )
                } else {
                    // 3. Minimum 48.dp touch target check against actual layout hit area
                    //    (M3 minimumInteractiveComponentSize and Modifier.thumbTarget() expand
                    //    layout size to >= 48.dp; checking layout size catches unpadded custom
                    //    clickable rows without false positives from scroll-edge clipping).
                    val widthDp = layoutSize.width / density
                    val heightDp = layoutSize.height / density
                    if (widthDp < minTouchDp || heightDp < minTouchDp) {
                        violations += AuditViolation(
                            storyId = storyId,
                            appearance = appearance,
                            rule = "touch-target",
                            detail = "Interactive node $descriptor layout touch target is " +
                                "${"%.1f".format(widthDp)}x${"%.1f".format(heightDp)} dp " +
                                "(minimum ${ThumbTarget.MinSize.value.toInt()}x${ThumbTarget.MinSize.value.toInt()} dp)",
                        )
                    }

                    // 4. Horizontal clipping check for visible items outside horizontal scrollables.
                    //    Use clipped boundsInRoot only to decide visibility, and check horizontal
                    //    extent against unclipped root coordinates so a non-scrollable layout
                    //    clipping a control past the viewport edge fails the audit.
                    val visibleOnScreen = bounds.width > 0f && bounds.height > 0f
                    if (!inHorizontalScroll && visibleOnScreen) {
                        val unclippedLeft = node.positionInRoot.x
                        val unclippedRight = unclippedLeft + layoutSize.width
                        if (unclippedLeft < -CLIP_TOLERANCE_PX || unclippedRight > viewportWidthPx + CLIP_TOLERANCE_PX) {
                            violations += AuditViolation(
                                storyId = storyId,
                                appearance = appearance,
                                rule = "horizontal-clip",
                                detail = "Interactive node $descriptor extends outside viewport " +
                                    "[${"%.1f".format(unclippedLeft / density)}dp..${"%.1f".format(unclippedRight / density)}dp] " +
                                    "vs viewport 0..${"%.1f".format(viewportWidthPx / density)}dp",
                            )
                        }
                    }
                }
            }

            val nextInHorizontalScroll = inHorizontalScroll ||
                config.getOrNull(SemanticsProperties.HorizontalScrollAxisRange) != null
            for (child in node.children) {
                walk(child, nextInHorizontalScroll)
            }
        }

        walk(root, inHorizontalScroll = false)

        if (headingCount == 0) {
            violations += AuditViolation(
                storyId = storyId,
                appearance = appearance,
                rule = "heading",
                detail = "Screen exposes no SemanticsProperties.Heading node for TalkBack navigation",
            )
        }
    }

    private fun isInteractive(node: SemanticsNode): Boolean {
        val config = node.config
        if (config.getOrNull(SemanticsProperties.HideFromAccessibility) != null) {
            return false
        }
        return config.getOrNull(SemanticsActions.OnClick) != null ||
            config.getOrNull(SemanticsActions.OnLongClick) != null ||
            config.getOrNull(SemanticsProperties.ToggleableState) != null ||
            config.getOrNull(SemanticsProperties.Selected) != null ||
            config.getOrNull(SemanticsActions.SetText) != null
    }

    private fun accessibleLabel(node: SemanticsNode): String {
        val config = node.config
        val contentDescriptions = config.getOrNull(SemanticsProperties.ContentDescription)
            .orEmpty()
            .filter { it.isNotBlank() }
        if (contentDescriptions.isNotEmpty()) {
            return contentDescriptions.joinToString(", ")
        }
        val texts = config.getOrNull(SemanticsProperties.Text)
            .orEmpty()
            .map { it.text }
            .filter { it.isNotBlank() }
        if (texts.isNotEmpty()) {
            return texts.joinToString(" ")
        }
        val editable = config.getOrNull(SemanticsProperties.EditableText)?.text.orEmpty()
        if (editable.isNotBlank()) {
            return editable
        }
        // Check direct non-interactive children in case a clickable container didn't merge text.
        val childTexts = mutableListOf<String>()
        fun collect(current: SemanticsNode) {
            for (child in current.children) {
                if (isInteractive(child)) continue
                child.config.getOrNull(SemanticsProperties.ContentDescription)
                    ?.filter { it.isNotBlank() }
                    ?.let(childTexts::addAll)
                child.config.getOrNull(SemanticsProperties.Text)
                    ?.map { it.text }
                    ?.filter { it.isNotBlank() }
                    ?.let(childTexts::addAll)
                collect(child)
            }
        }
        collect(node)
        return childTexts.joinToString(" ")
    }

    private fun describeNode(node: SemanticsNode, label: String): String {
        val tag = node.config.getOrNull(SemanticsProperties.TestTag)
        val role = node.config.getOrNull(SemanticsProperties.Role)
        val parts = mutableListOf<String>()
        if (tag != null) parts += "tag=$tag"
        if (role != null) parts += "role=$role"
        if (label.isNotBlank()) {
            val short = label.replace('\n', ' ').take(40)
            parts += "label=\"$short\""
        }
        parts += "id=${node.id}"
        return "(${parts.joinToString(", ")})"
    }

    private fun surfacesOf(colors: AmuxColors): List<Pair<String, Color>> = listOf(
        "ground" to colors.ground,
        "raised" to colors.raised,
        "sunken" to colors.sunken,
    )

    private fun contrastRatio(foreground: Color, background: Color): Double {
        val l1 = relativeLuminance(foreground)
        val l2 = relativeLuminance(background)
        return (max(l1, l2) + 0.05) / (min(l1, l2) + 0.05)
    }

    private fun relativeLuminance(color: Color): Double {
        val r = linearize(color.red.toDouble())
        val g = linearize(color.green.toDouble())
        val b = linearize(color.blue.toDouble())
        return 0.2126 * r + 0.7152 * g + 0.0722 * b
    }

    private fun linearize(channel: Double): Double =
        if (channel <= 0.04045) channel / 12.92 else ((channel + 0.055) / 1.055).pow(2.4)

    companion object {
        private const val TOUCH_TARGET_TOLERANCE_DP = 0.5f
        private const val CLIP_TOLERANCE_PX = 1.5f
    }
}
