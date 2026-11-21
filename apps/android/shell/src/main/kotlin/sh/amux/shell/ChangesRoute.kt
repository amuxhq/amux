package sh.amux.shell

import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import sh.amux.core.runtime.ReviewModel
import sh.amux.core.runtime.StoreBundle
import sh.amux.core.runtime.failureDescription
import sh.amux.design.AmuxGround
import sh.amux.features.ReviewPage
import sh.amux.features.ReviewPageAction
import sh.amux.features.ReviewPageState
import sh.amux.values.AgentKey

/**
 * The review of an agent's changes, on the comparison the chat had chosen
 * when it opened. The comparison stays put while the page is open.
 */
@Composable
internal fun ChangesRoute(
    agent: AgentKey,
    initialFile: String?,
    stores: StoreBundle,
    router: Router,
) {
    var retryNonce by remember(stores, agent) { mutableIntStateOf(0) }
    val opened = rememberChatLease(stores = stores, agent = agent, retryNonce = retryNonce)
    if (opened == null) {
        AmuxGround(modifier = Modifier.fillMaxSize()) {}
        return
    }
    val model = opened.getOrNull()?.model
    if (model == null) {
        ReviewPage(
            state = ReviewPageState(
                doc = null,
                error = opened.exceptionOrNull()?.failureDescription,
            ),
            onAction = { action ->
                when (action) {
                    ReviewPageAction.Back -> router.pop()
                    ReviewPageAction.Retry -> retryNonce += 1
                    else -> Unit
                }
            },
        )
        return
    }

    var reviewModel by remember(model) { mutableStateOf<ReviewModel?>(model.reviewing) }
    var loading by remember(model) { mutableStateOf(model.reviewing == null) }
    var loadError by remember(model) { mutableStateOf<String?>(null) }
    var reloadNonce by remember(model) { mutableIntStateOf(0) }

    LaunchedEffect(model, reloadNonce) {
        loading = reviewModel == null
        loadError = null
        model.loadReview().fold(
            onSuccess = { loaded ->
                reviewModel = loaded
                loadError = null
                if (loaded != null && !initialFile.isNullOrEmpty()) {
                    val matchIndex = loaded.doc.files.indexOfFirst { it.path == initialFile }
                    if (matchIndex >= 0) {
                        loaded.selectFile(matchIndex)
                    }
                }
            },
            onFailure = { failure ->
                loadError = failure.failureDescription
            },
        )
        loading = false
    }

    val reviewState = reviewModel?.state?.collectAsState()?.value

    ReviewPage(
        state = ReviewPageState(
            doc = reviewState?.doc,
            comments = reviewState?.comments.orEmpty(),
            selectedFileIndex = reviewState?.activeFile ?: 0,
            loading = loading,
            error = loadError ?: reviewState?.error,
        ),
        onAction = { action ->
            when (action) {
                ReviewPageAction.Back -> router.pop()
                ReviewPageAction.Retry -> reloadNonce += 1
                is ReviewPageAction.SelectFile -> reviewModel?.selectFile(action.fileIndex)
                is ReviewPageAction.AddComment -> {
                    reviewModel?.addComment(
                        path = action.path,
                        line = action.line,
                        oldLine = action.oldLine,
                        text = action.text,
                    )
                }
                is ReviewPageAction.EditComment -> reviewModel?.rewrite(action.index, action.text)
                is ReviewPageAction.DeleteComment -> reviewModel?.remove(action.index)
                ReviewPageAction.SendReview -> {
                    val current = reviewModel
                    if (current != null && current.comments.isNotEmpty()) {
                        model.attach(current)
                        router.pop()
                    }
                }
            }
        },
    )
}
