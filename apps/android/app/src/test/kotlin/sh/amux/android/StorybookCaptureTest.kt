package sh.amux.android

import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.getValue
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onRoot
import androidx.compose.ui.unit.Density
import com.github.takahirom.roborazzi.ExperimentalRoborazziApi
import com.github.takahirom.roborazzi.RoborazziOptions
import com.github.takahirom.roborazzi.RoborazziTaskType
import com.github.takahirom.roborazzi.captureRoboImage
import java.io.File
import kotlinx.serialization.Serializable
import kotlinx.serialization.encodeToString
import kotlinx.serialization.json.Json
import org.junit.Assert.assertTrue
import org.junit.Assume.assumeTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode
import sh.amux.design.AmuxTheme
import sh.amux.design.Appearance
import sh.amux.testsupport.Stories
import sh.amux.testsupport.Story

/**
 * Headless JVM Robolectric + Roborazzi renderer for all registered [Stories].
 *
 * Gated behind `-Damux.storybook.capture=true` so ordinary `just android unit`
 * stays fast, while `just android storybook [filter]` renders Light and Dark
 * PNGs at Pixel 9 portrait viewport (412x915dp) into `target/android/storybook/png/`.
 */
@RunWith(RobolectricTestRunner::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
@Config(sdk = [34], qualifiers = "w412dp-h915dp-xxhdpi")
class StorybookCaptureTest {
    @get:Rule
    val composeRule = createComposeRule()

    @Serializable
    private data class CapturedStory(
        val id: String,
        val area: String,
        val title: String,
        val description: String,
        val fontScale: Float,
        val lightPng: String,
        val darkPng: String,
    )

    @Serializable
    private data class StorybookManifest(
        val stories: List<CapturedStory>,
    )

    private data class RenderSlot(
        val story: Story,
        val appearance: Appearance,
    )

    @OptIn(ExperimentalRoborazziApi::class)
    @Test
    fun renderRegisteredStories() {
        assumeTrue(
            "Pass -Damux.storybook.capture=true (via `just android storybook`) to render PNGs",
            System.getProperty("amux.storybook.capture") == "true",
        )

        val filter = System.getProperty("amux.storybook.filter")
        val selected = Stories.matching(filter)
        assertTrue("No stories matched filter '$filter'", selected.isNotEmpty())

        val outRoot = System.getProperty("amux.storybook.out")
            ?.takeIf { it.isNotBlank() }
            ?.let(::File)
            ?: File("../../../target/android/storybook").canonicalFile
        val pngDir = File(outRoot, "png").apply { mkdirs() }

        var activeSlot by mutableStateOf<RenderSlot?>(null)

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

        val options = RoborazziOptions(taskType = RoborazziTaskType.Record)
        val manifestEntries = mutableListOf<CapturedStory>()

        for (story in selected) {
            for (appearance in listOf(Appearance.Light, Appearance.Dark)) {
                activeSlot = RenderSlot(story = story, appearance = appearance)
                composeRule.waitForIdle()
                val fileName = "${story.id}.${appearance.wireName}.png"
                val targetFile = File(pngDir, fileName)
                composeRule.onRoot().captureRoboImage(
                    filePath = targetFile.absolutePath,
                    roborazziOptions = options,
                )
                assertTrue(
                    "Expected non-empty storybook PNG at ${targetFile.absolutePath}",
                    targetFile.isFile && targetFile.length() > 0L,
                )
            }
            manifestEntries += CapturedStory(
                id = story.id,
                area = story.area,
                title = story.title,
                description = story.description,
                fontScale = story.fontScale,
                lightPng = "png/${story.id}.light.png",
                darkPng = "png/${story.id}.dark.png",
            )
        }

        val json = Json { prettyPrint = true }
        File(outRoot, "manifest.json").writeText(
            json.encodeToString(StorybookManifest(stories = manifestEntries)),
            Charsets.UTF_8,
        )
    }
}
