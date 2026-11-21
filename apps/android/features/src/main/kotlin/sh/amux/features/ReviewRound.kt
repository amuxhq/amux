package sh.amux.features

import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Immutable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.unit.dp
import sh.amux.design.AmuxTheme
import sh.amux.design.Space
import sh.amux.values.ReviewBanner
import sh.amux.values.ReviewCommentState
import sh.amux.values.ReviewPage
import sh.amux.values.ReviewRowOf
import sh.amux.values.SentReviewRef
import sh.amux.words.ChatWords

/** A sent review whose round is raised over the chat: the review as its chip names it, and the text the agent read. */
@Immutable
data class OpenedRound(val sent: SentReviewRef, val text: String)

/**
 * The round a review sent into the chat closed, raised over the chat from
 * its chip: its whole diff, from the base to the round's snapshot, with each
 * comment it sent on its lines, read from the agent's computer; each comment
 * with the code stored with it once the computer no longer keeps the round's
 * snapshot. When the computer no longer holds the round, as after starting
 * over, or cannot be reached with none of it loaded, it shows the text the
 * agent read. The review itself, as it stands now, is one tap away.
 *
 * [read] asks the agent's computer for the round; it answers null once the
 * computer holds none of it.
 */
@Composable
fun ReviewRoundSheet(
    sent: SentReviewRef,
    text: String,
    read: suspend () -> ReviewPage?,
    onOpenReview: () -> Unit,
    onClose: () -> Unit,
) {
    var asked by remember(sent) { mutableStateOf(false) }
    var round by remember(sent) { mutableStateOf<ReviewPage?>(null) }
    LaunchedEffect(sent) {
        if (asked) return@LaunchedEffect
        round = read()
        asked = true
    }
    AmuxSheet(onDismissRequest = onClose) {
        ReviewRoundContent(
            sent = sent,
            text = text,
            asked = asked,
            round = round,
            onOpenReview = onOpenReview,
            onClose = onClose,
        )
    }
}

/**
 * What the round's sheet holds, once [asked]: the round read, or when
 * [round] is null the text the agent read. Drawn on its own where the
 * sheet's window cannot be, as a story pictures it.
 */
@Composable
fun ReviewRoundContent(
    sent: SentReviewRef,
    text: String,
    asked: Boolean,
    round: ReviewPage?,
    onOpenReview: () -> Unit,
    onClose: () -> Unit,
) {
    val instead = if (asked) roundInstead(round) else null
    val number = round?.rows?.firstNotNullOfOrNull { row ->
        ((row.of as? ReviewRowOf.Comment)?.value?.state as? ReviewCommentState.Sent)?.round
    } ?: sent.round
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .fillMaxHeight()
            .semantics {
                stateDescription = when {
                    !asked -> "reading"
                    instead == null -> "round"
                    else -> "text"
                }
            }
            .testTag(ReviewTags.ROUND),
    ) {
        AmuxSheetHeader(
            title = ChatWords.reviewRound(number),
            trailing = {
                TextButton(
                    onClick = onClose,
                    modifier = Modifier.testTag(ReviewTags.ROUND_DONE),
                ) {
                    Text(
                        text = stringResource(R.string.review_round_done),
                        style = AmuxTheme.typography.bodyEmphasis,
                        color = AmuxTheme.colors.accent,
                    )
                }
            },
        )
        Box(modifier = Modifier.weight(1f)) {
            val shown = round
            when {
                !asked -> ReviewWaiting(text = ChatWords.REVIEW_OPENING)
                shown != null && instead == null -> RoundRows(page = shown)
                else -> RoundText(why = instead ?: ChatWords.REVIEW_ROUND_GONE, text = text)
            }
            if (asked) {
                AmuxActionButton(
                    label = ChatWords.REVIEW_OPEN_REVIEW,
                    kind = ActionButtonKind.Outline,
                    fillWidth = true,
                    onClick = {
                        onClose()
                        onOpenReview()
                    },
                    modifier = Modifier
                        .align(Alignment.BottomCenter)
                        .padding(horizontal = AmuxTheme.metrics.gutter, vertical = Space.s)
                        .testTag(ReviewTags.ROUND_OPEN),
                )
            }
        }
    }
}

/**
 * Why the sheet shows the text the agent read rather than the round: the
 * computer holds none of it, or cannot be reached and none of it was loaded.
 * Null while it shows the round.
 */
private fun roundInstead(page: ReviewPage?): String? {
    if (page == null) return ChatWords.REVIEW_ROUND_GONE
    val unloaded = page.header == null && ReviewBanner.Unreachable in page.banners
    return if (unloaded) ChatWords.REVIEW_ROUND_UNREACHABLE else null
}

/** A round's rows: its whole diff under its files' heads, with its comments, drawn as the page draws them, to read only. */
@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun RoundRows(page: ReviewPage) {
    val entries = remember(page.rows) { ReviewEntries(page.rows) }
    val folded = stringResource(R.string.review_folded)
    LazyColumn(
        contentPadding = PaddingValues(bottom = 96.dp),
        modifier = Modifier.fillMaxWidth(),
    ) {
        entries.all.forEachIndexed { index, entry ->
            val key = entries.keys[index]
            when (entry) {
                ReviewEntries.Entry.Above -> item(key = key) {
                    if (page.banners.isNotEmpty()) {
                        Column(
                            modifier = Modifier.padding(horizontal = AmuxTheme.metrics.gutter, vertical = Space.s),
                            verticalArrangement = Arrangement.spacedBy(Space.s),
                        ) {
                            page.banners.forEach { ReviewBannerCard(banner = it) }
                        }
                    }
                }
                is ReviewEntries.Entry.Head -> stickyHeader(key = key) {
                    val head = (page.rows[entry.row].of as ReviewRowOf.File).value
                    ReviewFileHeadRow(
                        head = head,
                        tally = false,
                        modifier = Modifier.testTag(ReviewTags.ROUND_FILE),
                    )
                }
                is ReviewEntries.Entry.Body -> item(key = key) {
                    when (val of = page.rows[entry.row].of) {
                        is ReviewRowOf.Comment -> ReviewCommentCard(
                            comment = of.value,
                            modifier = Modifier
                                .semantics(mergeDescendants = true) {
                                    stateDescription = commentValue(of.value, folded)
                                }
                                .testTag(ReviewTags.ROUND_COMMENT),
                        )
                        is ReviewRowOf.Line -> ReviewCodeLineRow(line = of.value)
                        is ReviewRowOf.Hunk -> ReviewHunkBand(hunk = of.value)
                        is ReviewRowOf.NoLines -> ReviewNote(text = ChatWords.reviewNoLines(of.why))
                        is ReviewRowOf.File, is ReviewRowOf.Gap, is ReviewRowOf.Loading -> Unit
                    }
                }
            }
        }
    }
}

/** The text the agent read, with why the round itself is not shown. */
@Composable
private fun RoundText(why: String, text: String) {
    val colors = AmuxTheme.colors
    LazyColumn(
        contentPadding = PaddingValues(
            start = AmuxTheme.metrics.gutter,
            end = AmuxTheme.metrics.gutter,
            top = Space.m,
            bottom = 96.dp,
        ),
        verticalArrangement = Arrangement.spacedBy(Space.m),
        modifier = Modifier.fillMaxWidth(),
    ) {
        item {
            Text(
                text = why,
                style = AmuxTheme.typography.detail,
                color = colors.inkMuted,
                modifier = Modifier.testTag(ReviewTags.ROUND_GONE),
            )
        }
        item {
            SelectionContainer {
                Text(
                    text = text,
                    style = AmuxTheme.typography.monoSmall,
                    color = colors.ink,
                    modifier = Modifier
                        .fillMaxWidth()
                        .semantics { contentDescription = text }
                        .testTag(ReviewTags.ROUND_TEXT),
                )
            }
        }
    }
}
