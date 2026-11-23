package sh.amux.features

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.test.assertHasNoClickAction
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.assertTextContains
import androidx.compose.ui.test.hasAnyAncestor
import androidx.compose.ui.test.hasScrollToIndexAction
import androidx.compose.ui.test.hasTestTag
import androidx.compose.ui.test.hasText
import androidx.compose.ui.test.junit4.StateRestorationTester
import androidx.compose.ui.test.performScrollToIndex
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performTextInput
import androidx.compose.ui.test.performTouchInput
import androidx.compose.ui.test.longClick
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
import sh.amux.values.FileStatus
import sh.amux.values.LineKind
import sh.amux.values.ReviewAnchor
import sh.amux.values.ReviewBanner
import sh.amux.values.ReviewCodeLine
import sh.amux.values.ReviewCommentAt
import sh.amux.values.ReviewCommentRow
import sh.amux.values.ReviewCommentState
import sh.amux.values.ReviewContextAsk
import sh.amux.values.ReviewFileHead
import sh.amux.values.ReviewGap
import sh.amux.values.ReviewHeader
import sh.amux.values.ReviewPage
import sh.amux.values.ReviewRow
import sh.amux.values.ReviewRowOf
import sh.amux.values.ReviewView
import sh.amux.values.ReviewViewTab
import sh.amux.values.SentReviewRef

@RunWith(RobolectricTestRunner::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
@Config(sdk = [34], qualifiers = "w412dp-h915dp-xxhdpi")
class ReviewScreenTest {
    @get:Rule
    val composeRule = createComposeRule()

    private val path = "src/lib.rs"
    private val ask = ReviewContextAsk(path = path, commit = "5555", start = 1, end = 9, definition = null)
    private val anchor = ReviewAnchor(commit = "5555", path = path, start = 10, end = 11)
    private val draft = ReviewCommentRow(
        id = listOf(1), path = path, text = "Say why.", state = ReviewCommentState.Draft,
        at = ReviewCommentAt.Lines(start = 11, end = 11), changedNear = false, scope = emptyList(),
        snippet = emptyList(), folded = false,
    )

    private fun line(number: Int, kind: LineKind = LineKind.Added): ReviewRow = ReviewRow(
        key = "l/$path/0/$number",
        of = ReviewRowOf.Line(
            ReviewCodeLine(
                path = path, kind = kind, text = "let x = $number;", runs = emptyList(), commented = false,
                extra = false, newLine = number, oldLine = null,
            ),
        ),
    )

    private fun header(views: List<ReviewViewTab> = listOf(ReviewViewTab(ReviewView.All, 1)), drafts: Int = 1) =
        ReviewHeader(
            baseBranch = "main", baseCommit = "bbbb", snapshot = "5555", view = ReviewView.All, comparedWith = "bbbb",
            comparedBase = "", views = views, rounds = views.size - 1, files = 1, added = 2, removed = 0,
            drafts = drafts, sent = 0, discardDrafts = drafts > 0, startOver = true,
        )

    private fun page(
        banners: List<ReviewBanner> = emptyList(),
        header: ReviewHeader? = header(),
    ): ReviewPage = ReviewPage(
        banners = banners,
        rows = listOf(
            ReviewRow(
                key = "f/$path",
                of = ReviewRowOf.File(
                    ReviewFileHead(
                        path = path, status = FileStatus.Modified, added = 2, removed = 0, binary = false,
                        language = "rust", flags = emptyList(), folded = false, changedSinceLast = false,
                        inChange = true, comments = 1, outline = emptyList(), oldPath = null,
                    ),
                ),
            ),
            ReviewRow(key = "g/$path/1", of = ReviewRowOf.Gap(ReviewGap(path = path, loading = false, down = null, hidden = 9, up = ask))),
            line(10),
            line(11),
            ReviewRow(key = "c/01", of = ReviewRowOf.Comment(draft)),
        ),
        toFetch = emptyList(),
        opening = false,
        empty = null,
        header = header,
    )

    private fun show(state: ReviewScreenState, actions: MutableList<ReviewAction>) {
        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                ReviewScreen(state = state, onAction = { actions += it })
            }
        }
    }

    @Test
    fun aTapSelectsALineAndAnotherInItsFileStretchesTheSelection() {
        val actions = mutableListOf<ReviewAction>()
        var state by mutableStateOf(ReviewScreenState(page = page()))
        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                ReviewScreen(state = state, onAction = { actions += it })
            }
        }
        composeRule.onNodeWithTag(ReviewTags.line("l/$path/0/10")).performClick()
        assertEquals(ReviewAction.Select(2), actions.last())

        state = state.copy(selected = 2..2, anchor = anchor.copy(end = 10))
        composeRule.onNodeWithTag(ReviewTags.line("l/$path/0/11")).performClick()
        assertEquals(ReviewAction.Extend(3), actions.last())

        // The selection's foot offers the comment, and Cancel ends it.
        state = state.copy(selected = 2..3, anchor = anchor)
        composeRule.onNodeWithText("Comment on 2 lines").assertIsDisplayed()
        composeRule.onNodeWithTag(ReviewTags.DESELECT).performClick()
        assertEquals(ReviewAction.ClearSelection, actions.last())
    }

    @Test
    fun aCommentIsWrittenInTheSheetOnTheSelection() {
        val actions = mutableListOf<ReviewAction>()
        show(ReviewScreenState(page = page(), selected = 2..3, anchor = anchor), actions)
        composeRule.onNodeWithTag(ReviewTags.COMMENT_ON).performClick()
        composeRule.onNodeWithTag(ReviewTags.ADD).assertIsNotEnabled()
        composeRule.onNodeWithTag(ReviewTags.NOTE).performTextInput("  Name it.  ")
        composeRule.onNodeWithTag(ReviewTags.ADD).performClick()
        assertEquals(ReviewAction.Comment("Name it."), actions.last())
    }

    /** A selection with no lines a comment can take, such as one across files, says so and offers nothing. */
    @Test
    fun aSelectionWithoutLinesOffersNoComment() {
        val actions = mutableListOf<ReviewAction>()
        show(ReviewScreenState(page = page(), selected = 0..3, anchor = null), actions)
        composeRule.onNodeWithText("Select lines in one file").assertIsDisplayed()
        composeRule.onNodeWithTag(ReviewTags.COMMENT_ON).performClick()
        assertTrue(actions.none { it is ReviewAction.Comment })
    }

    @Test
    fun aCommentOpensToBeRewordedOrDeleted() {
        val actions = mutableListOf<ReviewAction>()
        show(ReviewScreenState(page = page()), actions)
        composeRule.onNodeWithTag(ReviewTags.COMMENT).performClick()
        composeRule.onNodeWithTag(ReviewTags.SAVE).assertIsDisplayed()
        composeRule.onNodeWithTag(ReviewTags.NOTE).performTextInput(" More.")
        composeRule.onNodeWithTag(ReviewTags.SAVE).performClick()
        assertEquals(ReviewAction.Reword(listOf(1), "Say why. More."), actions.last())

        composeRule.onNodeWithTag(ReviewTags.COMMENT).performClick()
        composeRule.onNodeWithTag(ReviewTags.DELETE).performClick()
        assertEquals(ReviewAction.Delete(listOf(1)), actions.last())
    }

    @Test
    fun theFileFoldsAndItsGapLoadsMore() {
        val actions = mutableListOf<ReviewAction>()
        show(ReviewScreenState(page = page()), actions)
        composeRule.onNodeWithTag(ReviewTags.FILE).performClick()
        assertEquals(ReviewAction.ToggleFile(path), actions.last())
        composeRule.onNodeWithTag(ReviewTags.GAP_UP).performClick()
        assertEquals(ReviewAction.MoreContext(ask), actions.last())

    }

    /** Held, a file's head offers a comment on the whole file, written once the head is what is selected. */
    @Test
    fun aHeldFileHeadOffersACommentOnTheWholeFile() {
        val actions = mutableListOf<ReviewAction>()
        var state by mutableStateOf(ReviewScreenState(page = page(), selected = 2..3, anchor = anchor))
        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                ReviewScreen(
                    state = state,
                    onAction = { action ->
                        actions += action
                        if (action is ReviewAction.Select) {
                            state = state.copy(selected = action.row..action.row, anchor = anchor.copy(start = 0, end = 0))
                        }
                    },
                )
            }
        }
        composeRule.onNodeWithTag(ReviewTags.FILE).performTouchInput { longClick() }
        composeRule.onNodeWithTag(ReviewTags.FILE_COMMENT).performClick()
        composeRule.waitForIdle()
        assertEquals(listOf<ReviewAction>(ReviewAction.Select(0)), actions)
        composeRule.onNodeWithTag(ReviewTags.SHEET).assertIsDisplayed()
        composeRule.onNode(hasText("Comment on the File") and hasAnyAncestor(hasTestTag(ReviewTags.SHEET))).assertIsDisplayed()
    }

    /** The words in the sheet stay while the page's rows move under it. */
    @Test
    fun theSheetKeepsItsWordsAsThePageMoves() {
        val actions = mutableListOf<ReviewAction>()
        var state by mutableStateOf(ReviewScreenState(page = page(), selected = 2..3, anchor = anchor))
        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                ReviewScreen(state = state, onAction = { actions += it })
            }
        }
        composeRule.onNodeWithTag(ReviewTags.COMMENT_ON).performClick()
        composeRule.onNodeWithTag(ReviewTags.NOTE).performTextInput("Name it.")

        // A snapshot lands with a row above the selection: its rows move down one.
        val moved = page().let { it.copy(rows = listOf(it.rows[0], line(9, LineKind.Context)) + it.rows.drop(1)) }
        state = state.copy(page = moved, selected = 3..4)
        composeRule.waitForIdle()
        composeRule.onNodeWithTag(ReviewTags.NOTE).assertTextContains("Name it.")
        composeRule.onNodeWithTag(ReviewTags.ADD).performClick()
        assertEquals(ReviewAction.Comment("Name it."), actions.last())
        assertTrue("nothing was selected again", actions.none { it is ReviewAction.Select })
    }

    /**
     * The activity made again (a turn of the phone) starts the runtime's
     * selection afresh: the saved sheet selects its lines again, with its
     * words, once the page holds them, and closes on a page that does not.
     */
    @Test
    fun theSheetComesBackOverItsLinesAfterTheActivityIsMadeAgain() {
        val actions = mutableListOf<ReviewAction>()
        var state by mutableStateOf(ReviewScreenState(page = page(), selected = 2..3, anchor = anchor))
        val restoration = StateRestorationTester(composeRule)
        restoration.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                ReviewScreen(state = state, onAction = { actions += it })
            }
        }
        composeRule.onNodeWithTag(ReviewTags.COMMENT_ON).performClick()
        composeRule.onNodeWithTag(ReviewTags.NOTE).performTextInput("Name it.")

        // Made again, the runtime's session holds no selection and no page yet.
        state = ReviewScreenState(page = page(header = null).copy(rows = emptyList()))
        restoration.emulateSavedInstanceStateRestore()
        composeRule.onNodeWithTag(ReviewTags.SHEET).assertDoesNotExist()
        actions.clear()

        // The page arrives: the sheet's lines are selected again, and it shows.
        state = ReviewScreenState(page = page())
        composeRule.waitForIdle()
        assertEquals(listOf(ReviewAction.Select(2), ReviewAction.Extend(3)), actions)
        state = state.copy(selected = 2..3, anchor = anchor)
        composeRule.waitForIdle()
        composeRule.onNode(hasText("Comment on 2 lines") and hasAnyAncestor(hasTestTag(ReviewTags.SHEET))).assertIsDisplayed()
        composeRule.onNodeWithTag(ReviewTags.NOTE).assertTextContains("Name it.")
        composeRule.onNodeWithTag(ReviewTags.ADD).performClick()
        assertEquals(ReviewAction.Comment("Name it."), actions.last())
    }

    @Test
    fun theSheetClosesWhenItsLinesAreGoneAfterTheActivityIsMadeAgain() {
        val actions = mutableListOf<ReviewAction>()
        var state by mutableStateOf(ReviewScreenState(page = page(), selected = 2..3, anchor = anchor))
        val restoration = StateRestorationTester(composeRule)
        restoration.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                ReviewScreen(state = state, onAction = { actions += it })
            }
        }
        composeRule.onNodeWithTag(ReviewTags.COMMENT_ON).performClick()
        composeRule.onNodeWithTag(ReviewTags.NOTE).performTextInput("Name it.")

        // The page that arrives no longer holds the lines.
        val without = page().let { it.copy(rows = it.rows.filter { row -> row.of !is ReviewRowOf.Line }) }
        state = ReviewScreenState(page = without)
        restoration.emulateSavedInstanceStateRestore()
        composeRule.waitForIdle()
        assertTrue(actions.none { it is ReviewAction.Select })

        // Selecting again later does not bring back a sheet that adds nothing.
        state = ReviewScreenState(page = page(), selected = 2..3, anchor = anchor)
        composeRule.waitForIdle()
        composeRule.onNodeWithTag(ReviewTags.SHEET).assertDoesNotExist()
    }

    /**
     * A file's head pinned at the top is drawn over the lines scrolled under
     * it: a hold on it opens its menu and selects none of them.
     */
    @Test
    fun aHoldOnAPinnedHeadSelectsNoLineBeneathIt() {
        val actions = mutableListOf<ReviewAction>()
        val long = page().let { it.copy(rows = listOf(it.rows[0]) + (1..80).map { number -> line(number) }) }
        show(ReviewScreenState(page = long), actions)
        composeRule.onNode(hasScrollToIndexAction()).performScrollToIndex(30)
        composeRule.waitForIdle()
        composeRule.onNodeWithTag(ReviewTags.FILE).performTouchInput { longClick() }
        composeRule.waitForIdle()
        assertTrue("selected $actions", actions.none { it is ReviewAction.Select })
        composeRule.onNodeWithTag(ReviewTags.FILE_COMMENT).assertIsDisplayed()
    }

    /** Diff lines are drawn at text height and the rest at full targets; a line is read as one element. */
    @Test
    fun aLineIsReadWithItsNumberAndChange() {
        show(ReviewScreenState(page = page()), mutableListOf())
        composeRule.onNodeWithTag(ReviewTags.line("l/$path/0/10")).assertIsDisplayed()
        composeRule.onNodeWithText("let x = 10;").assertIsDisplayed()
    }

    @Test
    fun doneGoesBackToTheChatWithTheDrafts() {
        val actions = mutableListOf<ReviewAction>()
        show(ReviewScreenState(page = page()), actions)
        composeRule.onNodeWithText("Done · 1 draft").assertIsDisplayed()
        composeRule.onNodeWithTag(ReviewTags.DONE).performClick()
        assertEquals(ReviewAction.Done, actions.last())
    }

    @Test
    fun discardingDraftsAndStartingOverAreConfirmedFirst() {
        val actions = mutableListOf<ReviewAction>()
        show(ReviewScreenState(page = page()), actions)
        composeRule.onNodeWithTag(ReviewTags.MORE).performClick()
        composeRule.onNodeWithTag(ReviewTags.DISCARD).performClick()
        assertTrue("nothing is discarded before the person says so", actions.isEmpty())
        composeRule.onNodeWithTag(ReviewTags.DISCARD_CONFIRM).performClick()
        assertEquals(ReviewAction.DiscardDrafts, actions.last())

        composeRule.onNodeWithTag(ReviewTags.MORE).performClick()
        composeRule.onNodeWithTag(ReviewTags.START_OVER).performClick()
        composeRule.onNodeWithTag(ReviewTags.START_OVER_CONFIRM).performClick()
        assertEquals(ReviewAction.StartOver, actions.last())

        composeRule.onNodeWithTag(ReviewTags.MORE).performClick()
        composeRule.onNodeWithTag(ReviewTags.REFRESH).performClick()
        assertEquals(ReviewAction.Refresh, actions.last())
    }

    @Test
    fun aStaleSnapshotOffersRefreshAndTheViewsSwitch() {
        val actions = mutableListOf<ReviewAction>()
        val views = listOf(ReviewViewTab(ReviewView.All, 1), ReviewViewTab(ReviewView.SinceLast, 1))
        show(
            ReviewScreenState(page = page(banners = listOf(ReviewBanner.ChangedSinceYouStarted), header = header(views = views))),
            actions,
        )
        composeRule.onNodeWithTag(ReviewTags.BANNER_ACTION).performClick()
        assertEquals(ReviewAction.Refresh, actions.last())
        composeRule.onNodeWithTag("${ReviewTags.VIEWS}.SinceLast").performClick()
        assertEquals(ReviewAction.Show(ReviewView.SinceLast), actions.last())
    }

    /** A computer that runs no reviews has the page say so, with nothing of a review to act on. */
    @Test
    fun aComputerWithoutReviewsSaysSo() {
        show(
            ReviewScreenState(
                page = ReviewPage(
                    banners = listOf(ReviewBanner.NotOffered), rows = emptyList(), toFetch = emptyList(),
                    opening = false, empty = null, header = null,
                ),
            ),
            mutableListOf(),
        )
        composeRule.onNodeWithTag(ReviewTags.BANNER).assertIsDisplayed()
        composeRule.onNodeWithTag(ReviewTags.FILES).assertDoesNotExist()
        composeRule.onNodeWithTag(ReviewTags.MORE).assertDoesNotExist()
        composeRule.onNodeWithTag(ReviewTags.DONE).assertDoesNotExist()
    }

    @Test
    fun aSentRoundShowsItsCommentsOrTheTextTheAgentRead() {
        val sent = SentReviewRef(reviewId = listOf(1), round = 2, inputId = listOf(7))
        var opened = false
        var round by mutableStateOf<ReviewPage?>(page().copy(rows = page().rows.map { row ->
            val of = row.of
            if (of is ReviewRowOf.Comment) row.copy(of = ReviewRowOf.Comment(of.value.copy(state = ReviewCommentState.Sent(3)))) else row
        }))
        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                ReviewRoundContent(
                    sent = sent, text = "src/lib.rs, line 11: Say why.", asked = true, round = round,
                    onOpenReview = { opened = true }, onClose = {},
                )
            }
        }
        composeRule.onNodeWithText("Round 3").assertIsDisplayed()
        composeRule.onNodeWithTag(ReviewTags.ROUND_COMMENT).assertIsDisplayed().assertHasNoClickAction()
        composeRule.onNodeWithTag(ReviewTags.ROUND_OPEN).performClick()
        assertTrue(opened)

        round = null
        composeRule.onNodeWithText("Round 2").assertIsDisplayed()
        composeRule.onNodeWithTag(ReviewTags.ROUND_GONE).assertIsDisplayed()
        composeRule.onNodeWithTag(ReviewTags.ROUND_TEXT).assertIsDisplayed()
    }
}
