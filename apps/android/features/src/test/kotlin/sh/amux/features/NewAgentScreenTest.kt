package sh.amux.features

import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertIsEnabled
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performScrollTo
import androidx.compose.ui.test.performTextInput
import androidx.compose.ui.unit.Density
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import sh.amux.design.AmuxTheme
import sh.amux.design.Appearance
import sh.amux.values.AgentNameProblem
import sh.amux.values.Changeability
import sh.amux.values.Changeable
import sh.amux.values.Directory
import sh.amux.values.EffortChoice
import sh.amux.values.HostView
import sh.amux.values.ModeChoice
import sh.amux.values.ModelChoice
import sh.amux.values.NewAgentPick
import sh.amux.values.Offline
import sh.amux.values.PermissionChoice
import sh.amux.values.Platform
import sh.amux.values.Provider
import sh.amux.values.ProviderSignIn
import sh.amux.values.Reach
import sh.amux.values.RouteKind
import sh.amux.values.SettingsView

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34], qualifiers = "w412dp-h915dp-xxhdpi")
class NewAgentScreenTest {
    @get:Rule
    val composeRule = createComposeRule()

    private fun host(
        index: Int,
        name: String,
        reach: Reach = Reach.Online(RouteKind.Direct),
        providers: List<ProviderSignIn> = listOf(
            ProviderSignIn(Provider.Claude, signedIn = true),
            ProviderSignIn(Provider.Codex, signedIn = true),
        ),
    ): HostView = HostView(
        hostId = List(16) { index },
        name = name,
        local = false,
        trusted = true,
        candidate = false,
        reach = reach,
        addrs = emptyList(),
        relayOnly = false,
        pairingOpen = false,
        current = true,
        providers = providers,
        platform = Platform.Macos,
    )

    private val sampleSettings = SettingsView(
        models = listOf(
            ModelChoice(
                value = "gpt-6-astra",
                displayName = "GPT-6-Astra",
                description = "Fast reasoning",
                efforts = listOf("low", "high"),
                current = true,
                unlisted = false,
                defaultEffort = "low",
            ),
            ModelChoice(
                value = "gpt-6-sol",
                displayName = "GPT-6-Sol",
                description = "Compact model",
                efforts = listOf("low"),
                current = false,
                unlisted = false,
                defaultEffort = "low",
            ),
        ),
        efforts = listOf(
            EffortChoice(value = "low", current = true, default = true, unlisted = false),
            EffortChoice(value = "high", current = false, default = false, unlisted = false),
        ),
        permissions = listOf(
            PermissionChoice(
                value = "default",
                displayName = "Default",
                custom = false,
                current = true,
                unlisted = false,
                normal = true,
                neverAsks = false,
                settable = true,
            ),
            PermissionChoice(
                value = "auto",
                displayName = "Auto",
                custom = false,
                current = false,
                unlisted = false,
                normal = false,
                neverAsks = true,
                settable = true,
            ),
        ),
        modes = listOf(
            ModeChoice(value = "default", displayName = "Default", current = true, unlisted = false, normal = true, settable = true),
            ModeChoice(value = "plan", displayName = "Plan", current = false, unlisted = false, normal = false, settable = true),
        ),
        changeable = Changeability(
            model = Changeable.Pick,
            effort = Changeable.Pick,
            permission = Changeable.Pick,
            mode = Changeable.Pick,
        ),
        commands = emptyList(),
    )

    @Test
    fun rendersNoHostsStateWithDisabledStartButton() {
        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                NewAgentScreen(
                    state = NewAgentScreenState(hosts = emptyList()),
                    onAction = {},
                )
            }
        }

        composeRule.onNodeWithTag(NewAgentTags.NO_HOSTS).assertIsDisplayed()
        composeRule.onNodeWithTag(NewAgentTags.START_BUTTON).assertIsNotEnabled()
    }

    @Test
    fun switchesHostProviderAndPickersAndSubmitsWhenReady() {
        val desk = host(1, "desk")
        val studio = host(2, "studio")
        val offline = host(3, "mini", reach = Reach.Offline(Offline.Plain))
        val actions = mutableListOf<NewAgentAction>()

        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                NewAgentScreen(
                    state = NewAgentScreenState(
                        hosts = listOf(desk, studio, offline),
                        selectedHostId = desk.hostId,
                        provider = Provider.Codex,
                        offer = NewAgentOfferUi.Offered,
                        settings = sampleSettings,
                        listing = NewAgentListingUi.Ready,
                        repositories = listOf(Directory("/Users/ada/source/amux", "amux")),
                        recent = listOf(Directory("/Users/ada/source/web", "web")),
                        directory = "/Users/ada/source/amux",
                    ),
                    onAction = { actions += it },
                )
            }
        }

        // Start button is enabled and names the selected host.
        composeRule.onNodeWithTag(NewAgentTags.START_BUTTON).assertIsEnabled()
        composeRule.onNodeWithText("Start on desk").assertIsDisplayed()
        composeRule.onNodeWithTag(NewAgentTags.START_BUTTON).performClick()
        assertEquals(NewAgentAction.Start, actions.last())

        // Switch host via host picker bottom sheet; offline host is disabled.
        composeRule.onNodeWithTag(NewAgentTags.HOST_ROW).performClick()
        composeRule.onNodeWithTag(NewAgentTags.HOST_SHEET).assertIsDisplayed()
        composeRule.onNodeWithTag(NewAgentTags.hostOption("mini")).assertIsNotEnabled()
        composeRule.onNodeWithTag(NewAgentTags.hostOption("studio")).performClick()
        assertEquals(NewAgentAction.SelectHost(studio.hostId), actions.last())

        // Switch provider chip.
        composeRule.onNodeWithTag(NewAgentTags.providerChip(Provider.Claude)).performClick()
        assertEquals(NewAgentAction.SelectProvider(Provider.Claude), actions.last())

        // Pick model via M3 bottom sheet picker.
        composeRule.onNodeWithTag(NewAgentTags.MODEL_ROW).performScrollTo().performClick()
        composeRule.onNodeWithTag(NewAgentTags.PICKER_SHEET).assertIsDisplayed()
        composeRule.onNodeWithTag(NewAgentTags.pickerOption("gpt-6-sol")).performClick()
        assertEquals(NewAgentAction.PickSetting(NewAgentPick.Model("gpt-6-sol")), actions.last())

        // Pick effort.
        composeRule.onNodeWithTag(NewAgentTags.EFFORT_ROW).performScrollTo().performClick()
        composeRule.onNodeWithTag(NewAgentTags.pickerOption("high")).performClick()
        assertEquals(NewAgentAction.PickSetting(NewAgentPick.Effort("high")), actions.last())

        // Pick permission.
        composeRule.onNodeWithTag(NewAgentTags.PERMISSION_ROW).performScrollTo().performClick()
        composeRule.onNodeWithTag(NewAgentTags.pickerOption("auto")).performClick()
        assertEquals(NewAgentAction.PickSetting(NewAgentPick.Permission("auto")), actions.last())

        // Pick mode.
        composeRule.onNodeWithTag(NewAgentTags.MODE_ROW).performScrollTo().performClick()
        composeRule.onNodeWithTag(NewAgentTags.pickerOption("plan")).performClick()
        assertEquals(NewAgentAction.PickSetting(NewAgentPick.Mode("plan")), actions.last())

        // Toggle worktree.
        composeRule.onNodeWithTag(NewAgentTags.WORKTREE_ROW).performScrollTo().performClick()
        assertEquals(NewAgentAction.SetNewWorktree(true), actions.last())
    }

    @Test
    fun folderBrowserSupportsSearchPinningSelectionAndTypedPath() {
        val desk = host(1, "desk")
        val amuxRepo = Directory("/Users/ada/source/amux", "amux")
        val scratchDir = Directory("/Users/ada/scratch", "scratch")
        val actions = mutableListOf<NewAgentAction>()

        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                NewAgentDirectoriesContent(
                    state = NewAgentScreenState(
                        hosts = listOf(desk),
                        selectedHostId = desk.hostId,
                        listing = NewAgentListingUi.Ready,
                        repositories = listOf(amuxRepo),
                        recent = listOf(scratchDir),
                        directory = amuxRepo.path,
                        typedPath = "~/other/project",
                        browsing = true,
                    ),
                    onAction = { actions += it },
                )
            }
        }

        composeRule.onNodeWithTag(NewAgentTags.BROWSE_SHEET).assertIsDisplayed()
        composeRule.onNodeWithTag(NewAgentTags.browseDirectory("amux")).assertIsDisplayed()
        composeRule.onNodeWithTag(NewAgentTags.browseDirectory("scratch")).assertIsDisplayed()

        // Pin recent folder.
        composeRule.onNodeWithTag(NewAgentTags.browsePin("scratch")).performClick()
        assertEquals(NewAgentAction.PinFolder(scratchDir, pinned = true), actions.last())

        // Unpin repository.
        composeRule.onNodeWithTag(NewAgentTags.browsePin("amux")).performClick()
        assertEquals(NewAgentAction.PinFolder(amuxRepo, pinned = false), actions.last())

        // Search input updates query.
        composeRule.onNodeWithTag(NewAgentTags.BROWSE_SEARCH).performTextInput("scr")
        assertTrue(actions.any { it is NewAgentAction.UpdateQuery })

        // Selecting a folder chooses it and closes browser.
        composeRule.onNodeWithTag(NewAgentTags.browseDirectory("scratch")).performClick()
        assertTrue(actions.contains(NewAgentAction.ChooseDirectory(scratchDir.path)))
        assertEquals(NewAgentAction.SetBrowsing(false), actions.last())

        // Using typed custom path emits ChooseDirectory.
        composeRule.onNodeWithTag(NewAgentTags.BROWSE_TYPED_USE).performScrollTo().performClick()
        assertTrue(actions.contains(NewAgentAction.ChooseDirectory("~/other/project")))
    }

    @Test
    fun validationErrorDisablesStartAndLargeTextRendersCleanly() {
        val desk = host(1, "desk")
        composeRule.setContent {
            val baseDensity = LocalDensity.current
            CompositionLocalProvider(
                LocalDensity provides Density(baseDensity.density, fontScale = 1.5f),
            ) {
                AmuxTheme(appearance = Appearance.Dark, photographed = true) {
                    NewAgentScreen(
                        state = NewAgentScreenState(
                            hosts = listOf(desk),
                            selectedHostId = desk.hostId,
                            offer = NewAgentOfferUi.Offered,
                            settings = sampleSettings,
                            listing = NewAgentListingUi.Ready,
                            directory = "/Users/ada/source/amux",
                            name = "Invalid Name",
                            nameProblem = AgentNameProblem.Characters,
                            failure = "The computer refused that name.",
                        ),
                        onAction = {},
                    )
                }
            }
        }

        composeRule.onNodeWithTag(NewAgentTags.HOST_ROW).assertIsDisplayed()
        composeRule.onNodeWithTag(NewAgentTags.DIRECTORY_ROW).assertIsDisplayed()
        composeRule.onNodeWithTag(NewAgentTags.NAME_ERROR).performScrollTo().assertIsDisplayed()
        composeRule.onNodeWithTag(NewAgentTags.FAILURE).assertIsDisplayed()
        composeRule.onNodeWithTag(NewAgentTags.START_BUTTON).assertIsNotEnabled()
    }
}
