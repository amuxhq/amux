package sh.amux.features

import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
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
import androidx.compose.material.icons.outlined.ChevronRight
import androidx.compose.material.icons.outlined.Close
import androidx.compose.material.icons.outlined.RadioButtonChecked
import androidx.compose.material.icons.outlined.RadioButtonUnchecked
import androidx.compose.material.icons.outlined.StopCircle
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilterChip
import androidx.compose.material3.FilterChipDefaults
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.rememberModalBottomSheetState
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
import sh.amux.design.AmuxChrome
import sh.amux.design.AmuxHairline
import sh.amux.design.AmuxPanel
import sh.amux.design.AmuxTheme
import sh.amux.design.Space
import sh.amux.design.thumbTarget
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
    data class OpenReview(val filePath: String? = null) : ChatOverviewAction
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
) {
    val colors = AmuxTheme.colors
    val sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true)
    ModalBottomSheet(
        onDismissRequest = { onAction(ChatOverviewAction.Dismiss) },
        sheetState = sheetState,
        containerColor = colors.ground,
        contentColor = colors.ink,
    ) {
        ChatOverviewSheetContent(
            state = state,
            onAction = onAction,
        )
    }
}

/**
 * Pure Compose body of the chat overview sheet. Exposed directly so headless
 * storybook and Robolectric tests can exercise every section cleanly.
 */
@Composable
fun ChatOverviewSheetContent(
    state: ChatOverviewState,
    onAction: (ChatOverviewAction) -> Unit,
    modifier: Modifier = Modifier,
) {
    val colors = AmuxTheme.colors
    val overview = state.overview
    val now = Instant.ofEpochMilli(state.nowMs)
    var pendingStop by remember { mutableStateOf<Pair<JobRow, String>?>(null) }

    Column(
        modifier = modifier
            .fillMaxWidth()
            .background(colors.ground)
            .verticalScroll(rememberScrollState())
            .padding(bottom = Space.xl)
            .testTag(AskReviewTags.OVERVIEW_SHEET),
        verticalArrangement = Arrangement.spacedBy(Space.l),
    ) {
        OverviewPlaceHeader(
            subject = state.subject,
            onClose = { onAction(ChatOverviewAction.Dismiss) },
        )

        val tasks = overview?.tasks
        if (tasks != null && (tasks.total > 0 || tasks.entries.isNotEmpty())) {
            OverviewTasksSection(tasks = tasks)
        }

        if (overview != null && overview.jobs.isNotEmpty()) {
            OverviewJobsSection(
                jobs = overview.jobs,
                now = now,
                onRequestStop = { job, key -> pendingStop = job to key },
            )
        }

        if (overview != null && overview.failedServers.isNotEmpty()) {
            OverviewFailedServersSection(servers = overview.failedServers)
        }

        val usage = overview?.usageNearLimit
        if (usage != null) {
            OverviewUsageSection(usage = usage, now = now)
        }

        val context = state.context
        if (context != null) {
            OverviewContextSection(context = context)
        }

        OverviewChangesSection(
            changes = overview?.changes,
            totals = state.changeTotals,
            comparisons = state.comparisons,
            comparison = state.comparison,
            baseBranch = state.baseBranch,
            onSelectComparison = { onAction(ChatOverviewAction.SelectComparison(it)) },
            onOpenReview = { path -> onAction(ChatOverviewAction.OpenReview(path)) },
        )
    }

    val confirmStop = pendingStop
    if (confirmStop != null) {
        val (job, key) = confirmStop
        AlertDialog(
            onDismissRequest = { pendingStop = null },
            containerColor = colors.raised,
            titleContentColor = colors.ink,
            textContentColor = colors.inkMuted,
            title = {
                Text(
                    text = ChatWords.stopJob(job),
                    style = AmuxTheme.typography.bodyEmphasis,
                )
            },
            text = {
                Text(
                    text = ChatWords.STOP_JOB_LEAVES,
                    style = AmuxTheme.typography.detail,
                )
            },
            confirmButton = {
                TextButton(
                    onClick = {
                        pendingStop = null
                        onAction(ChatOverviewAction.StopJob(key))
                    },
                    modifier = Modifier
                        .thumbTarget()
                        .testTag(AskReviewTags.OVERVIEW_STOP_CONFIRM),
                ) {
                    Text(
                        text = stringResource(R.string.overview_stop),
                        style = AmuxTheme.typography.bodyEmphasis,
                        color = colors.removed,
                    )
                }
            },
            dismissButton = {
                TextButton(
                    onClick = { pendingStop = null },
                    modifier = Modifier
                        .thumbTarget()
                        .testTag(AskReviewTags.OVERVIEW_STOP_CANCEL),
                ) {
                    Text(
                        text = stringResource(R.string.action_cancel),
                        style = AmuxTheme.typography.body,
                        color = colors.inkMuted,
                    )
                }
            },
            modifier = Modifier.testTag(AskReviewTags.OVERVIEW_STOP_DIALOG),
        )
    }
}

@Composable
private fun OverviewPlaceHeader(
    subject: ChatSubjectUi,
    onClose: () -> Unit,
) {
    val colors = AmuxTheme.colors
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = Space.margin)
            .testTag(AskReviewTags.OVERVIEW_PLACE),
        verticalArrangement = Arrangement.spacedBy(Space.s),
    ) {
        Row(
            modifier = Modifier.fillMaxWidth(),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text(
                text = subject.name,
                style = AmuxTheme.typography.screenTitle,
                color = colors.ink,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.weight(1f),
            )
            val closeLabel = stringResource(R.string.action_close)
            IconButton(
                onClick = onClose,
                modifier = Modifier
                    .thumbTarget()
                    .testTag(AskReviewTags.OVERVIEW_CLOSE),
            ) {
                Icon(
                    imageVector = Icons.Outlined.Close,
                    contentDescription = closeLabel,
                    tint = colors.inkMuted,
                    modifier = Modifier.size(20.dp),
                )
            }
        }
        Column(verticalArrangement = Arrangement.spacedBy(Space.xs)) {
            if (subject.host.isNotEmpty()) {
                PlaceFactRow(label = stringResource(R.string.overview_label_host), value = subject.host)
            }
            if (subject.directory.isNotEmpty()) {
                PlaceFactRow(label = stringResource(R.string.overview_label_directory), value = subject.directory)
            }
            if (!subject.branch.isNullOrEmpty()) {
                PlaceFactRow(label = stringResource(R.string.overview_label_branch), value = subject.branch)
            }
            if (subject.address.isNotEmpty()) {
                PlaceFactRow(label = stringResource(R.string.overview_label_address), value = subject.address)
            }
        }
    }
}

@Composable
private fun PlaceFactRow(
    label: String,
    value: String,
) {
    val colors = AmuxTheme.colors
    Row(
        modifier = Modifier.fillMaxWidth(),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(Space.m),
    ) {
        Text(
            text = label,
            style = AmuxTheme.typography.caption,
            color = colors.inkFaint,
        )
        SelectionContainer(modifier = Modifier.weight(1f)) {
            Text(
                text = value,
                style = AmuxTheme.typography.monoSmall,
                color = colors.inkMuted,
                maxLines = 2,
                overflow = TextOverflow.Ellipsis,
            )
        }
    }
}

@Composable
private fun OverviewTasksSection(tasks: TasksView) {
    val colors = AmuxTheme.colors
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .testTag(AskReviewTags.OVERVIEW_TASKS),
    ) {
        AmuxSectionHeader(
            title = stringResource(R.string.overview_section_tasks),
            trailing = "${tasks.done}/${tasks.total}",
        )
        AmuxPanel(
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = Space.margin),
        ) {
            Column(
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(Space.m),
                verticalArrangement = Arrangement.spacedBy(Space.s),
            ) {
                tasks.entries.forEachIndexed { index, entry ->
                    val icon = when (entry.mark) {
                        TaskMark.Done -> Icons.Outlined.CheckCircle
                        TaskMark.Current -> Icons.Outlined.RadioButtonChecked
                        TaskMark.Todo -> Icons.Outlined.RadioButtonUnchecked
                    }
                    val iconTint = when (entry.mark) {
                        TaskMark.Done -> colors.inkFaint
                        TaskMark.Current -> colors.ink
                        TaskMark.Todo -> colors.inkMuted
                    }
                    Row(
                        modifier = Modifier
                            .fillMaxWidth()
                            .testTag(AskReviewTags.overviewTask(index)),
                        verticalAlignment = Alignment.CenterVertically,
                        horizontalArrangement = Arrangement.spacedBy(Space.s),
                    ) {
                        Icon(
                            imageVector = icon,
                            contentDescription = null,
                            tint = iconTint,
                            modifier = Modifier.size(16.dp),
                        )
                        Text(
                            text = entry.subject,
                            style = if (entry.mark == TaskMark.Current) {
                                AmuxTheme.typography.bodyEmphasis
                            } else {
                                AmuxTheme.typography.body
                            },
                            color = if (entry.mark == TaskMark.Done) colors.inkFaint else colors.ink,
                            modifier = Modifier.weight(1f),
                        )
                    }
                }
            }
        }
    }
}

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
        AmuxPanel(
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = Space.margin),
        ) {
            Column(modifier = Modifier.fillMaxWidth()) {
                jobs.forEachIndexed { index, job ->
                    if (index > 0) AmuxHairline()
                    val stopKey = when (val stop = job.stop) {
                        is JobStop.Offered -> stop.key
                        is JobStop.Refused -> stop.key
                        JobStop.Stopping, JobStop.Unavailable -> null
                    }
                    val elapsed = ChatWords.ran(sinceMs = job.startedAtMs, now = now)
                    Column(
                        modifier = Modifier
                            .fillMaxWidth()
                            .padding(horizontal = Space.m, vertical = Space.s)
                            .testTag(AskReviewTags.overviewJob(index)),
                        verticalArrangement = Arrangement.spacedBy(Space.xxs),
                    ) {
                        Row(
                            modifier = Modifier.fillMaxWidth(),
                            verticalAlignment = Alignment.CenterVertically,
                            horizontalArrangement = Arrangement.spacedBy(Space.s),
                        ) {
                            Text(
                                text = ChatWords.firstLine(job.command),
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
                                val stopDescription = stringResource(
                                    R.string.overview_stop_job_description,
                                    ChatWords.firstLine(job.command),
                                )
                                IconButton(
                                    onClick = { onRequestStop(job, stopKey) },
                                    modifier = Modifier
                                        .thumbTarget()
                                        .testTag(AskReviewTags.overviewJobStop(index)),
                                ) {
                                    Icon(
                                        imageVector = Icons.Outlined.StopCircle,
                                        contentDescription = stopDescription,
                                        tint = colors.inkMuted,
                                        modifier = Modifier.size(20.dp),
                                    )
                                }
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
                            )
                        }
                    }
                }
            }
        }
    }
}

@Composable
private fun OverviewFailedServersSection(servers: List<ServerView>) {
    val colors = AmuxTheme.colors
    Column(modifier = Modifier.fillMaxWidth()) {
        AmuxSectionHeader(
            title = stringResource(R.string.overview_section_servers),
            trailing = servers.size.toString(),
        )
        AmuxPanel(
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = Space.margin),
            borderColor = colors.accent.copy(alpha = 0.45f),
        ) {
            Column(modifier = Modifier.fillMaxWidth()) {
                servers.forEachIndexed { index, server ->
                    if (index > 0) AmuxHairline()
                    Column(
                        modifier = Modifier
                            .fillMaxWidth()
                            .padding(horizontal = Space.m, vertical = Space.s)
                            .testTag(AskReviewTags.overviewServer(server.name)),
                        verticalArrangement = Arrangement.spacedBy(Space.xxs),
                    ) {
                        Text(
                            text = ChatWords.failed(server),
                            style = AmuxTheme.typography.bodyEmphasis,
                            color = colors.accent,
                        )
                        if (server.error.isNotEmpty()) {
                            Text(
                                text = ChatWords.firstLine(server.error),
                                style = AmuxTheme.typography.caption,
                                color = colors.inkMuted,
                                maxLines = 2,
                                overflow = TextOverflow.Ellipsis,
                            )
                        }
                    }
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
    ) {
        AmuxSectionHeader(
            title = stringResource(R.string.overview_section_usage),
            trailing = stringResource(
                if (usage.blocked) R.string.overview_usage_blocked else R.string.overview_usage_near_limit,
            ),
        )
        AmuxPanel(
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = Space.margin),
            borderColor = if (usage.blocked) colors.accent else colors.hairline,
        ) {
            Column(
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(Space.m),
                verticalArrangement = Arrangement.spacedBy(Space.s),
            ) {
                usage.windows.forEachIndexed { index, window ->
                    val label = ChatWords.usageLabel(window.label)
                    val detail = ChatWords.usageDetail(window, now = now)
                    val rowColor = when (window.state) {
                        UsageState.Blocked -> colors.accent
                        UsageState.NearLimit -> colors.ink
                        else -> colors.inkMuted
                    }
                    Column(
                        modifier = Modifier
                            .fillMaxWidth()
                            .testTag(AskReviewTags.overviewUsageWindow(index)),
                        verticalArrangement = Arrangement.spacedBy(Space.xxs),
                    ) {
                        Row(
                            modifier = Modifier.fillMaxWidth(),
                            verticalAlignment = Alignment.CenterVertically,
                        ) {
                            Text(
                                text = label,
                                style = AmuxTheme.typography.body,
                                color = rowColor,
                                modifier = Modifier.weight(1f),
                            )
                            Text(
                                text = ChatWords.used(window),
                                style = AmuxTheme.typography.bodyEmphasis,
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
                    )
                }
            }
        }
    }
}

@Composable
private fun OverviewContextSection(context: ContextView) {
    val colors = AmuxTheme.colors
    val trailing = ChatWords.contextLeft(context)
        ?: context.percent?.let { stringResource(R.string.overview_context_percent, it) }

    Column(
        modifier = Modifier
            .fillMaxWidth()
            .testTag(AskReviewTags.OVERVIEW_CONTEXT),
    ) {
        AmuxSectionHeader(
            title = stringResource(R.string.overview_section_context),
            trailing = trailing,
        )
        AmuxPanel(
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = Space.margin),
            borderColor = if (context.nearFull) colors.accent.copy(alpha = 0.55f) else colors.hairline,
        ) {
            ContextMeterBlock(
                context = context,
                modifier = Modifier.padding(Space.m),
            )
        }
    }
}

/**
 * Static horizontal context capacity meter. Draws a crisp neutral bar (switching
 * to the attention accent at or above 80% used) without using Material progress
 * indicator spinners.
 */
@Composable
internal fun ContextMeterBlock(
    context: ContextView,
    modifier: Modifier = Modifier,
) {
    val colors = AmuxTheme.colors
    val fraction = ((context.percent ?: 0L).coerceIn(0L, 100L)) / 100f
    val fillColor = if (context.nearFull) colors.accent else colors.inkMuted

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
                            .background(fillColor),
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
                color = if (context.nearFull) colors.accent else colors.ink,
                modifier = Modifier.weight(1f),
            )
            val percent = context.percent
            if (percent != null) {
                Text(
                    text = stringResource(R.string.overview_context_percent, percent),
                    style = AmuxTheme.typography.monoSmall,
                    color = if (context.nearFull) colors.accent else colors.inkMuted,
                )
            }
        }
    }
}

@Composable
private fun OverviewChangesSection(
    changes: Changes?,
    totals: ChangeTotals?,
    comparisons: List<Comparison>,
    comparison: Comparison,
    baseBranch: String?,
    onSelectComparison: (Comparison) -> Unit,
    onOpenReview: (String?) -> Unit,
) {
    val colors = AmuxTheme.colors
    val displayTotals = changes?.totals ?: totals

    Column(
        modifier = Modifier.fillMaxWidth(),
        verticalArrangement = Arrangement.spacedBy(Space.s),
    ) {
        AmuxSectionHeader(
            title = stringResource(R.string.overview_section_changes),
            trailing = displayTotals?.let { ChatWords.files(it.files) },
        )

        if (comparisons.size > 1) {
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(horizontal = Space.margin),
                horizontalArrangement = Arrangement.spacedBy(Space.s),
            ) {
                comparisons.forEach { option ->
                    val selected = option == comparison
                    FilterChip(
                        selected = selected,
                        onClick = { onSelectComparison(option) },
                        label = {
                            Text(
                                text = ChatWords.comparison(option, base = baseBranch),
                                style = AmuxTheme.typography.caption,
                            )
                        },
                        colors = FilterChipDefaults.filterChipColors(
                            selectedContainerColor = colors.ink,
                            selectedLabelColor = colors.ground,
                            containerColor = colors.sunken,
                            labelColor = colors.inkMuted,
                        ),
                        border = FilterChipDefaults.filterChipBorder(
                            enabled = true,
                            selected = selected,
                            borderColor = colors.hairline,
                            selectedBorderColor = colors.ink,
                        ),
                        modifier = Modifier
                            .thumbTarget()
                            .testTag(AskReviewTags.overviewComparison(option.name.lowercase())),
                    )
                }
            }
        }

        if (changes != null && changes.totals.files > 0) {
            AmuxPanel(
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(horizontal = Space.margin),
            ) {
                Column(modifier = Modifier.fillMaxWidth()) {
                    changes.folders.forEachIndexed { folderIndex, folder ->
                        if (folderIndex > 0) AmuxHairline()
                        OverviewFolderGroup(
                            folder = folder,
                            onSelectFile = { onOpenReview(it) },
                        )
                    }
                }
            }
            Box(
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(horizontal = Space.margin),
            ) {
                AmuxActionButton(
                    label = stringResource(R.string.overview_review_changes),
                    kind = ActionButtonKind.Outline,
                    fillWidth = true,
                    onClick = { onOpenReview(null) },
                    modifier = Modifier.testTag(AskReviewTags.OVERVIEW_REVIEW),
                )
            }
        } else {
            val emptyMessage = if (changes == null && totals != null) {
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

@Composable
private fun OverviewFolderGroup(
    folder: Folder,
    onSelectFile: (String) -> Unit,
) {
    val colors = AmuxTheme.colors
    Column(modifier = Modifier.fillMaxWidth()) {
        if (folder.path.isNotEmpty()) {
            Text(
                text = folder.path,
                style = AmuxTheme.typography.monoSmall,
                color = colors.inkFaint,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.padding(
                    start = Space.m,
                    end = Space.m,
                    top = Space.s,
                    bottom = Space.xxs,
                ),
            )
        }
        folder.files.forEach { file ->
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .clickable(role = Role.Button) { onSelectFile(file.path) }
                    .thumbTarget()
                    .padding(
                        start = if (folder.path.isEmpty()) Space.m else Space.xl,
                        end = Space.m,
                        top = Space.xs,
                        bottom = Space.xs,
                    )
                    .testTag(AskReviewTags.overviewFile(file.path)),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(Space.s),
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
                    overflow = TextOverflow.Ellipsis,
                    modifier = Modifier.weight(1f),
                )
                DiffDeltaCounts(added = file.added, removed = file.removed)
            }
        }
    }
}

/**
 * Compact folded overview strip docked above the composer whenever tasks,
 * background jobs, failed tool servers, usage warnings, near-full context, or
 * changed files are active.
 */
@Composable
fun ChatOverviewStrip(
    overview: Overview?,
    context: ContextView?,
    changeTotals: ChangeTotals? = null,
    nowMs: Long = 0L,
    onOpenOverview: () -> Unit,
    onOpenReview: (() -> Unit)? = null,
    modifier: Modifier = Modifier,
) {
    val tasks = overview?.tasks?.takeIf { it.total > 0 || it.entries.isNotEmpty() }
    val jobCount = overview?.jobs?.size ?: 0
    val failedCount = overview?.failedServers?.size ?: 0
    val usage = overview?.usageNearLimit
    val contextWarning = ChatWords.contextLeft(context)
    val totals = overview?.changes?.totals ?: changeTotals
    val changedFiles = totals?.files ?: 0

    val hasAny = tasks != null ||
        jobCount > 0 ||
        failedCount > 0 ||
        usage != null ||
        contextWarning != null ||
        changedFiles > 0
    if (!hasAny) return

    val colors = AmuxTheme.colors
    val openOverviewLabel = stringResource(R.string.overview_strip_open)

    AmuxChrome(
        modifier = modifier
            .fillMaxWidth()
            .testTag(AskReviewTags.OVERVIEW_STRIP),
        shape = RoundedCornerShape(AmuxTheme.metrics.cardRadius),
    ) {
        Column(modifier = Modifier.fillMaxWidth()) {
            var drewRow = false
            if (tasks != null) {
                drewRow = true
                StripRow(
                    leading = "${tasks.done}/${tasks.total}",
                    text = ChatWords.headTask(tasks),
                    accent = false,
                    description = openOverviewLabel,
                    onClick = onOpenOverview,
                )
            }
            if (jobCount > 0) {
                if (drewRow) AmuxHairline()
                drewRow = true
                StripRow(
                    leading = null,
                    text = ChatWords.inBackground(jobCount),
                    accent = false,
                    description = openOverviewLabel,
                    onClick = onOpenOverview,
                )
            }
            if (failedCount > 0) {
                if (drewRow) AmuxHairline()
                drewRow = true
                val label = if (failedCount == 1) {
                    stringResource(R.string.overview_strip_failed_one)
                } else {
                    stringResource(R.string.overview_strip_failed_many, failedCount)
                }
                StripRow(
                    leading = null,
                    text = label,
                    accent = true,
                    description = openOverviewLabel,
                    onClick = onOpenOverview,
                )
            }
            if (usage != null) {
                if (drewRow) AmuxHairline()
                drewRow = true
                val label = if (usage.blocked) {
                    ChatWords.limitReached(usage, now = Instant.ofEpochMilli(nowMs))
                } else {
                    stringResource(R.string.overview_strip_usage_near)
                }
                StripRow(
                    leading = null,
                    text = label,
                    accent = true,
                    description = openOverviewLabel,
                    onClick = onOpenOverview,
                )
            }
            if (contextWarning != null) {
                if (drewRow) AmuxHairline()
                drewRow = true
                StripRow(
                    leading = null,
                    text = contextWarning,
                    accent = true,
                    description = openOverviewLabel,
                    onClick = onOpenOverview,
                )
            }
            if (changedFiles > 0 && totals != null) {
                if (drewRow) AmuxHairline()
                Row(
                    modifier = Modifier
                        .fillMaxWidth()
                        .clickable(role = Role.Button) {
                            if (onOpenReview != null) onOpenReview() else onOpenOverview()
                        }
                        .thumbTarget()
                        .padding(horizontal = Space.m, vertical = Space.s)
                        .testTag(AskReviewTags.OVERVIEW_STRIP_CHANGES),
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(Space.s),
                ) {
                    Text(
                        text = ChatWords.files(changedFiles),
                        style = AmuxTheme.typography.body,
                        color = colors.ink,
                        modifier = Modifier.weight(1f),
                    )
                    DiffDeltaCounts(added = totals.added, removed = totals.removed)
                    Icon(
                        imageVector = Icons.Outlined.ChevronRight,
                        contentDescription = null,
                        tint = colors.inkFaint,
                        modifier = Modifier.size(16.dp),
                    )
                }
            }
        }
    }
}

@Composable
private fun StripRow(
    leading: String?,
    text: String,
    accent: Boolean,
    description: String,
    onClick: () -> Unit,
) {
    val colors = AmuxTheme.colors
    val spoken = stringResource(R.string.overview_strip_spoken, text, description)
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .clickable(role = Role.Button, onClick = onClick)
            .thumbTarget()
            .padding(horizontal = Space.m, vertical = Space.s)
            .semantics { contentDescription = spoken },
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(Space.s),
    ) {
        if (leading != null) {
            Text(
                text = leading,
                style = AmuxTheme.typography.monoSmall,
                color = colors.inkMuted,
            )
        }
        Text(
            text = text,
            style = if (accent) AmuxTheme.typography.bodyEmphasis else AmuxTheme.typography.body,
            color = if (accent) colors.accent else colors.ink,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
            modifier = Modifier.weight(1f),
        )
        Icon(
            imageVector = Icons.Outlined.ChevronRight,
            contentDescription = null,
            tint = colors.inkFaint,
            modifier = Modifier.size(16.dp),
        )
    }
}
