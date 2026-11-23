package sh.amux.shell

import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import sh.amux.core.runtime.ReviewSession
import sh.amux.core.runtime.StoreBundle
import sh.amux.core.runtime.failureDescription
import sh.amux.features.ReviewAction
import sh.amux.features.ReviewPlaceholderScreen
import sh.amux.features.ReviewScreen
import sh.amux.features.ReviewScreenState
import sh.amux.values.AgentKey

/**
 * An agent's review, pushed over its chat from the changes capsule, the
 * overview, the composer's review chip or a sent round. It is the agent's
 * review on its computer, read and written there; a computer that runs no
 * reviews has the page say so.
 *
 * The route holds its own lease on the chat, so the chat stays open under
 * the review while the chat's route is off screen, and its deltas keep
 * carrying the page.
 */
@Composable
internal fun ReviewRoute(
    agent: AgentKey,
    stores: StoreBundle,
    router: Router,
) {
    val opened = rememberChatLease(stores = stores, agent = agent)
    val model = opened?.getOrNull()?.model
    var session by remember(model) { mutableStateOf<ReviewSession?>(null) }
    // Opened once the route is on screen rather than while it is composed:
    // opening reads the chat's page from the core.
    DisposableEffect(model) {
        if (model == null) return@DisposableEffect onDispose {}
        session = model.openReviewPage()
        onDispose {
            session = null
            model.closeReviewPage()
        }
    }
    val live = session
    if (live == null) {
        ReviewPlaceholderScreen(
            failure = opened?.exceptionOrNull()?.failureDescription,
            onBack = { router.pop() },
        )
        return
    }
    val reviewState by live.state.collectAsState()
    val screenState = remember(reviewState) {
        ReviewScreenState(
            page = reviewState.page,
            selected = reviewState.selected,
            anchor = reviewState.anchor,
            notice = reviewState.notice,
            closed = reviewState.closed,
        )
    }
    val onAction: (ReviewAction) -> Unit = remember(live, router) {
        { action -> act(live, router, action) }
    }
    ReviewScreen(state = screenState, onAction = onAction)
}

private fun act(session: ReviewSession, router: Router, action: ReviewAction) {
    when (action) {
        ReviewAction.Back, ReviewAction.Done -> router.pop()
        ReviewAction.Refresh -> session.refresh()
        ReviewAction.Reopen -> session.open()
        ReviewAction.DiscardDrafts -> session.discardDrafts()
        ReviewAction.StartOver -> session.startOver()
        is ReviewAction.Show -> session.show(action.view)
        is ReviewAction.ToggleFile -> session.toggleFile(action.path)
        is ReviewAction.ToggleComment -> session.toggleComment(action.id)
        is ReviewAction.MoreContext -> session.moreContext(action.ask)
        is ReviewAction.Select -> session.select(action.row)
        is ReviewAction.Extend -> session.extend(action.row)
        ReviewAction.ClearSelection -> session.clearSelection()
        is ReviewAction.Comment -> session.comment(action.text)
        is ReviewAction.Reword -> session.reword(action.id, action.text)
        is ReviewAction.Delete -> session.delete(action.id)
    }
}
