package sh.amux.features

import androidx.compose.ui.graphics.Color
import sh.amux.design.backdrop
import sh.amux.design.ThumbTarget
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.wrapContentHeight
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.text.rememberTextMeasurer
import sh.amux.design.IconSize
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.CheckCircle
import androidx.compose.material.icons.outlined.RadioButtonChecked
import androidx.compose.material.icons.outlined.RadioButtonUnchecked
import androidx.compose.material.icons.outlined.StopCircle
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Immutable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextDecoration
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import sh.amux.design.AmuxChrome
import sh.amux.design.AmuxHairline
import sh.amux.design.AmuxPanel
import sh.amux.design.AmuxTheme
import sh.amux.design.Space
import sh.amux.values.ChangeTotals
import sh.amux.values.Changes
import sh.amux.values.Comparison
import sh.amux.values.ContextView
import sh.amux.values.FileStatus
import sh.amux.values.Folder
import sh.amux.values.JobRow
import sh.amux.values.JobStop
import sh.amux.values.Overview
import sh.amux.values.ServerView
import sh.amux.values.TaskMark
import sh.amux.values.TasksView
import sh.amux.values.UsageState
import sh.amux.values.UsageView
import sh.amux.words.ChatWords
import java.time.Instant

/** Where an agent runs, shown at the top of its overview sheet. */
@Immutable
data class ChatSubjectUi(
    val name: String,
    val host: String = "",
    val directory: String = "",
    val branch: String? = null,
    val address: String = "",
)

/** Pure UI state rendered by [ChatOverviewSheetContent] and [ChatOverviewModalSheet]. */
@Immutable
data class ChatOverviewState(
    val subject: ChatSubjectUi,
    val overview: Overview? = null,
    val changesComparison: Comparison? = null,
    val changesLoading: Boolean = false,
    val changesError: String? = null,
    val context: ContextView? = null,
    val comparisons: List<Comparison> = listOf(Comparison.Uncommitted),
    val comparison: Comparison = Comparison.Uncommitted,
    val baseBranch: String? = null,
    val changeTotals: ChangeTotals? = null,
    val nowMs: Long = 0L,
)

/** Actions emitted by the chat overview sheet. */
sealed interface ChatOverviewAction {
    data class StopJob(val key: String) : ChatOverviewAction
    data class SelectComparison(val comparison: Comparison) : ChatOverviewAction
    data object RetryChanges : ChatOverviewAction
    data object OpenReview : ChatOverviewAction
    data object Dismiss : ChatOverviewAction
}

/**
 * M3 [ModalBottomSheet] displaying the chat overview: tasks, background jobs
 * with stop confirmation, failed tool servers, usage near limit, context usage,
 * and changed files grouped by folder.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ChatOverviewModalSheet(
    state: ChatOverviewState,
    onAction: (ChatOverviewAction) -> Unit,
    clock: ElapsedClock = ElapsedClock.Still,
) {
    AmuxSheet(
        onDismissRequest = { onAction(ChatOverviewAction.Dismiss) },
    ) {
        // The open sheet counts its jobs' and limits' times with the chat's
        // clock; read here, a tick redraws the sheet and not the chat.
        ChatOverviewSheetContent(
            state = state.copy(nowMs = clock.nowMs()),
            onAction = onAction,
        )
    }
}

/**
 * Pure Compose body of the chat overview sheet. Exposed directly so headless
 * storybook and Robolectric tests can exercise every section cleanly.
 *
 * The agent's own page: its name and where it runs, then what runs around
 * the chat, in iOS's order: the background jobs, the changed files for the
 * chosen comparison, the tool servers that failed, the usage windows near a
 * limit and the context near its end. Sections are flat rows under their
 * heads. The task list is not repeated here; it docks above the composer.
 */
@Composable
fun ChatOverviewSheetContent(
    state: ChatOverviewState,
    onAction: (ChatOverviewAction) -> Unit,
    modifier: Modifier = Modifier,
) {
    val overview = state.overview
    val now = Instant.ofEpochMilli(state.nowMs)
    var pendingStop by remember { mutableStateOf<Pair<JobRow, String>?>(null) }

    Column(
        modifier = modifier
            .fillMaxWidth()
            .background(AmuxTheme.backdrop)
            .verticalScroll(rememberScrollState())
            .padding(bottom = Space.xl)
            .testTag(AskReviewTags.OVERVIEW_SHEET),
        verticalArrangement = Arrangement.spacedBy(OverviewSectionGap),
    ) {
        OverviewPlaceHeader(
            subject = state.subject,
            onClose = { onAction(ChatOverviewAction.Dismiss) },
        )

        if (overview != null && overview.jobs.isNotEmpty()) {
            OverviewJobsSection(
                jobs = overview.jobs,
                now = now,
                onRequestStop = { job, key -> pendingStop = job to key },
            )
        }

        OverviewChangesSection(
            changes = overview?.changes,
            changesComparison = state.changesComparison,
            changesLoading = state.changesLoading,
            changesError = state.changesError,
            totals = state.changeTotals,
            comparisons = state.comparisons,
            comparison = state.comparison,
            baseBranch = state.baseBranch,
            onSelectComparison = { onAction(ChatOverviewAction.SelectComparison(it)) },
            onRetryChanges = { onAction(ChatOverviewAction.RetryChanges) },
            onOpenReview = { onAction(ChatOverviewAction.OpenReview) },
        )

        if (overview != null && overview.failedServers.isNotEmpty()) {
            OverviewFailedServersSection(servers = overview.failedServers)
        }

        val usage = overview?.usageNearLimit
        if (usage != null) {
            OverviewUsageSection(usage = usage, now = now)
        }

        // Context is said only while it is worth saying: near its end the
        // agent compacts what it remembers.
        val context = state.context
        val left = context?.let(ChatWords::contextLeft)
        if (context != null && left != null) {
            OverviewContextSection(context = context, left = left)
        }
    }

    val confirmStop = pendingStop
    if (confirmStop != null) {
        val (job, key) = confirmStop
        StopJobDialog(
            job = job,
            onDismiss = { pendingStop = null },
            onConfirm = {
                pendingStop = null
                onAction(ChatOverviewAction.StopJob(key))
            },
        )
    }
}

/** The room between the overview's sections, as iOS spaces them. */
private val OverviewSectionGap = 22.dp

@Composable
private fun OverviewPlaceHeader(
    subject: ChatSubjectUi,
    onClose: () -> Unit,
) {
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .testTag(AskReviewTags.OVERVIEW_PLACE),
        verticalArrangement = Arrangement.spacedBy(Space.s),
    ) {
        AmuxSheetHeader(
            title = subject.name,
            onClose = onClose,
            closeModifier = Modifier.testTag(AskReviewTags.OVERVIEW_CLOSE),
        )
        val facts = buildList {
            if (subject.host.isNotEmpty()) add(PlaceFact(stringResource(R.string.overview_label_host), subject.host))
            if (subject.directory.isNotEmpty()) {
                add(PlaceFact(stringResource(R.string.overview_label_directory), subject.directory))
            }
            if (!subject.branch.isNullOrEmpty()) {
                add(PlaceFact(stringResource(R.string.overview_label_branch), subject.branch, AskReviewTags.OVERVIEW_BRANCH))
            }
            if (subject.address.isNotEmpty()) add(PlaceFact(stringResource(R.string.overview_label_address), subject.address))
        }
        PlaceFacts(facts = facts, modifier = Modifier.padding(horizontal = Space.margin))
    }
}

/** One of the place's facts; [tag] names the ones a journey reads. */
private data class PlaceFact(val label: String, val value: String, val tag: String? = null)

/**
 * The place's facts as two columns: every label the widest label's width,
 * so the values start on one edge.
 */
@Composable
private fun PlaceFacts(
    facts: List<PlaceFact>,
    modifier: Modifier = Modifier,
) {
    if (facts.isEmpty()) return
    val colors = AmuxTheme.colors
    val labelStyle = AmuxTheme.typography.caption
    val measurer = rememberTextMeasurer()
    val density = LocalDensity.current
    val labelWidth = remember(facts, labelStyle, density) {
        with(density) {
            facts.maxOf { measurer.measure(it.label, labelStyle).size.width }.toDp()
        }
    }
    Column(
        modifier = modifier.fillMaxWidth(),
        verticalArrangement = Arrangement.spacedBy(Space.s),
    ) {
        facts.forEach { (label, value, tag) ->
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .semantics(mergeDescendants = true) {
                        if (tag != null) {
                            reportedLabel = label
                            reportedValue = value
                        }
                    }
                    .then(if (tag != null) Modifier.testTag(tag) else Modifier),
                horizontalArrangement = Arrangement.spacedBy(Space.m),
            ) {
                Text(
                    text = label,
                    style = labelStyle,
                    color = colors.inkFaint,
                    modifier = Modifier
                        .width(labelWidth)
                        .alignByBaseline(),
                )
                SelectionContainer(modifier = Modifier.weight(1f).alignByBaseline()) {
                    Text(
                        text = value,
                        style = AmuxTheme.typography.mono,
                        color = colors.inkMuted,
                        maxLines = 2,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
            }
        }
    }
}

/**
 * The jobs still running. One the agent's kind can stop alone is a button
 * the whole row wide; a tap asks first, naming it. The agent keeps running
 * either way.
 */
@Composable
private fun OverviewJobsSection(
    jobs: List<JobRow>,
    now: Instant,
    onRequestStop: (JobRow, String) -> Unit,
) {
    val colors = AmuxTheme.colors
    Column(modifier = Modifier.fillMaxWidth()) {
        AmuxSectionHeader(
            title = stringResource(R.string.overview_section_background),
            trailing = jobs.size.toString(),
        )
        jobs.forEachIndexed { index, job ->
            val stopKey = when (val stop = job.stop) {
                is JobStop.Offered -> stop.key
                is JobStop.Refused -> stop.key
                JobStop.Stopping, JobStop.Unavailable -> null
            }
            val elapsed = ChatWords.ran(sinceMs = job.startedAtMs, now = now)
            val command = ChatWords.firstLine(job.command)
            val stopLabel = stringResource(R.string.overview_stop_job_description, command)
            Column(
                modifier = Modifier
                    .fillMaxWidth()
                    .testTag(AskReviewTags.overviewJob(index)),
            ) {
                Row(
                    modifier = Modifier
                        .fillMaxWidth()
                        .heightIn(min = ThumbTarget.MinSize)
                        .then(
                            if (stopKey != null) {
                                Modifier
                                    .clickable(role = Role.Button, onClickLabel = stopLabel) {
                                        onRequestStop(job, stopKey)
                                    }
                                    .testTag(AskReviewTags.overviewJobStop(index))
                            } else {
                                Modifier
                            },
                        )
                        .semantics(mergeDescendants = true) {}
                        .padding(horizontal = Space.margin),
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(Space.s + Space.xxs),
                ) {
                    Text(
                        text = command,
                        style = AmuxTheme.typography.mono,
                        color = colors.ink,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                        modifier = Modifier.weight(1f),
                    )
                    if (job.stop is JobStop.Stopping) {
                        Text(
                            text = ChatWords.STOPPING_JOB,
                            style = AmuxTheme.typography.caption,
                            color = colors.inkMuted,
                        )
                    } else {
                        Text(
                            text = elapsed,
                            style = AmuxTheme.typography.caption,
                            color = colors.inkFaint,
                        )
                    }
                    if (stopKey != null) {
                        Icon(
                            imageVector = Icons.Outlined.StopCircle,
                            contentDescription = null,
                            tint = colors.inkMuted,
                            modifier = Modifier.size(IconSize.Row),
                        )
                    }
                }
                val refused = job.stop as? JobStop.Refused
                if (refused != null && refused.error.isNotEmpty()) {
                    Text(
                        text = ChatWords.firstLine(refused.error),
                        style = AmuxTheme.typography.caption,
                        color = colors.accent,
                        maxLines = 2,
                        overflow = TextOverflow.Ellipsis,
                        modifier = Modifier.padding(horizontal = Space.margin),
                    )
                }
            }
        }
    }
}

@Composable
private fun OverviewFailedServersSection(servers: List<ServerView>) {
    val colors = AmuxTheme.colors
    Column(
        modifier = Modifier.fillMaxWidth(),
        verticalArrangement = Arrangement.spacedBy(Space.s + Space.xxs),
    ) {
        AmuxSectionHeader(
            title = stringResource(R.string.overview_section_servers),
            trailing = servers.size.toString(),
        )
        servers.forEach { server ->
            Column(
                modifier = Modifier
                    .fillMaxWidth()
                    .semantics(mergeDescendants = true) {}
                    .padding(horizontal = Space.margin)
                    .testTag(AskReviewTags.overviewServer(server.name)),
                verticalArrangement = Arrangement.spacedBy(Space.xxs),
            ) {
                Text(
                    text = ChatWords.failed(server),
                    style = AmuxTheme.typography.body,
                    color = colors.accent,
                )
                if (server.error.isNotEmpty()) {
                    Text(
                        text = ChatWords.firstLine(server.error),
                        style = AmuxTheme.typography.caption,
                        color = colors.inkFaint,
                        maxLines = 2,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
            }
        }
    }
}

@Composable
private fun OverviewUsageSection(
    usage: UsageView,
    now: Instant,
) {
    val colors = AmuxTheme.colors
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .testTag(AskReviewTags.OVERVIEW_USAGE),
        verticalArrangement = Arrangement.spacedBy(Space.s + Space.xxs),
    ) {
        AmuxSectionHeader(
            title = stringResource(R.string.overview_section_usage),
            trailing = stringResource(
                if (usage.blocked) R.string.overview_usage_blocked else R.string.overview_usage_near_limit,
            ),
        )
        usage.windows.forEachIndexed { index, window ->
            val label = ChatWords.usageLabel(window.label)
            val detail = ChatWords.usageDetail(window, now = now)
            // A reached limit is a failure and takes the accent; one near
            // its limit reads in ink; the rest stay muted.
            val rowColor = when (window.state) {
                UsageState.Blocked -> colors.accent
                UsageState.NearLimit -> colors.ink
                else -> colors.inkMuted
            }
            Column(
                modifier = Modifier
                    .fillMaxWidth()
                    .semantics(mergeDescendants = true) {}
                    .padding(horizontal = Space.margin)
                    .testTag(AskReviewTags.overviewUsageWindow(index)),
                verticalArrangement = Arrangement.spacedBy(Space.xxs),
            ) {
                Row(
                    modifier = Modifier.fillMaxWidth(),
                    horizontalArrangement = Arrangement.spacedBy(6.dp),
                ) {
                    Text(
                        text = label,
                        style = AmuxTheme.typography.body,
                        color = rowColor,
                        modifier = Modifier.weight(1f),
                    )
                    Text(
                        text = ChatWords.used(window),
                        style = AmuxTheme.typography.body,
                        color = rowColor,
                    )
                }
                if (detail != null) {
                    Text(
                        text = detail,
                        style = AmuxTheme.typography.caption,
                        color = colors.inkFaint,
                    )
                }
            }
        }
        val credits = usage.credits
        if (!credits.isNullOrEmpty()) {
            Text(
                text = stringResource(R.string.overview_usage_credits, credits),
                style = AmuxTheme.typography.caption,
                color = colors.inkFaint,
                modifier = Modifier.padding(horizontal = Space.margin),
            )
        }
    }
}

/** How full the conversation's context is, in ink: it is information, not a failure. */
@Composable
private fun OverviewContextSection(context: ContextView, left: String) {
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .semantics(mergeDescendants = true) {}
            .testTag(AskReviewTags.OVERVIEW_CONTEXT),
        verticalArrangement = Arrangement.spacedBy(Space.s + Space.xxs),
    ) {
        AmuxSectionHeader(
            title = stringResource(R.string.overview_section_context),
            trailing = left,
        )
        Text(
            text = ChatWords.contextUsed(context),
            style = AmuxTheme.typography.body,
            color = AmuxTheme.colors.ink,
            modifier = Modifier.padding(horizontal = Space.margin),
        )
    }
}

/**
 * Static horizontal context capacity meter: a crisp neutral bar, without
 * Material's progress indicator spinners.
 */
@Composable
internal fun ContextMeterBlock(
    context: ContextView,
    modifier: Modifier = Modifier,
) {
    val colors = AmuxTheme.colors
    val fraction = ((context.percent ?: 0L).coerceIn(0L, 100L)) / 100f

    Column(
        modifier = modifier.fillMaxWidth(),
        verticalArrangement = Arrangement.spacedBy(Space.s),
    ) {
        if (context.percent != null) {
            Box(
                modifier = Modifier
                    .fillMaxWidth()
                    .height(6.dp)
                    .clip(RoundedCornerShape(3.dp))
                    .background(colors.sunken),
            ) {
                if (fraction > 0f) {
                    Box(
                        modifier = Modifier
                            .fillMaxWidth(fraction)
                            .fillMaxHeight()
                            .clip(RoundedCornerShape(3.dp))
                            .background(colors.inkMuted),
                    )
                }
            }
        }
        Row(
            modifier = Modifier.fillMaxWidth(),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text(
                text = ChatWords.contextUsed(context),
                style = AmuxTheme.typography.detail,
                color = colors.ink,
                modifier = Modifier.weight(1f),
            )
            val percent = context.percent
            if (percent != null) {
                Text(
                    text = stringResource(R.string.overview_context_percent, percent),
                    style = AmuxTheme.typography.monoSmall,
                    color = colors.inkMuted,
                )
            }
        }
    }
}

@Composable
private fun OverviewChangesSection(
    changes: Changes?,
    changesComparison: Comparison?,
    changesLoading: Boolean,
    changesError: String?,
    totals: ChangeTotals?,
    comparisons: List<Comparison>,
    comparison: Comparison,
    baseBranch: String?,
    onSelectComparison: (Comparison) -> Unit,
    onRetryChanges: () -> Unit,
    onOpenReview: () -> Unit,
) {
    val colors = AmuxTheme.colors
    val comparisonMatches = changesComparison == null || changesComparison == comparison
    val matchedChanges = if (!changesLoading && changesError == null && comparisonMatches) {
        changes
    } else {
        null
    }
    val displayTotals = matchedChanges?.totals ?: totals

    Column(
        modifier = Modifier.fillMaxWidth(),
        verticalArrangement = Arrangement.spacedBy(Space.s + Space.xxs),
    ) {
        AmuxSectionHeader(
            title = stringResource(R.string.overview_section_changes),
            trailing = displayTotals?.let { ChatWords.files(it.files) },
        )

        if (comparisons.size > 1) {
            AmuxSegmentedChoice(
                options = comparisons,
                selected = comparison,
                label = { option -> ChatWords.comparison(option, base = baseBranch) },
                onSelect = onSelectComparison,
                tag = { option -> AskReviewTags.overviewComparison(option.name.lowercase()) },
                modifier = Modifier.padding(horizontal = Space.margin),
            )
        }

        if (changesError != null) {
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(horizontal = Space.margin)
                    .testTag(AskReviewTags.OVERVIEW_CHANGES_ERROR),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(Space.s),
            ) {
                Text(
                    text = stringResource(R.string.overview_changes_failed, changesError),
                    style = AmuxTheme.typography.detail,
                    color = colors.accent,
                    modifier = Modifier.weight(1f),
                )
                TextButton(
                    onClick = onRetryChanges,
                    modifier = Modifier.testTag(AskReviewTags.OVERVIEW_CHANGES_RETRY),
                ) {
                    Text(
                        text = stringResource(R.string.action_retry),
                        style = AmuxTheme.typography.bodyEmphasis,
                        color = colors.accent,
                    )
                }
            }
        } else if (changesLoading || (changes != null && !comparisonMatches)) {
            Text(
                text = stringResource(R.string.overview_changes_listing),
                style = AmuxTheme.typography.detail,
                color = colors.inkFaint,
                modifier = Modifier
                    .padding(horizontal = Space.margin)
                    .testTag(AskReviewTags.OVERVIEW_CHANGES_LOADING),
            )
        } else if (matchedChanges != null && matchedChanges.totals.files > 0) {
            Column(modifier = Modifier.fillMaxWidth()) {
                matchedChanges.folders.forEach { folder ->
                    OverviewFolderGroup(folder = folder)
                }
            }
            val reviewLabel = stringResource(R.string.overview_review_changes)
            Text(
                text = reviewLabel,
                style = AmuxTheme.typography.bodyEmphasis,
                color = colors.accent,
                modifier = Modifier
                    .padding(horizontal = Space.margin - Space.s)
                    .clip(RoundedCornerShape(Space.s))
                    .clickable(role = Role.Button, onClick = onOpenReview)
                    .heightIn(min = ThumbTarget.MinSize)
                    .wrapContentHeight(Alignment.CenterVertically)
                    .padding(horizontal = Space.s)
                    .testTag(AskReviewTags.OVERVIEW_REVIEW),
            )
        } else {
            val emptyMessage = if (matchedChanges == null && totals != null) {
                stringResource(R.string.overview_changes_listing)
            } else {
                stringResource(R.string.overview_changes_none)
            }
            Text(
                text = emptyMessage,
                style = AmuxTheme.typography.detail,
                color = colors.inkFaint,
                modifier = Modifier
                    .padding(horizontal = Space.margin)
                    .testTag(AskReviewTags.OVERVIEW_CHANGES_NONE),
            )
        }
    }
}

/** A folder's path, then its files under it; files at the root stand alone. Listed only: the review opens from the link under them. */
@Composable
private fun OverviewFolderGroup(folder: Folder) {
    val colors = AmuxTheme.colors
    Column(modifier = Modifier.fillMaxWidth()) {
        if (folder.path.isNotEmpty()) {
            Text(
                text = folder.path,
                style = AmuxTheme.typography.monoSmall,
                color = colors.inkFaint,
                maxLines = 1,
                overflow = TextOverflow.MiddleEllipsis,
                modifier = Modifier.padding(
                    start = Space.margin,
                    end = Space.margin,
                    top = Space.s,
                    bottom = Space.xxs,
                ),
            )
        }
        folder.files.forEach { file ->
            val counts = ChatWords.counts(added = file.added, removed = file.removed)
            val spokenFile = stringResource(R.string.chat_overview_file_spoken, file.path, counts)
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .semantics(mergeDescendants = true) {
                        contentDescription = spokenFile
                    }
                    .heightIn(min = ThumbTarget.MinSize)
                    .padding(
                        start = if (folder.path.isEmpty()) Space.margin else Space.margin + Space.m + Space.xxs,
                        end = Space.margin,
                    )
                    .testTag(AskReviewTags.overviewFile(file.path)),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(Space.s + Space.xxs),
            ) {
                Text(
                    text = file.name,
                    style = AmuxTheme.typography.mono.copy(
                        textDecoration = if (file.status == FileStatus.Deleted) {
                            TextDecoration.LineThrough
                        } else {
                            TextDecoration.None
                        },
                    ),
                    color = if (file.status == FileStatus.Deleted) colors.inkFaint else colors.ink,
                    maxLines = 1,
                    overflow = TextOverflow.StartEllipsis,
                    modifier = Modifier.weight(1f),
                )
                DiffDeltaCounts(added = file.added, removed = file.removed)
            }
        }
    }
}

