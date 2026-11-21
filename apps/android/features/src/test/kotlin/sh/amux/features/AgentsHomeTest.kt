package sh.amux.features

import androidx.compose.ui.semantics.SemanticsActions
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.hasClickAction
import androidx.compose.ui.test.hasContentDescription
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.longClick
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performSemanticsAction
import androidx.compose.ui.test.performTouchInput
import androidx.compose.ui.text.AnnotatedString
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode
import sh.amux.design.AmuxTheme
import sh.amux.design.Appearance
import sh.amux.push.Said
import sh.amux.push.Wants
import sh.amux.values.Activity
import sh.amux.values.ActivityKind
import sh.amux.values.ActivityLine
import sh.amux.values.AgentKey
import sh.amux.values.Attention
import sh.amux.values.Filing
import sh.amux.values.FleetCard
import sh.amux.values.FleetFilter
import sh.amux.values.FleetOnly
import sh.amux.values.FleetRow
import sh.amux.values.FleetSection
import sh.amux.values.HostView
import sh.amux.values.Kind
import sh.amux.values.Platform
import sh.amux.values.Reach
import sh.amux.values.RouteKind
import sh.amux.values.SecondLine
import sh.amux.values.SectionKind

@RunWith(RobolectricTestRunner::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
@Config(sdk = [34], qualifiers = "w412dp-h915dp-xxhdpi")
class AgentsHomeTest {
    @get:Rule
    val composeRule = createComposeRule()

    private val nowMs = 1_750_000_000_000L

    private fun host(index: Int, name: String, trusted: Boolean = true, candidate: Boolean = false, pairingOpen: Boolean = false): HostView =
        HostView(
            hostId = List(16) { index },
            link = 0L,
            sameNetwork = false,
            name = name,
            local = false,
            trusted = trusted,
            candidate = candidate,
            reach = Reach.Online(RouteKind.Direct),
            addrs = if (candidate) listOf("192.168.1.$index:4433") else emptyList(),
            relayOnly = false,
            pairingOpen = pairingOpen,
            current = true,
            providers = emptyList(),
            platform = Platform.Macos,
        )

    private fun row(
        index: Int,
        name: String,
        host: HostView,
        attention: Attention,
        filing: Filing = Filing.Open,
        secondLine: SecondLine = SecondLine.Blank,
        children: Int = 0,
    ): FleetRow = FleetRow(
        card = FleetCard(
            agent = AgentKey(host = host.hostId, agent = List(16) { index }),
            name = name,
            handle = name,
            kind = Kind.ClaudeSdk,
            attention = attention,
            cwd = "/Users/ada/source/amux",
            phaseSinceMs = nowMs - 120_000L,
            host = host.name,
            hostReach = host.reach,
            children = children,
            familyAttention = attention,
            members = 1 + children,
            descendants = children,
            backgroundJobs = 0,
            closeHeld = false,
            filing = filing,
            topLevel = true,
            native = false,
        ),
        depth = 0,
        expanded = false,
        secondLine = secondLine,
    )

    private fun sampleBusyState(): Triple<AgentsHomeState, FleetRow, FleetRow> {
        val desk = host(1, "desk")
        val studio = host(2, "studio")
        val askRow = row(
            index = 1,
            name = "release-notes",
            host = desk,
            attention = Attention.NeedsYou,
            secondLine = SecondLine.Said(
                Said.Ask(
                    key = "",
                    wants = Wants.Run("cargo test"),
                    why = "",
                    more = 0,
                ),
            ),
        )
        val workingRow = row(
            index = 2,
            name = "migration",
            host = studio,
            attention = Attention.Working,
            children = 2,
            secondLine = SecondLine.Step(
                ActivityLine(
                    activity = Activity(
                        kind = ActivityKind.Running("t1"),
                        sinceMs = nowMs - 10_000L,
                        elapsedMs = 10_000L,
                        turnElapsedMs = 10_000L,
                    ),
                    step = "cargo test -p store",
                ),
            ),
        )
        val settledRow = row(
            index = 3,
            name = "old-spike",
            host = desk,
            attention = Attention.Exited,
            filing = Filing.Closed,
        )

        val state = AgentsHomeState(
            loaded = true,
            rows = listOf(askRow, workingRow, settledRow),
            sections = listOf(
                FleetSection(SectionKind.NeedsYou, families = 1, foldedByDefault = false, rows = listOf(askRow)),
                FleetSection(SectionKind.Live, families = 1, foldedByDefault = false, rows = listOf(workingRow)),
                FleetSection(SectionKind.Closed, families = 1, foldedByDefault = true, rows = listOf(settledRow)),
            ),
            needYou = 1,
            working = 1,
            machines = listOf(desk, studio),
            orderedAtMs = nowMs,
            accounts = listOf(
                AccountUi("a1", "Ada", "ada@example.com", "2 hosts", signedIn = true, selected = true),
            ),
            selectedAccount = AccountUi("a1", "Ada", "ada@example.com", "2 hosts", signedIn = true, selected = true),
        )
        return Triple(state, askRow, workingRow)
    }

    @Test
    fun busyFleetShowsSectionsAndDispatchesTapAndFilter() {
        val (state, askRow, workingRow) = sampleBusyState()
        val actions = mutableListOf<HomeAction>()
        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                AgentsHome(
                    state = state,
                    onAction = { actions += it },
                    searchExpanded = true,
                )
            }
        }

        composeRule.onNodeWithTag(TestTags.HOME_FAB_NEW_AGENT).assertIsDisplayed()
        composeRule.onNodeWithTag(TestTags.homeRow("release-notes")).assertIsDisplayed()
        composeRule.onNodeWithTag(TestTags.HOME_SETTLED_ROW).assertIsDisplayed()

        // Tap a row to open conversation
        composeRule.onNodeWithTag(TestTags.homeRow("release-notes")).performClick()
        assertEquals(HomeAction.Open(askRow.card.agent), actions.last())

        // Tap family toggle
        composeRule.onNodeWithTag(TestTags.homeFamily("migration")).performClick()
        assertEquals(HomeAction.ToggleFamily(workingRow.card.agent), actions.last())

        // Tap filter chip
        composeRule.onNodeWithTag(TestTags.homeFilter("needs-you")).performClick()
        assertEquals(HomeAction.Filter(FleetFilter(words = "", only = FleetOnly.NeedsYou)), actions.last())
    }

    @Test
    fun rowLongPressAndTalkBackActionsDispatchPinAndRename() {
        val (state, askRow, _) = sampleBusyState()
        val actions = mutableListOf<HomeAction>()
        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                AgentsHome(
                    state = state,
                    onAction = { actions += it },
                )
            }
        }

        composeRule.onNodeWithTag(TestTags.homeRow("release-notes")).performTouchInput { longClick() }
        composeRule.onNodeWithTag(TestTags.HOME_ROW_ACTIONS_SHEET).assertIsDisplayed()
        composeRule.onNodeWithTag(TestTags.HOME_ACTION_PIN).performClick()
        assertEquals(HomeAction.Pin(askRow.card.agent), actions.last())

        // TalkBack reaches the same actions on the row, with no sheet.
        val rename = composeRule.onNode(hasContentDescription("release-notes", substring = true) and hasClickAction())
            .fetchSemanticsNode().config[SemanticsActions.CustomActions]
            .single { it.label == "Rename" }
        composeRule.runOnUiThread { rename.action() }
        composeRule.onNodeWithTag(TestTags.HOME_RENAME_INPUT).performSemanticsAction(SemanticsActions.SetText) {
            it(AnnotatedString("release-notes-v2"))
        }
        composeRule.onNodeWithTag(TestTags.HOME_RENAME_CONFIRM).performClick()
        assertEquals(HomeAction.Rename(askRow.card.agent, "release-notes-v2"), actions.last())
    }

    @Test
    fun longPressWorkingRowPromptsBeforeSettling() {
        val (state, _, workingRow) = sampleBusyState()
        val actions = mutableListOf<HomeAction>()
        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                AgentsHome(
                    state = state,
                    onAction = { actions += it },
                )
            }
        }

        composeRule.onNodeWithTag(TestTags.homeRow("migration")).performTouchInput { longClick() }
        composeRule.onNodeWithTag(TestTags.HOME_ACTION_SETTLE).performClick()
        composeRule.onNodeWithTag(TestTags.HOME_SETTLE_CONFIRM).assertIsDisplayed().performClick()
        assertEquals(HomeAction.Settle(workingRow.card.agent), actions.last())
    }

    @Test
    fun firstRunHidesNewAgentFabAndOffersNearbyPair() {
        val laptop = host(4, "laptop", trusted = false, candidate = true, pairingOpen = true)
        val state = AgentsHomeState(
            loaded = true,
            machines = emptyList(),
            discoveredHosts = listOf(laptop),
            orderedAtMs = nowMs,
        )
        val actions = mutableListOf<HomeAction>()
        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                AgentsHome(state = state, onAction = { actions += it })
            }
        }

        composeRule.onNodeWithTag(TestTags.HOME_EMPTY_FIRST_RUN).assertIsDisplayed()
        composeRule.onNodeWithTag(TestTags.HOME_FAB_NEW_AGENT).assertDoesNotExist()
        composeRule.onNodeWithTag(TestTags.homePair("laptop")).assertIsDisplayed().performClick()
        assertEquals(HomeAction.Pair(laptop.hostId), actions.last())
    }

    @Test
    fun noAgentsYetShowsNewAgentPrimaryAction() {
        val desk = host(1, "desk")
        val state = AgentsHomeState(
            loaded = true,
            machines = listOf(desk),
            orderedAtMs = nowMs,
        )
        val actions = mutableListOf<HomeAction>()
        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                AgentsHome(state = state, onAction = { actions += it })
            }
        }

        composeRule.onNodeWithTag(TestTags.HOME_EMPTY_NO_AGENTS).assertIsDisplayed()
        composeRule.onNodeWithTag(TestTags.HOME_FAB_NEW_AGENT).assertDoesNotExist()
        composeRule.onNodeWithTag(TestTags.HOME_EMPTY_NEW_AGENT).performClick()
        assertEquals(HomeAction.NewAgent, actions.last())
        composeRule.onNodeWithText("No agents yet").assertIsDisplayed()
    }
}
