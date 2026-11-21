package sh.amux.features

import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performScrollTo
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
import sh.amux.values.BackgroundKind
import sh.amux.values.ChangeTotals
import sh.amux.values.ChangedFile
import sh.amux.values.Changes
import sh.amux.values.Comparison
import sh.amux.values.ContextView
import sh.amux.values.FileStatus
import sh.amux.values.Folder
import sh.amux.values.JobRow
import sh.amux.values.JobStop
import sh.amux.values.Overview
import sh.amux.values.ServerView
import sh.amux.values.TaskLine
import sh.amux.values.TaskMark
import sh.amux.values.TasksView
import sh.amux.values.UsageLabel
import sh.amux.values.UsageState
import sh.amux.values.UsageView
import sh.amux.values.UsageWindowView

@RunWith(RobolectricTestRunner::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
@Config(sdk = [34], qualifiers = "w412dp-h915dp-xxhdpi")
class ChatOverviewSheetTest {
    @get:Rule
    val composeRule = createComposeRule()

    private val nowMs = 1_750_000_000_000L

    private fun sampleState(): ChatOverviewState = ChatOverviewState(
        subject = ChatSubjectUi(
            name = "refactor-auth",
            host = "studio",
            directory = "/Users/ada/source/amux",
            branch = "android",
            address = "studio/refactor-auth",
        ),
        overview = Overview(
            tasks = TasksView(
                done = 3,
                total = 7,
                current = "Updating the pairing copy",
                entries = listOf(
                    TaskLine("Audit strings", TaskMark.Done),
                    TaskLine("Updating the pairing copy", TaskMark.Current),
                    TaskLine("Run tests", TaskMark.Todo),
                ),
            ),
            jobs = listOf(
                JobRow(
                    command = "cargo test -p store",
                    startedAtMs = nowMs - 45_000L,
                    kind = BackgroundKind.Shell,
                    stop = JobStop.Offered("job-1"),
                ),
                JobRow(
                    command = "npm run dev",
                    startedAtMs = nowMs - 120_000L,
                    kind = BackgroundKind.Shell,
                    stop = JobStop.Stopping,
                ),
            ),
            failedServers = listOf(
                ServerView(name = "github", error = "token expired", needsAuth = true),
            ),
            usageNearLimit = UsageView(
                blocked = false,
                windows = listOf(
                    UsageWindowView(
                        label = UsageLabel.FiveHour,
                        state = UsageState.NearLimit,
                        resetsAtMs = nowMs + 3_600_000L,
                        usedPercent = 88.0,
                    ),
                ),
                credits = "$4.20 left",
            ),
            changes = Changes(
                totals = ChangeTotals(files = 2, added = 25, removed = 8),
                folders = listOf(
                    Folder(
                        path = "",
                        files = listOf(
                            ChangedFile("README.md", "README.md", added = 5, removed = 1, status = FileStatus.Modified, binary = false),
                        ),
                    ),
                    Folder(
                        path = "src/auth/",
                        files = listOf(
                            ChangedFile("src/auth/token.rs", "token.rs", added = 20, removed = 7, status = FileStatus.Modified, binary = false),
                        ),
                    ),
                ),
            ),
        ),
        context = ContextView(
            usedTokens = 168_000L,
            nearFull = true,
            percent = 84L,
            windowTokens = 200_000L,
        ),
        comparisons = listOf(Comparison.Uncommitted, Comparison.OnBranch),
        comparison = Comparison.Uncommitted,
        baseBranch = "main",
        nowMs = nowMs,
    )

    @Test
    fun overviewRendersTasksJobsStopConfirmationServersUsageAndChangedFiles() {
        val actions = mutableListOf<ChatOverviewAction>()
        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                ChatOverviewSheetContent(
                    state = sampleState(),
                    onAction = { actions += it },
                )
            }
        }

        // Tasks
        composeRule.onNodeWithTag(AskReviewTags.OVERVIEW_TASKS).assertIsDisplayed()
        composeRule.onNodeWithText("3/7").assertIsDisplayed()
        composeRule.onNodeWithText("Updating the pairing copy").assertIsDisplayed()

        // Background jobs & stop confirmation AlertDialog
        composeRule.onNodeWithTag(AskReviewTags.overviewJob(0)).performScrollTo().assertIsDisplayed()
        composeRule.onNodeWithText("Stopping…").performScrollTo().assertIsDisplayed()
        composeRule.onNodeWithTag(AskReviewTags.overviewJobStop(0)).performScrollTo().performClick()
        composeRule.onNodeWithTag(AskReviewTags.OVERVIEW_STOP_DIALOG).assertIsDisplayed()
        assertTrue("stop must not fire before confirmation", actions.isEmpty())
        composeRule.onNodeWithTag(AskReviewTags.OVERVIEW_STOP_CONFIRM).performClick()
        assertEquals(ChatOverviewAction.StopJob("job-1"), actions.last())

        // Failed tool server
        composeRule.onNodeWithTag(AskReviewTags.overviewServer("github")).performScrollTo().assertIsDisplayed()
        composeRule.onNodeWithText("github needs sign-in").assertIsDisplayed()

        // Usage warning
        composeRule.onNodeWithTag(AskReviewTags.OVERVIEW_USAGE).performScrollTo().assertIsDisplayed()
        composeRule.onNodeWithText("88% used").assertIsDisplayed()

        // Context
        composeRule.onNodeWithTag(AskReviewTags.OVERVIEW_CONTEXT).performScrollTo().assertIsDisplayed()

        // Changed files tap opens review
        composeRule.onNodeWithTag(AskReviewTags.overviewFile("src/auth/token.rs")).performScrollTo().performClick()
        assertEquals(ChatOverviewAction.OpenReview("src/auth/token.rs"), actions.last())
    }

    @Test
    fun overviewStripShowsWhenActiveAndDispatchesCallbacks() {
        var openedOverview = false
        var openedReview = false
        val state = sampleState()

        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                ChatOverviewStrip(
                    overview = state.overview,
                    context = state.context,
                    clock = ElapsedClock { nowMs },
                    onOpenOverview = { openedOverview = true },
                    onOpenReview = { openedReview = true },
                )
            }
        }

        composeRule.onNodeWithTag(AskReviewTags.OVERVIEW_STRIP).assertIsDisplayed()
        composeRule.onNodeWithText("Updating the pairing copy").performClick()
        assertTrue(openedOverview)

        composeRule.onNodeWithTag(AskReviewTags.OVERVIEW_STRIP_CHANGES).performClick()
        assertTrue(openedReview)
    }

    @Test
    fun overviewStripHidesWhenNothingIsActive() {
        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                ChatOverviewStrip(
                    overview = Overview(jobs = emptyList(), failedServers = emptyList()),
                    context = ContextView(usedTokens = 20_000L, nearFull = false, percent = 10L, windowTokens = 200_000L),
                    onOpenOverview = {},
                )
            }
        }

        composeRule.onNodeWithTag(AskReviewTags.OVERVIEW_STRIP).assertDoesNotExist()
    }

    @Test
    fun mismatchedComparisonHidesStaleFilesAndErrorOffersRetry() {
        val actions = mutableListOf<ChatOverviewAction>()
        val mismatched = sampleState().copy(
            comparison = Comparison.OnBranch,
            changesComparison = Comparison.Uncommitted,
            changesLoading = true,
        )

        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                ChatOverviewSheetContent(
                    state = mismatched,
                    onAction = { actions += it },
                )
            }
        }

        composeRule.onNodeWithTag(AskReviewTags.OVERVIEW_CHANGES_LOADING).performScrollTo().assertIsDisplayed()
        composeRule.onNodeWithTag(AskReviewTags.overviewFile("src/auth/token.rs")).assertDoesNotExist()
    }

    @Test
    fun failedFileListingShowsErrorAndEmitsRetry() {
        val actions = mutableListOf<ChatOverviewAction>()
        val failed = sampleState().copy(
            comparison = Comparison.OnBranch,
            changesComparison = Comparison.Uncommitted,
            changesLoading = false,
            changesError = "host away",
        )

        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                ChatOverviewSheetContent(
                    state = failed,
                    onAction = { actions += it },
                )
            }
        }

        composeRule.onNodeWithTag(AskReviewTags.OVERVIEW_CHANGES_ERROR).performScrollTo().assertIsDisplayed()
        composeRule.onNodeWithTag(AskReviewTags.OVERVIEW_CHANGES_RETRY).performScrollTo().performClick()
        assertEquals(listOf(ChatOverviewAction.RetryChanges), actions)
    }
}
