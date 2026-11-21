package sh.amux.features

import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performTextInput
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import sh.amux.design.AmuxTheme
import sh.amux.design.Appearance
import sh.amux.values.AgentKey
import sh.amux.values.Attention
import sh.amux.values.Filing
import sh.amux.values.FleetCard
import sh.amux.values.FleetRow
import sh.amux.values.Kind
import sh.amux.values.Reach
import sh.amux.values.RouteKind
import sh.amux.values.SecondLine

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34], qualifiers = "w412dp-h915dp-xxhdpi")
class SettledScreenTest {
    @get:Rule
    val composeRule = createComposeRule()

    private fun settledRow(
        index: Int,
        name: String,
        host: String = "desk",
        cwd: String = "/Users/ada/source/amux",
        native: Boolean = false,
        phaseSinceMs: Long = 10_000L,
    ): FleetRow = FleetRow(
        card = FleetCard(
            agent = AgentKey(host = List(16) { 1 }, agent = List(16) { index }),
            name = name,
            handle = name,
            kind = Kind.ClaudeSdk,
            attention = Attention.Exited,
            cwd = cwd,
            phaseSinceMs = phaseSinceMs,
            host = host,
            hostReach = Reach.Online(RouteKind.Direct),
            children = 0,
            familyAttention = Attention.Exited,
            members = 1,
            descendants = 0,
            backgroundJobs = 0,
            closeHeld = false,
            filing = Filing.Closed,
            topLevel = true,
            native = native,
        ),
        depth = 0,
        expanded = false,
        secondLine = SecondLine.Blank,
    )

    @Test
    fun rendersEmptyStateWhenNoSettledAgents() {
        val actions = mutableListOf<SettledAction>()
        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                SettledScreen(
                    state = SettledScreenState(),
                    onAction = { actions += it },
                )
            }
        }

        composeRule.onNodeWithTag(SettledTags.EMPTY).assertIsDisplayed()
        composeRule.onNodeWithTag(SettledTags.SEARCH_FIELD).assertDoesNotExist()
        composeRule.onNodeWithTag(SettledTags.BACK).performClick()
        assertEquals(SettledAction.Back, actions.single())
    }

    @Test
    fun rendersListOpensRowAndSupportsUnsettleAndConfirmedDelete() {
        val first = settledRow(1, "refactor-auth", host = "desk", cwd = "/Users/ada/source/amux")
        val second = settledRow(2, "subagent-helper", host = "studio", native = true)
        val actions = mutableListOf<SettledAction>()

        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                SettledScreen(
                    state = SettledScreenState(
                        rows = listOf(first, second),
                        nowMs = 190_000L,
                    ),
                    onAction = { actions += it },
                )
            }
        }

        composeRule.onNodeWithTag(SettledTags.SUBTITLE).assertIsDisplayed()
        composeRule.onNodeWithText("2 agents").assertIsDisplayed()
        composeRule.onNodeWithTag(SettledTags.row("refactor-auth")).assertIsDisplayed()
        composeRule.onNodeWithTag(SettledTags.row("subagent-helper")).assertIsDisplayed()

        // Tap row opens conversation.
        composeRule.onNodeWithTag(SettledTags.row("refactor-auth")).performClick()
        assertEquals(SettledAction.Open(first.card.agent), actions.last())

        // Overflow menu -> Un-settle.
        composeRule.onNodeWithTag(SettledTags.rowMenu("refactor-auth")).performClick()
        composeRule.onNodeWithTag(SettledTags.ACTION_SHEET).assertIsDisplayed()
        composeRule.onNodeWithTag(SettledTags.ACTION_UNSETTLE).performClick()
        assertEquals(SettledAction.Unsettle(first.card.agent), actions.last())

        // Overflow menu -> Delete opens confirmation dialog and confirms delete.
        composeRule.onNodeWithTag(SettledTags.rowMenu("refactor-auth")).performClick()
        composeRule.onNodeWithTag(SettledTags.ACTION_DELETE).performClick()
        composeRule.onNodeWithTag(SettledTags.DELETE_DIALOG).assertIsDisplayed()
        composeRule.onNodeWithTag(SettledTags.DELETE_CONFIRM).performClick()
        assertEquals(SettledAction.Delete(first.card.agent, discardingChanges = false), actions.last())

        // Native subagent hides Delete Agent action.
        composeRule.onNodeWithTag(SettledTags.rowMenu("subagent-helper")).performClick()
        composeRule.onNodeWithTag(SettledTags.ACTION_UNSETTLE).assertIsDisplayed()
        composeRule.onNodeWithTag(SettledTags.ACTION_DELETE).assertDoesNotExist()
    }

    @Test
    fun filtersListAndShowsNoMatchesStateWithClearAction() {
        val first = settledRow(1, "refactor-auth", host = "desk")
        val actions = mutableListOf<SettledAction>()

        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Dark, photographed = true) {
                SettledScreen(
                    state = SettledScreenState(
                        rows = listOf(first),
                        query = "nomatch",
                        nowMs = 190_000L,
                    ),
                    onAction = { actions += it },
                )
            }
        }

        composeRule.onNodeWithTag(SettledTags.NO_MATCHES).assertIsDisplayed()
        composeRule.onNodeWithTag(SettledTags.NO_MATCHES_CLEAR).performClick()
        assertEquals(SettledAction.UpdateQuery(""), actions.last())

        composeRule.onNodeWithTag(SettledTags.SEARCH_FIELD).performTextInput("desk")
        assertEquals(true, actions.any { it is SettledAction.UpdateQuery })
    }
}
