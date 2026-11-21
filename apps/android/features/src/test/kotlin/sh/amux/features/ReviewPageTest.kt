package sh.amux.features

import androidx.compose.ui.test.longClick

import androidx.compose.ui.test.performTouchInput

import androidx.activity.OnBackPressedDispatcher
import androidx.activity.OnBackPressedDispatcherOwner
import androidx.activity.compose.LocalOnBackPressedDispatcherOwner
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleRegistry
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
    fun fileSelectionAndDiffLinesWork() {
        val actions = mutableListOf<ReviewPageAction>()
        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                ReviewPage(
                    state = ReviewPageState(doc = sampleDoc()),
                    onAction = { actions += it },
                )
            }
        }

        // Every file in one scroll.
        composeRule.onNodeWithText("return valid;").assertIsDisplayed()
        composeRule.onNodeWithText("Added Android docs.").assertIsDisplayed()

        // A heading folds its file shut and opens it again.
        composeRule.onNodeWithTag(AskReviewTags.reviewFileChip(0)).performClick()
        composeRule.onNodeWithText("return valid;").assertDoesNotExist()
        composeRule.onNodeWithTag(AskReviewTags.reviewFileChip(0)).performClick()
        composeRule.onNodeWithText("return valid;").assertIsDisplayed()

        // The Files menu jumps to a file.
        composeRule.onNodeWithTag(AskReviewTags.REVIEW_FILES).performClick()
        composeRule.onNodeWithTag(AskReviewTags.reviewFileMenuItem(1)).performClick()
        assertEquals(ReviewPageAction.SelectFile(1), actions.last())
    }

    @Test
    fun addingEditingAndDeletingReviewCommentsUpdatesPage() {
        var state by mutableStateOf(
            ReviewPageState(
                doc = sampleDoc(),
                comments = emptyList(),
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

        // Attach the review
        composeRule.onNodeWithTag(AskReviewTags.REVIEW_SEND).performClick()
        assertEquals(ReviewPageAction.SendReview, emitted.last())

        // Tap comment thread to delete
        composeRule.onNodeWithTag(AskReviewTags.reviewCommentThread(0)).performClick()
        composeRule.onNodeWithTag(AskReviewTags.REVIEW_COMMENT_DELETE).performClick()
        assertEquals(ReviewPageAction.DeleteComment(0), emitted.last())
        composeRule.onNodeWithTag(AskReviewTags.REVIEW_SEND).assertDoesNotExist()

        // Add file-level comment with a long press on the file's heading
        composeRule.onNodeWithTag(AskReviewTags.reviewFileChip(0)).performTouchInput { longClick() }
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

    @Test
    fun loadingAndErrorStatesRenderAndRetryEmitsAction() {
        val actions = mutableListOf<ReviewPageAction>()
        var state by mutableStateOf(ReviewPageState(doc = null, loading = true))

        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                ReviewPage(state = state, onAction = { actions += it })
            }
        }

        composeRule.onNodeWithTag(AskReviewTags.REVIEW_LOADING).assertIsDisplayed()

        state = ReviewPageState(doc = null, loading = false, error = "host unreachable")
        composeRule.waitForIdle()
        composeRule.onNodeWithTag(AskReviewTags.REVIEW_ERROR).assertIsDisplayed()
        composeRule.onNodeWithText("host unreachable").assertIsDisplayed()
        composeRule.onNodeWithTag(AskReviewTags.REVIEW_RETRY).performClick()
        assertEquals(listOf(ReviewPageAction.Retry), actions)
    }

    @Test
    fun aReloadedDiffClosesTheOpenCommentComposer() {
        var state by mutableStateOf(ReviewPageState(doc = sampleDoc()))
        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                ReviewPage(state = state, onAction = {})
            }
        }

        composeRule.onNodeWithTag(AskReviewTags.reviewLine(0, 0, 2)).performClick()
        composeRule.onNodeWithTag(AskReviewTags.REVIEW_COMMENT_SHEET).assertIsDisplayed()

        state = state.copy(doc = sampleDoc().copy(head = "a-later-head"))
        composeRule.waitForIdle()
        composeRule.onNodeWithTag(AskReviewTags.REVIEW_COMMENT_SHEET).assertDoesNotExist()
    }

    @Test
    fun systemBackDismissesCommentComposerInsteadOfPoppingReviewPage() {
        val backDispatcher = OnBackPressedDispatcher()
        val backOwner = object : OnBackPressedDispatcherOwner {
            private val registry = LifecycleRegistry(this).apply {
                currentState = Lifecycle.State.RESUMED
            }
            override val lifecycle: Lifecycle = registry
            override val onBackPressedDispatcher: OnBackPressedDispatcher = backDispatcher
        }
        val actions = mutableListOf<ReviewPageAction>()
        composeRule.setContent {
            CompositionLocalProvider(LocalOnBackPressedDispatcherOwner provides backOwner) {
                AmuxTheme(appearance = Appearance.Light, photographed = true) {
                    ReviewPage(
                        state = ReviewPageState(doc = sampleDoc()),
                        onAction = { actions += it },
                    )
                }
            }
        }

        composeRule.onNodeWithTag(AskReviewTags.reviewLine(0, 0, 2)).performClick()
        composeRule.onNodeWithTag(AskReviewTags.REVIEW_COMMENT_SHEET).assertIsDisplayed()

        composeRule.runOnIdle { backDispatcher.onBackPressed() }
        composeRule.onNodeWithTag(AskReviewTags.REVIEW_COMMENT_SHEET).assertDoesNotExist()
        assertEquals(emptyList<ReviewPageAction>(), actions)
    }

    @Test
    fun theCommentDraftSurvivesThePageRedrawingUnderIt() {
        var state by mutableStateOf(ReviewPageState(doc = sampleDoc()))
        val actions = mutableListOf<ReviewPageAction>()
        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                ReviewPage(state = state, onAction = { actions += it })
            }
        }

        composeRule.onNodeWithTag(AskReviewTags.reviewLine(0, 0, 2)).performClick()
        composeRule.onNodeWithTag(AskReviewTags.REVIEW_COMMENT_INPUT).performSemanticsAction(SemanticsActions.SetText) {
            it(AnnotatedString("Keep this draft"))
        }
        state = state.copy(loading = true)
        composeRule.waitForIdle()

        composeRule.onNodeWithTag(AskReviewTags.REVIEW_COMMENT_SHEET).assertIsDisplayed()
        composeRule.onNodeWithText("Keep this draft").assertIsDisplayed()
        assertEquals(emptyList<ReviewPageAction>(), actions)

        composeRule.onNodeWithTag(AskReviewTags.REVIEW_COMMENT_ADD).performClick()
        assertEquals(
            listOf<ReviewPageAction>(
                ReviewPageAction.AddComment("src/auth.rs", line = 15, oldLine = 0, text = "Keep this draft"),
            ),
            actions,
        )
    }
}
