package sh.amux.features

import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performScrollTo
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode
import sh.amux.design.AmuxTheme
import sh.amux.design.Appearance
import sh.amux.values.Changeability
import sh.amux.values.Changeable
import sh.amux.values.CommandView
import sh.amux.values.ContextView
import sh.amux.values.EffortChoice
import sh.amux.values.Kind
import sh.amux.values.ModeChoice
import sh.amux.values.ModelChoice
import sh.amux.values.PermissionChoice
import sh.amux.values.SettingChange
import sh.amux.values.SettingsView

@RunWith(RobolectricTestRunner::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
@Config(sdk = [34], qualifiers = "w412dp-h915dp-xxhdpi")
class ChatSettingsSheetTest {
    @get:Rule
    val composeRule = createComposeRule()

    @Test
    fun pickableSettingsEmitModelEffortPermissionModeAndCommandCallbacks() {
        val changes = mutableListOf<SettingChange>()
        val selectedCommands = mutableListOf<CommandView>()
        val settings = SettingsView(
            models = listOf(
                ModelChoice("opus", "Opus 4", "Most capable", listOf("low", "high"), current = true, unlisted = false, defaultEffort = "high"),
                ModelChoice("sonnet", "Sonnet 4", "Fast and balanced", listOf("low", "high"), current = false, unlisted = false, defaultEffort = "high"),
            ),
            efforts = listOf(
                EffortChoice("low", current = false, default = false, unlisted = false),
                EffortChoice("high", current = true, default = true, unlisted = false),
            ),
            permissions = listOf(
                PermissionChoice("default", "Ask before editing", custom = false, current = true, unlisted = false, normal = true, neverAsks = false, settable = true),
                PermissionChoice("auto", "Auto-approve", custom = false, current = false, unlisted = false, normal = false, neverAsks = true, settable = true),
            ),
            modes = listOf(
                ModeChoice("default", "Default", current = true, unlisted = false, normal = true, settable = true),
                ModeChoice("plan", "Plan", current = false, unlisted = false, normal = false, settable = true),
            ),
            changeable = Changeability(
                model = Changeable.Pick,
                effort = Changeable.Pick,
                permission = Changeable.Pick,
                mode = Changeable.Pick,
            ),
            commands = listOf(
                CommandView("compact", "Compact conversation", "", "builtin", "/", anywhere = false),
            ),
        )

        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                ChatSettingsSheetContent(
                    state = ChatSettingsState(
                        settings = settings,
                        kind = Kind.Codex,
                        context = ContextView(usedTokens = 90_000L, nearFull = false, percent = 45L, windowTokens = 200_000L),
                    ),
                    onChange = { changes += it },
                    onDismiss = {},
                    onSelectCommand = { selectedCommands += it },
                )
            }
        }

        composeRule.onNodeWithTag(AskReviewTags.chatSettingsModel("sonnet")).performScrollTo().performClick()
        assertEquals(SettingChange.Model("sonnet"), changes.last())

        composeRule.onNodeWithTag(AskReviewTags.chatSettingsEffort("low")).performScrollTo().performClick()
        assertEquals(SettingChange.Effort("low"), changes.last())

        composeRule.onNodeWithTag(AskReviewTags.chatSettingsPermission("auto")).performScrollTo().performClick()
        assertEquals(SettingChange.Permission("auto"), changes.last())

        composeRule.onNodeWithTag(AskReviewTags.chatSettingsMode("plan")).performScrollTo().performClick()
        assertEquals(SettingChange.Mode("plan"), changes.last())

        composeRule.onNodeWithTag(AskReviewTags.CHAT_SETTINGS_CONTEXT).performScrollTo().assertIsDisplayed()

        composeRule.onNodeWithTag(AskReviewTags.chatSettingsCommand("compact")).performScrollTo().performClick()
        assertEquals("compact", selectedCommands.last().name)
    }

    @Test
    fun byTypingAndCycleSettingsRenderHintsAndCycleAction() {
        val changes = mutableListOf<SettingChange>()
        val settings = SettingsView(
            models = listOf(
                ModelChoice("sonnet", "Sonnet 4", "", emptyList(), current = true, unlisted = false),
            ),
            efforts = listOf(
                EffortChoice("medium", current = true, default = true, unlisted = false),
            ),
            permissions = listOf(
                PermissionChoice("default", "Ask before editing", custom = false, current = true, unlisted = false, normal = true, neverAsks = false, settable = false),
            ),
            modes = emptyList(),
            changeable = Changeability(
                model = Changeable.ByTyping("/model"),
                effort = Changeable.ByTyping("/effort"),
                permission = Changeable.Cycle,
                mode = Changeable.NotOffered,
            ),
            commands = emptyList(),
        )

        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                ChatSettingsSheetContent(
                    state = ChatSettingsState(settings = settings, kind = Kind.ClaudePty),
                    onChange = { changes += it },
                    onDismiss = {},
                )
            }
        }

        composeRule.onNodeWithTag(AskReviewTags.CHAT_SETTINGS_TYPING_HINT).assertIsDisplayed()
        composeRule.onNodeWithText("To change the model or effort, type /model <name> or /effort <level> in the composer.").assertIsDisplayed()
        composeRule.onNodeWithTag(AskReviewTags.CHAT_SETTINGS_CYCLES_HINT).assertIsDisplayed()

        composeRule.onNodeWithTag(AskReviewTags.CHAT_SETTINGS_CYCLE).performClick()
        assertEquals(SettingChange.CyclePermission, changes.last())
    }
}
