package sh.amux.features

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.semantics.SemanticsActions
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performSemanticsAction
import androidx.compose.ui.text.AnnotatedString
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode
import sh.amux.design.AmuxTheme
import sh.amux.design.Appearance
import sh.amux.values.Comparison
import sh.amux.values.DiffLine
import sh.amux.values.FileStatus
import sh.amux.values.Hunk
import sh.amux.values.LineKind
import sh.amux.values.ReviewComment
import sh.amux.values.ReviewDoc
import sh.amux.values.ReviewFile

@RunWith(RobolectricTestRunner::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
@Config(sdk = [34], qualifiers = "w412dp-h915dp-xxhdpi")
class ReviewPageTest {
    @get:Rule
    val composeRule = createComposeRule()

    private fun sampleDoc(): ReviewDoc = ReviewDoc(
        base = "main",
        head = "abc1234",
        added = 3,
        removed = 1,
        files = listOf(
            ReviewFile(
                path = "src/auth.rs",
                status = FileStatus.Modified,
                added = 2,
                removed = 1,
                binary = false,
                comments = emptyList(),
                hunks = listOf(
                    Hunk(
                        skips = true,
                        scope = "fn verify_token()",
                        lines = listOf(
                            DiffLine(LineKind.Context, "let valid = check(tok);", emptyList(), newLine = 14, oldLine = 14),
                            DiffLine(LineKind.Removed, "return false;", emptyList(), newLine = null, oldLine = 15),
                            DiffLine(LineKind.Added, "return valid;", emptyList(), newLine = 15, oldLine = null),
                            DiffLine(LineKind.Added, "log::debug!(\"ok\");", emptyList(), newLine = 16, oldLine = null),
                        ),
                    ),
                ),
            ),
            ReviewFile(
                path = "README.md",
                status = FileStatus.Modified,
                added = 1,
                removed = 0,
                binary = false,
                comments = emptyList(),
                hunks = listOf(
                    Hunk(
                        skips = false,
                        scope = "",
                        lines = listOf(
                            DiffLine(LineKind.Added, "Added Android docs.", emptyList(), newLine = 1, oldLine = null),
                        ),
                    ),
                ),
            ),
        ),
    )

    @Test
    fun fileSelectionComparisonToggleAndDiffLinesWork() {
        val actions = mutableListOf<ReviewPageAction>()
        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                ReviewPage(
                    state = ReviewPageState(
                        doc = sampleDoc(),
                        comparison = Comparison.Uncommitted,
                        baseBranch = "main",
                    ),
                    onAction = { actions += it },
                )
            }
        }

        composeRule.onNodeWithText("return valid;").assertIsDisplayed()

        // Switch comparison
        composeRule.onNodeWithTag(AskReviewTags.reviewComparison("onbranch")).performClick()
        assertEquals(ReviewPageAction.SelectComparison(Comparison.OnBranch), actions.last())

        // Switch file chip
        composeRule.onNodeWithTag(AskReviewTags.reviewFileChip(1)).performClick()
        assertEquals(ReviewPageAction.SelectFile(1), actions.last())
        composeRule.onNodeWithText("Added Android docs.").assertIsDisplayed()
    }

    @Test
    fun addingEditingAndDeletingReviewCommentsUpdatesPage() {
        var state by mutableStateOf(
            ReviewPageState(
                doc = sampleDoc(),
                comments = emptyList(),
                comparison = Comparison.Uncommitted,
                baseBranch = "main",
            ),
        )
        val emitted = mutableListOf<ReviewPageAction>()

        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                ReviewPage(
                    state = state,
                    onAction = { action ->
                        emitted += action
                        when (action) {
                            is ReviewPageAction.AddComment -> {
                                state = state.copy(
                                    comments = state.comments + ReviewComment(
                                        path = action.path,
                                        line = action.line,
                                        oldLine = action.oldLine,
                                        text = action.text,
                                    ),
                                )
                            }
                            is ReviewPageAction.EditComment -> {
                                val next = state.comments.toMutableList()
                                next[action.index] = next[action.index].copy(text = action.text)
                                state = state.copy(comments = next)
                            }
                            is ReviewPageAction.DeleteComment -> {
                                val next = state.comments.toMutableList()
                                next.removeAt(action.index)
                                state = state.copy(comments = next)
                            }
                            else -> Unit
                        }
                    },
                )
            }
        }

        // Tap added line 15 to open comment composer
        composeRule.onNodeWithTag(AskReviewTags.reviewLine(0, 0, 2)).performClick()
        composeRule.onNodeWithTag(AskReviewTags.REVIEW_COMMENT_SHEET).assertIsDisplayed()
        composeRule.onNodeWithTag(AskReviewTags.REVIEW_COMMENT_INPUT).performSemanticsAction(SemanticsActions.SetText) {
            it(AnnotatedString("Handle expired tokens here"))
        }
        composeRule.onNodeWithTag(AskReviewTags.REVIEW_COMMENT_ADD).performClick()

        assertEquals(
            ReviewPageAction.AddComment("src/auth.rs", line = 15, oldLine = 0, text = "Handle expired tokens here"),
            emitted.last(),
        )
        composeRule.onNodeWithTag(AskReviewTags.reviewCommentThread(0)).assertIsDisplayed()
        composeRule.onNodeWithTag(AskReviewTags.REVIEW_SEND).assertIsDisplayed()

        // Tap comment thread to edit and save
        composeRule.onNodeWithTag(AskReviewTags.reviewCommentThread(0)).performClick()
        composeRule.onNodeWithTag(AskReviewTags.REVIEW_COMMENT_INPUT).performSemanticsAction(SemanticsActions.SetText) {
            it(AnnotatedString("Handle expired and revoked tokens"))
        }
        composeRule.onNodeWithTag(AskReviewTags.REVIEW_COMMENT_SAVE).performClick()
        assertEquals(ReviewPageAction.EditComment(0, "Handle expired and revoked tokens"), emitted.last())
        composeRule.onNodeWithText("Handle expired and revoked tokens").assertIsDisplayed()

        // Tap Send Review
        composeRule.onNodeWithTag(AskReviewTags.REVIEW_SEND).performClick()
        assertEquals(ReviewPageAction.SendReview, emitted.last())

        // Tap comment thread to delete
        composeRule.onNodeWithTag(AskReviewTags.reviewCommentThread(0)).performClick()
        composeRule.onNodeWithTag(AskReviewTags.REVIEW_COMMENT_DELETE).performClick()
        assertEquals(ReviewPageAction.DeleteComment(0), emitted.last())
        composeRule.onNodeWithTag(AskReviewTags.REVIEW_SEND).assertDoesNotExist()

        // Add file-level comment
        composeRule.onNodeWithTag(AskReviewTags.REVIEW_FILE_COMMENT).performClick()
        composeRule.onNodeWithTag(AskReviewTags.REVIEW_COMMENT_INPUT).performSemanticsAction(SemanticsActions.SetText) {
            it(AnnotatedString("Looks solid overall"))
        }
        composeRule.onNodeWithTag(AskReviewTags.REVIEW_COMMENT_ADD).performClick()
        assertEquals(
            ReviewPageAction.AddComment("src/auth.rs", line = 0, oldLine = 0, text = "Looks solid overall"),
            emitted.last(),
        )
    }

    @Test
    fun emptyDocShowsCleanEmptyState() {
        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                ReviewPage(
                    state = ReviewPageState(doc = null),
                    onAction = {},
                )
            }
        }

        composeRule.onNodeWithTag(AskReviewTags.REVIEW_EMPTY).assertIsDisplayed()
        composeRule.onNodeWithText("No changes").assertIsDisplayed()
    }
}
