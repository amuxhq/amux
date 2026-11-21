package sh.amux.features

import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.clickable
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.ArrowBack
import androidx.compose.material.icons.outlined.Close
import androidx.compose.material.icons.outlined.DeleteOutline
import androidx.compose.material.icons.outlined.MoreVert
import androidx.compose.material.icons.outlined.Search
import androidx.compose.material.icons.outlined.Unarchive
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.OutlinedTextFieldDefaults
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.TopAppBarDefaults
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Immutable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import java.time.Instant
import kotlinx.coroutines.launch
import sh.amux.design.AmuxGround
import sh.amux.design.AmuxHairline
import sh.amux.design.AmuxTheme
import sh.amux.design.Space
import sh.amux.design.thumbTarget
import sh.amux.values.AgentKey
import sh.amux.values.DeleteWarning
import sh.amux.values.FleetRow
import sh.amux.words.DeleteWords
import sh.amux.words.FleetWords
import sh.amux.words.PlaceNames
import sh.amux.words.RowState
import sh.amux.words.age
import sh.amux.words.deletable
import sh.amux.words.hostName
import sh.amux.words.id
import sh.amux.words.name
import sh.amux.words.readable
import sh.amux.words.workingDirectory

/** Semantic test tags for [SettledScreen]. */
object SettledTags {
    const val SCREEN: String = "settled"
    const val BACK: String = "settled.back"
    const val SUBTITLE: String = "settled.subtitle"
    const val SEARCH_FIELD: String = "settled.search"
    const val SEARCH_CLEAR: String = "settled.search.clear"
    const val LIST: String = "settled.list"
    const val EMPTY: String = "settled.empty"
    const val NO_MATCHES: String = "settled.no-matches"
    const val NO_MATCHES_CLEAR: String = "settled.no-matches.clear"
    const val ACTION_SHEET: String = "settled.action-sheet"
    const val ACTION_UNSETTLE: String = "settled.action.unsettle"
    const val ACTION_DELETE: String = "settled.action.delete"
    const val DELETE_DIALOG: String = "settled.delete.dialog"
    const val DELETE_CONFIRM: String = "settled.delete.confirm"
    const val DELETE_CANCEL: String = "settled.delete.cancel"

    fun row(name: String): String = "settled.row.$name"
    fun rowMenu(name: String): String = "settled.row.$name.menu"
}

/** Pure immutable UI state rendered by [SettledScreen]. */
@Immutable
data class SettledScreenState(
    val rows: List<FleetRow> = emptyList(),
    val query: String = "",
    val nowMs: Long = 0L,
) {
    val filteredRows: List<FleetRow>
        get() {
            val wanted = query.trim().lowercase()
            if (wanted.isEmpty()) return rows
            return rows.filter { row ->
                row.name.lowercase().contains(wanted) ||
                    row.card.handle.lowercase().contains(wanted) ||
                    row.hostName.lowercase().contains(wanted) ||
                    row.workingDirectory.lowercase().contains(wanted) ||
                    row.card.repository?.name?.lowercase()?.contains(wanted) == true
            }
        }
}

/** Pure actions emitted by [SettledScreen]. */
sealed interface SettledAction {
    data object Back : SettledAction
    data class Open(val agent: AgentKey) : SettledAction
    data class UpdateQuery(val query: String) : SettledAction
    data class Unsettle(val agent: AgentKey) : SettledAction
    data class Delete(val agent: AgentKey, val discardingChanges: Boolean = false) : SettledAction
}

private data class PendingSettledDelete(
    val row: FleetRow,
    val warning: DeleteWarning?,
)

/**
 * Full-screen Settled agents archive showing settled agents with host, folder,
 * and last-active timestamp, search filtering, tap-to-open, Un-settle, and
 * Delete with confirmation dialog.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SettledScreen(
    state: SettledScreenState,
    onAction: (SettledAction) -> Unit,
    modifier: Modifier = Modifier,
    onPreviewDelete: (suspend (AgentKey) -> Result<DeleteWarning?>)? = null,
) {
    val colors = AmuxTheme.colors
    val coroutineScope = rememberCoroutineScope()
    val nowInstant = remember(state.nowMs) { Instant.ofEpochMilli(state.nowMs) }
    val filtered = remember(state.rows, state.query) { state.filteredRows }

    var actionRow by remember { mutableStateOf<FleetRow?>(null) }
    var pendingDelete by remember { mutableStateOf<PendingSettledDelete?>(null) }

    val subtitle = when (state.rows.size) {
        0 -> null
        1 -> stringResource(R.string.settled_count_one)
        else -> stringResource(R.string.settled_count_many, state.rows.size)
    }

    AmuxGround(
        modifier = modifier.testTag(SettledTags.SCREEN),
    ) {
        Scaffold(
            containerColor = Color.Transparent,
            contentColor = colors.ink,
            topBar = {
                Column {
                    TopAppBar(
                        colors = TopAppBarDefaults.topAppBarColors(
                            containerColor = colors.ground,
                            titleContentColor = colors.ink,
                            navigationIconContentColor = colors.ink,
                        ),
                        navigationIcon = {
                            val backLabel = stringResource(R.string.action_back)
                            IconButton(
                                onClick = { onAction(SettledAction.Back) },
                                modifier = Modifier
                                    .thumbTarget()
                                    .testTag(SettledTags.BACK),
                            ) {
                                Icon(
                                    imageVector = Icons.AutoMirrored.Outlined.ArrowBack,
                                    contentDescription = backLabel,
                                )
                            }
                        },
                        title = {
                            Column {
                                Text(
                                    text = stringResource(R.string.settled_title),
                                    style = AmuxTheme.typography.screenTitle,
                                    color = colors.ink,
                                    maxLines = 1,
                                    overflow = TextOverflow.Ellipsis,
                                )
                                if (subtitle != null) {
                                    Text(
                                        text = subtitle,
                                        style = AmuxTheme.typography.caption,
                                        color = colors.inkMuted,
                                        maxLines = 1,
                                        overflow = TextOverflow.Ellipsis,
                                        modifier = Modifier.testTag(SettledTags.SUBTITLE),
                                    )
                                }
                            }
                        },
                    )
                    if (state.rows.isNotEmpty() || state.query.isNotEmpty()) {
                        SettledSearchBar(
                            query = state.query,
                            onQueryChange = { onAction(SettledAction.UpdateQuery(it)) },
                        )
                    }
                    AmuxHairline(color = colors.hairline.copy(alpha = 0.6f))
                }
            },
        ) { innerPadding ->
            when {
                state.rows.isEmpty() -> {
                    Column(
                        modifier = Modifier
                            .fillMaxSize()
                            .padding(innerPadding)
                            .padding(horizontal = Space.margin, vertical = Space.xl)
                            .testTag(SettledTags.EMPTY),
                        verticalArrangement = Arrangement.spacedBy(Space.s),
                    ) {
                        Text(
                            text = stringResource(R.string.settled_empty_title),
                            style = AmuxTheme.typography.screenTitle,
                            color = colors.ink,
                        )
                        Text(
                            text = stringResource(R.string.settled_empty_detail),
                            style = AmuxTheme.typography.detail,
                            color = colors.inkMuted,
                        )
                    }
                }
                filtered.isEmpty() -> {
                    Column(
                        modifier = Modifier
                            .fillMaxSize()
                            .padding(innerPadding)
                            .padding(horizontal = Space.margin, vertical = Space.xl),
                        verticalArrangement = Arrangement.spacedBy(Space.l),
                    ) {
                        Column(verticalArrangement = Arrangement.spacedBy(Space.s)) {
                            Text(
                                text = stringResource(R.string.settled_no_matches_title),
                                style = AmuxTheme.typography.screenTitle,
                                color = colors.ink,
                                modifier = Modifier.testTag(SettledTags.NO_MATCHES),
                            )
                            Text(
                                text = stringResource(R.string.settled_no_matches_detail),
                                style = AmuxTheme.typography.detail,
                                color = colors.inkMuted,
                            )
                        }
                        AmuxActionButton(
                            label = stringResource(R.string.action_clear),
                            kind = ActionButtonKind.Outline,
                            onClick = { onAction(SettledAction.UpdateQuery("")) },
                            modifier = Modifier.testTag(SettledTags.NO_MATCHES_CLEAR),
                        )
                    }
                }
                else -> {
                    LazyColumn(
                        modifier = Modifier
                            .fillMaxSize()
                            .testTag(SettledTags.LIST),
                        contentPadding = PaddingValues(
                            top = innerPadding.calculateTopPadding() + Space.xs,
                            bottom = innerPadding.calculateBottomPadding() + Space.xxl,
                        ),
                    ) {
                        itemsIndexed(
                            items = filtered,
                            key = { _, row -> "${row.card.agent.host}-${row.card.agent.agent}" },
                        ) { index, row ->
                            SettledAgentRow(
                                row = row,
                                now = nowInstant,
                                showDivider = index < filtered.lastIndex,
                                onOpen = { onAction(SettledAction.Open(row.id)) },
                                onOpenMenu = { actionRow = row },
                            )
                        }
                    }
                }
            }
        }
    }

    // Row actions bottom sheet
    val currentRow = actionRow
    if (currentRow != null) {
        val sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true)
        ModalBottomSheet(
            onDismissRequest = { actionRow = null },
            sheetState = sheetState,
            containerColor = colors.raised,
            contentColor = colors.ink,
        ) {
            Column(
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(bottom = Space.xl)
                    .testTag(SettledTags.ACTION_SHEET),
            ) {
                Text(
                    text = currentRow.name,
                    style = AmuxTheme.typography.identifier,
                    color = colors.ink,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                    modifier = Modifier.padding(horizontal = Space.margin, vertical = Space.s),
                )
                AmuxHairline()
                SettledSheetActionRow(
                    icon = Icons.Outlined.Unarchive,
                    label = stringResource(R.string.settled_action_unsettle),
                    destructive = false,
                    onClick = {
                        actionRow = null
                        onAction(SettledAction.Unsettle(currentRow.id))
                    },
                    modifier = Modifier.testTag(SettledTags.ACTION_UNSETTLE),
                )
                if (currentRow.deletable) {
                    AmuxInsetHairline()
                    SettledSheetActionRow(
                        icon = Icons.Outlined.DeleteOutline,
                        label = stringResource(R.string.settled_action_delete),
                        destructive = true,
                        onClick = {
                            actionRow = null
                            val initialWarning = DeleteWords.asking(currentRow.card.workspaceGoes)
                            pendingDelete = PendingSettledDelete(currentRow, initialWarning)
                            if (initialWarning != null && onPreviewDelete != null) {
                                coroutineScope.launch {
                                    val answer = onPreviewDelete(currentRow.id)
                                    val active = pendingDelete
                                    if (active != null && active.row.id == currentRow.id) {
                                        pendingDelete = active.copy(
                                            warning = DeleteWords.answered(active.warning, answer),
                                        )
                                    }
                                }
                            }
                        },
                        modifier = Modifier.testTag(SettledTags.ACTION_DELETE),
                    )
                }
            }
        }
    }

    // Delete confirmation dialog
    val deleteTarget = pendingDelete
    if (deleteTarget != null) {
        val message = DeleteWords.message(
            descendants = deleteTarget.row.card.descendants,
            warning = deleteTarget.warning,
        )
        AlertDialog(
            onDismissRequest = { pendingDelete = null },
            containerColor = colors.raised,
            titleContentColor = colors.ink,
            textContentColor = colors.inkMuted,
            modifier = Modifier.testTag(SettledTags.DELETE_DIALOG),
            title = {
                Text(
                    text = stringResource(R.string.settled_delete_title, deleteTarget.row.name),
                    style = AmuxTheme.typography.identifier,
                )
            },
            text = {
                Text(
                    text = message,
                    style = AmuxTheme.typography.detail,
                    color = colors.inkMuted,
                )
            },
            confirmButton = {
                TextButton(
                    onClick = {
                        pendingDelete = null
                        onAction(
                            SettledAction.Delete(
                                agent = deleteTarget.row.id,
                                discardingChanges = DeleteWords.discards(deleteTarget.warning),
                            ),
                        )
                    },
                    enabled = !DeleteWords.awaitsAnswer(deleteTarget.warning),
                    modifier = Modifier.testTag(SettledTags.DELETE_CONFIRM),
                ) {
                    Text(
                        text = stringResource(R.string.settled_delete_confirm),
                        style = AmuxTheme.typography.bodyEmphasis,
                        color = colors.removed,
                    )
                }
            },
            dismissButton = {
                TextButton(
                    onClick = { pendingDelete = null },
                    modifier = Modifier.testTag(SettledTags.DELETE_CANCEL),
                ) {
                    Text(
                        text = stringResource(R.string.action_cancel),
                        style = AmuxTheme.typography.body,
                        color = colors.inkMuted,
                    )
                }
            },
        )
    }
}

@Composable
private fun SettledSearchBar(
    query: String,
    onQueryChange: (String) -> Unit,
) {
    val colors = AmuxTheme.colors
    OutlinedTextField(
        value = query,
        onValueChange = onQueryChange,
        singleLine = true,
        placeholder = {
            Text(
                text = stringResource(R.string.settled_search_placeholder),
                style = AmuxTheme.typography.detail,
                color = colors.inkFaint,
            )
        },
        leadingIcon = {
            Icon(
                imageVector = Icons.Outlined.Search,
                contentDescription = null,
                tint = colors.inkMuted,
                modifier = Modifier.size(18.dp),
            )
        },
        trailingIcon = {
            if (query.isNotEmpty()) {
                val clearLabel = stringResource(R.string.settled_search_clear)
                IconButton(
                    onClick = { onQueryChange("") },
                    modifier = Modifier
                        .thumbTarget()
                        .testTag(SettledTags.SEARCH_CLEAR),
                ) {
                    Icon(
                        imageVector = Icons.Outlined.Close,
                        contentDescription = clearLabel,
                        tint = colors.inkMuted,
                        modifier = Modifier.size(18.dp),
                    )
                }
            }
        },
        textStyle = AmuxTheme.typography.body.copy(color = colors.ink),
        shape = RoundedCornerShape(AmuxTheme.metrics.controlRadius),
        colors = OutlinedTextFieldDefaults.colors(
            focusedContainerColor = colors.raised,
            unfocusedContainerColor = colors.raised,
            focusedBorderColor = colors.ink,
            unfocusedBorderColor = colors.hairline,
            focusedTextColor = colors.ink,
            unfocusedTextColor = colors.ink,
            cursorColor = colors.ink,
        ),
        modifier = Modifier
            .fillMaxWidth()
            .padding(start = Space.margin, end = Space.margin, bottom = Space.s)
            .testTag(SettledTags.SEARCH_FIELD),
    )
}

@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun SettledAgentRow(
    row: FleetRow,
    now: Instant,
    showDivider: Boolean,
    onOpen: () -> Unit,
    onOpenMenu: () -> Unit,
) {
    val colors = AmuxTheme.colors
    val typography = AmuxTheme.typography
    val rowState = remember(row) { RowState.of(row) }
    val isLargeText = LocalDensity.current.fontScale >= 1.3f

    val folder = row.workingDirectory.takeIf { it.isNotEmpty() }?.let(PlaceNames::directory)
    val host = row.hostName.takeIf { it.isNotEmpty() }?.let(PlaceNames::host)
    val age = row.age(at = now)
    val placeLine = listOfNotNull(folder, host, if (isLargeText) age else null).joinToString(" · ")
    val secondLine = FleetWords.secondLine(
        line = row.secondLine,
        kind = row.card.kind,
        now = now,
        backgroundJobs = row.card.backgroundJobs,
    )

    val clickModifier = if (row.readable) {
        Modifier.combinedClickable(
            onClick = onOpen,
            onLongClick = onOpenMenu,
            role = Role.Button,
        )
    } else {
        Modifier.combinedClickable(
            onClick = {},
            onLongClick = onOpenMenu,
        )
    }

    Column(
        modifier = Modifier
            .fillMaxWidth()
            .testTag(SettledTags.row(row.name)),
    ) {
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .then(clickModifier)
                .padding(
                    start = Space.margin,
                    end = Space.xs,
                    top = Space.m,
                    bottom = Space.m,
                ),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Box(
                modifier = Modifier
                    .width(20.dp)
                    .padding(top = 2.dp),
                contentAlignment = Alignment.CenterStart,
            ) {
                AmuxStateMark(state = rowState, size = 10.dp)
            }

            Column(
                modifier = Modifier.weight(1f),
                verticalArrangement = Arrangement.spacedBy(Space.xxs),
            ) {
                Row(
                    modifier = Modifier.fillMaxWidth(),
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(Space.s),
                ) {
                    Text(
                        text = row.name,
                        style = typography.identifier,
                        color = colors.ink,
                        maxLines = if (isLargeText) 2 else 1,
                        overflow = TextOverflow.Ellipsis,
                        modifier = Modifier.weight(1f),
                    )
                    if (!isLargeText) {
                        Text(
                            text = age,
                            style = typography.caption,
                            color = colors.inkFaint,
                            maxLines = 1,
                        )
                    }
                }
                if (placeLine.isNotEmpty()) {
                    Text(
                        text = placeLine,
                        style = typography.monoSmall,
                        color = colors.inkMuted,
                        maxLines = if (isLargeText) 2 else 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
                if (secondLine != null) {
                    val secondColor = when (secondLine.ink) {
                        FleetWords.Ink.Error -> colors.removed
                        FleetWords.Ink.Warning -> colors.accent
                        FleetWords.Ink.Ask -> colors.ink
                        FleetWords.Ink.Quiet -> colors.inkMuted
                    }
                    Text(
                        text = secondLine.text,
                        style = typography.detail,
                        color = secondColor,
                        maxLines = 2,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
            }

            val moreDesc = stringResource(R.string.action_more_options)
            IconButton(
                onClick = onOpenMenu,
                modifier = Modifier
                    .thumbTarget()
                    .testTag(SettledTags.rowMenu(row.name)),
            ) {
                Icon(
                    imageVector = Icons.Outlined.MoreVert,
                    contentDescription = moreDesc,
                    tint = colors.inkFaint,
                    modifier = Modifier.size(18.dp),
                )
            }
        }
        if (showDivider) {
            AmuxInsetHairline(leadingInset = Space.margin + 20.dp)
        }
    }
}

@Composable
private fun SettledSheetActionRow(
    icon: ImageVector,
    label: String,
    destructive: Boolean,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val colors = AmuxTheme.colors
    val tint = if (destructive) colors.removed else colors.ink
    Row(
        modifier = modifier
            .fillMaxWidth()
            .thumbTarget()
            .clickable(role = Role.Button, onClick = onClick)
            .padding(horizontal = Space.margin, vertical = Space.m),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(Space.m),
    ) {
        Icon(
            imageVector = icon,
            contentDescription = null,
            tint = tint,
            modifier = Modifier.size(20.dp),
        )
        Text(
            text = label,
            style = AmuxTheme.typography.body,
            color = tint,
        )
    }
}
