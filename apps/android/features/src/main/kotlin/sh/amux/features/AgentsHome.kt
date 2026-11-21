package sh.amux.features

import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.ArrowBack
import androidx.compose.material.icons.automirrored.outlined.Undo
import androidx.compose.material.icons.outlined.Add
import androidx.compose.material.icons.outlined.Archive
import androidx.compose.material.icons.outlined.Check
import androidx.compose.material.icons.outlined.CheckCircle
import androidx.compose.material.icons.outlined.Close
import androidx.compose.material.icons.outlined.DeleteOutline
import androidx.compose.material.icons.outlined.Edit
import androidx.compose.material.icons.outlined.KeyboardArrowDown
import androidx.compose.material.icons.outlined.KeyboardArrowUp
import androidx.compose.material.icons.outlined.MoreVert
import androidx.compose.material.icons.outlined.NotificationsNone
import androidx.compose.material.icons.outlined.PersonAdd
import androidx.compose.material.icons.outlined.PersonOff
import androidx.compose.material.icons.outlined.PushPin
import androidx.compose.material.icons.outlined.Search
import androidx.compose.material.icons.outlined.Settings
import androidx.compose.material.icons.outlined.Unarchive
import androidx.compose.material.icons.outlined.WifiOff
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.ExtendedFloatingActionButton
import androidx.compose.material3.FilterChip
import androidx.compose.material3.FilterChipDefaults
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
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.derivedStateOf
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import java.time.Instant
import kotlinx.coroutines.launch
import sh.amux.design.AmuxGround
import sh.amux.design.AmuxHairline
import sh.amux.design.AmuxTheme
import sh.amux.design.LocalAmuxPhotographed
import sh.amux.design.Space
import sh.amux.design.thumbTarget
import sh.amux.push.Said
import sh.amux.push.Wants
import sh.amux.values.ActivityKind
import sh.amux.values.AgentKey
import sh.amux.values.DeleteWarning
import sh.amux.values.FleetFilter
import sh.amux.values.FleetOnly
import sh.amux.values.FleetRow
import sh.amux.values.FleetSection
import sh.amux.values.HostView
import sh.amux.values.RelayLink
import sh.amux.values.SecondLine
import sh.amux.values.SectionKind
import sh.amux.words.ChatWords
import sh.amux.words.DeleteWords
import sh.amux.words.FleetWords
import sh.amux.words.PlaceNames
import sh.amux.words.RowState
import sh.amux.words.age
import sh.amux.words.children
import sh.amux.words.closed
import sh.amux.words.deletable
import sh.amux.words.hostName
import sh.amux.words.id
import sh.amux.words.name
import sh.amux.words.pinnable
import sh.amux.words.pinned
import sh.amux.words.readable
import sh.amux.words.speaksForAMember

internal data class HomeCallout(
    val id: String,
    val title: String,
    val detail: String?,
    val icon: ImageVector,
    val working: Boolean = false,
    val buttonLabel: String? = null,
    val cardTestTag: String? = null,
    val actionTestTag: String? = null,
    val action: HomeAction? = null,
    val dismiss: HomeAction? = null,
)

private sealed interface RowConfirmation {
    data class Settle(val row: FleetRow, val cuts: String) : RowConfirmation
    data class Delete(val row: FleetRow, val warning: DeleteWarning?) : RowConfirmation
}

/**
 * Primary Agents screen (and Settled archive screen when [AgentsHomeState.scope] is [HomeScope.Closed]).
 *
 * Pure Compose function of [state] and [onAction] callbacks with zero navigation,
 * activity, or network dependencies.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun AgentsHome(
    state: AgentsHomeState,
    onAction: (HomeAction) -> Unit,
    modifier: Modifier = Modifier,
    searchExpanded: Boolean = false,
    onSearchExpandedChange: (Boolean) -> Unit = {},
    onPreviewDelete: (suspend (AgentKey) -> Result<DeleteWarning?>)? = null,
) {
    val colors = AmuxTheme.colors
    val listState = rememberLazyListState()
    val coroutineScope = rememberCoroutineScope()
    val fabExpanded by remember {
        derivedStateOf {
            listState.firstVisibleItemIndex == 0 && listState.firstVisibleItemScrollOffset < 24
        }
    }

    var actionRow by remember { mutableStateOf<FleetRow?>(null) }
    var renamingRow by remember { mutableStateOf<FleetRow?>(null) }
    var renameDraft by remember { mutableStateOf("") }
    var confirming by remember { mutableStateOf<RowConfirmation?>(null) }

    val showSearchControl = state.scope == HomeScope.Fleet &&
        (state.rows.isNotEmpty() || state.filtering)
    val searchVisible = showSearchControl && (searchExpanded || state.filtering)

    val subtitleText = when {
        state.scope != HomeScope.Fleet -> null
        state.rows.isNotEmpty() || state.filtering -> {
            if (state.needYou == 0) {
                stringResource(R.string.home_subtitle_nothing)
            } else {
                stringResource(R.string.home_subtitle_needs_you, state.needYou)
            }
        }
        else -> null
    }

    val callouts = rememberHomeCallouts(state = state)

    AmuxGround(
        modifier = modifier.testTag(
            if (state.scope == HomeScope.Fleet) TestTags.HOME_SCREEN else TestTags.SCREEN_SETTLED,
        ),
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
                            actionIconContentColor = colors.ink,
                        ),
                        navigationIcon = {
                            if (state.scope == HomeScope.Closed) {
                                val backLabel = stringResource(R.string.action_back)
                                IconButton(
                                    onClick = { onAction(HomeAction.Back) },
                                    modifier = Modifier
                                        .thumbTarget()
                                        .testTag(TestTags.NAV_BACK),
                                ) {
                                    Icon(
                                        imageVector = Icons.AutoMirrored.Outlined.ArrowBack,
                                        contentDescription = backLabel,
                                    )
                                }
                            }
                        },
                        title = {
                            Column {
                                Text(
                                    text = stringResource(
                                        if (state.scope == HomeScope.Fleet) {
                                            R.string.home_title
                                        } else {
                                            R.string.home_settled_title
                                        },
                                    ),
                                    style = AmuxTheme.typography.screenTitle,
                                    color = colors.ink,
                                    maxLines = 1,
                                    overflow = TextOverflow.Ellipsis,
                                )
                                if (subtitleText != null) {
                                    Text(
                                        text = subtitleText,
                                        style = AmuxTheme.typography.caption,
                                        color = if (state.needYou > 0) colors.accent else colors.inkMuted,
                                        maxLines = 1,
                                        overflow = TextOverflow.Ellipsis,
                                        modifier = Modifier.testTag(TestTags.HOME_SUBTITLE),
                                    )
                                }
                            }
                        },
                        actions = {
                            if (showSearchControl) {
                                val searchDesc = stringResource(R.string.home_search_button)
                                IconButton(
                                    onClick = {
                                        if (searchVisible) {
                                            if (state.filtering) {
                                                onAction(HomeAction.Filter(FleetFilter(words = "", only = null)))
                                            }
                                            onSearchExpandedChange(false)
                                        } else {
                                            onSearchExpandedChange(true)
                                        }
                                    },
                                    modifier = Modifier
                                        .thumbTarget()
                                        .testTag(TestTags.HOME_SEARCH_TOGGLE),
                                ) {
                                    Icon(
                                        imageVector = if (searchVisible) Icons.Outlined.Close else Icons.Outlined.Search,
                                        contentDescription = searchDesc,
                                        tint = colors.inkMuted,
                                    )
                                }
                            }
                            if (state.scope == HomeScope.Fleet) {
                                AmuxAccountAvatar(
                                    account = state.selectedAccount,
                                    onClick = { onAction(HomeAction.OpenAccountMenu) },
                                    modifier = Modifier
                                        .padding(end = Space.xs)
                                        .testTag(TestTags.HOME_ACCOUNT_MENU),
                                )
                            }
                        },
                    )
                    if (searchVisible) {
                        FleetSearchBar(
                            filter = state.filter,
                            needYouCount = state.needYou,
                            machines = state.machines,
                            onFilterChange = { onAction(HomeAction.Filter(it)) },
                        )
                    }
                    AmuxHairline(color = colors.hairline.copy(alpha = 0.6f))
                }
            },
            floatingActionButton = {
                if (state.offersNewAgent && (state.rows.isNotEmpty() || state.filtering)) {
                    val newAgentLabel = stringResource(R.string.home_new_agent)
                    ExtendedFloatingActionButton(
                        onClick = { onAction(HomeAction.NewAgent) },
                        expanded = fabExpanded,
                        containerColor = colors.ink,
                        contentColor = colors.ground,
                        icon = {
                            Icon(
                                imageVector = Icons.Outlined.Add,
                                contentDescription = null,
                            )
                        },
                        text = {
                            Text(
                                text = newAgentLabel,
                                style = AmuxTheme.typography.bodyEmphasis,
                            )
                        },
                        modifier = Modifier.testTag(TestTags.HOME_FAB_NEW_AGENT),
                    )
                }
            },
        ) { innerPadding ->
            when {
                state.scope == HomeScope.Closed -> {
                    FleetListContent(
                        state = state,
                        callouts = emptyList(),
                        listState = listState,
                        contentPadding = innerPadding,
                        onOpenRow = { onAction(HomeAction.Open(it.id)) },
                        onOpenRowMenu = { actionRow = it },
                        onToggleFamily = { onAction(HomeAction.ToggleFamily(it.id)) },
                        onAction = onAction,
                    )
                }
                state.rows.isNotEmpty() || state.filtering -> {
                    FleetListContent(
                        state = state,
                        callouts = callouts,
                        listState = listState,
                        contentPadding = innerPadding,
                        onOpenRow = { onAction(HomeAction.Open(it.id)) },
                        onOpenRowMenu = { actionRow = it },
                        onToggleFamily = { onAction(HomeAction.ToggleFamily(it.id)) },
                        onAction = onAction,
                    )
                }
                !state.loaded -> {
                    // Waiting for initial local store snapshot; keep neutral ground without spinner.
                    Box(modifier = Modifier.fillMaxSize())
                }
                state.machines.isNotEmpty() -> {
                    NoAgentsYetContent(
                        state = state,
                        callouts = callouts,
                        onAction = onAction,
                        contentPadding = innerPadding,
                    )
                }
                else -> {
                    FirstRunContent(
                        state = state,
                        onAction = onAction,
                        contentPadding = innerPadding,
                    )
                }
            }
        }
    }

    // Long-press / overflow action sheet for an agent row.
    val currentActionRow = actionRow
    if (currentActionRow != null) {
        AgentRowActionSheet(
            row = currentActionRow,
            onDismiss = { actionRow = null },
            onPin = {
                actionRow = null
                onAction(HomeAction.Pin(currentActionRow.id))
            },
            onUnpin = {
                actionRow = null
                onAction(HomeAction.Unfile(currentActionRow.id))
            },
            onRename = {
                actionRow = null
                renameDraft = currentActionRow.card.handle.ifEmpty { currentActionRow.name }
                renamingRow = currentActionRow
            },
            onSettle = {
                actionRow = null
                val cuts = ChatWords.closeCuts(
                    name = currentActionRow.name,
                    attention = currentActionRow.card.attention,
                    jobs = currentActionRow.card.backgroundJobs,
                    subAgents = currentActionRow.card.descendants,
                )
                if (cuts != null) {
                    confirming = RowConfirmation.Settle(currentActionRow, cuts)
                } else {
                    onAction(HomeAction.Settle(currentActionRow.id))
                }
            },
            onUnsettle = {
                actionRow = null
                onAction(HomeAction.Unfile(currentActionRow.id))
            },
            onCancelSettle = {
                actionRow = null
                onAction(HomeAction.CancelSettle(currentActionRow.id))
            },
            onDelete = {
                actionRow = null
                val initialWarning = DeleteWords.asking(currentActionRow.card.workspaceGoes)
                confirming = RowConfirmation.Delete(currentActionRow, initialWarning)
                if (initialWarning != null && onPreviewDelete != null) {
                    coroutineScope.launch {
                        val answer = onPreviewDelete(currentActionRow.id)
                        val active = confirming
                        if (active is RowConfirmation.Delete && active.row.id == currentActionRow.id) {
                            confirming = active.copy(
                                warning = DeleteWords.answered(active.warning, answer),
                            )
                        }
                    }
                }
            },
        )
    }

    // Rename AlertDialog
    val currentRenameRow = renamingRow
    if (currentRenameRow != null) {
        AlertDialog(
            onDismissRequest = { renamingRow = null },
            containerColor = colors.raised,
            titleContentColor = colors.ink,
            textContentColor = colors.ink,
            modifier = Modifier.testTag(TestTags.HOME_RENAME_DIALOG),
            title = {
                Text(
                    text = stringResource(R.string.home_rename_title),
                    style = AmuxTheme.typography.identifier,
                )
            },
            text = {
                val inputShape = RoundedCornerShape(AmuxTheme.metrics.controlRadius)
                val photographed = LocalAmuxPhotographed.current
                Box(
                    modifier = Modifier
                        .width(260.dp)
                        .clip(inputShape)
                        .background(colors.ground)
                        .border(AmuxTheme.metrics.hairline, colors.hairline, inputShape)
                        .padding(horizontal = Space.m, vertical = Space.m),
                ) {
                    if (renameDraft.isEmpty()) {
                        Text(
                            text = stringResource(R.string.home_rename_placeholder),
                            style = AmuxTheme.typography.body,
                            color = colors.inkFaint,
                        )
                    }
                    BasicTextField(
                        value = renameDraft,
                        onValueChange = { renameDraft = it },
                        singleLine = true,
                        textStyle = AmuxTheme.typography.body.copy(color = colors.ink),
                        cursorBrush = SolidColor(if (photographed) Color.Unspecified else colors.ink),
                        modifier = Modifier
                            .width(236.dp)
                            .testTag(TestTags.HOME_RENAME_INPUT),
                    )
                }
            },
            confirmButton = {
                TextButton(
                    onClick = {
                        val trimmed = renameDraft.trim()
                        if (trimmed.isNotEmpty()) {
                            onAction(HomeAction.Rename(currentRenameRow.id, trimmed))
                        }
                        renamingRow = null
                    },
                    enabled = renameDraft.trim().isNotEmpty(),
                    modifier = Modifier.testTag(TestTags.HOME_RENAME_CONFIRM),
                ) {
                    Text(
                        text = stringResource(R.string.home_action_rename),
                        style = AmuxTheme.typography.bodyEmphasis,
                        color = colors.ink,
                    )
                }
            },
            dismissButton = {
                TextButton(onClick = { renamingRow = null }) {
                    Text(
                        text = stringResource(R.string.action_cancel),
                        style = AmuxTheme.typography.body,
                        color = colors.inkMuted,
                    )
                }
            },
        )
    }

    // Settle / Delete confirmation AlertDialog
    when (val dialog = confirming) {
        is RowConfirmation.Settle -> {
            AlertDialog(
                onDismissRequest = { confirming = null },
                containerColor = colors.raised,
                titleContentColor = colors.ink,
                textContentColor = colors.inkMuted,
                title = {
                    Text(
                        text = stringResource(R.string.home_settle_title, dialog.row.name),
                        style = AmuxTheme.typography.identifier,
                    )
                },
                text = {
                    Text(
                        text = stringResource(R.string.home_settle_confirm_detail, dialog.cuts),
                        style = AmuxTheme.typography.detail,
                        color = colors.inkMuted,
                    )
                },
                confirmButton = {
                    TextButton(
                        onClick = {
                            confirming = null
                            onAction(HomeAction.Settle(dialog.row.id))
                        },
                        modifier = Modifier.testTag(TestTags.HOME_SETTLE_CONFIRM),
                    ) {
                        Text(
                            text = stringResource(R.string.home_action_settle),
                            style = AmuxTheme.typography.bodyEmphasis,
                            color = colors.removed,
                        )
                    }
                },
                dismissButton = {
                    TextButton(onClick = { confirming = null }) {
                        Text(
                            text = stringResource(R.string.action_cancel),
                            style = AmuxTheme.typography.body,
                            color = colors.inkMuted,
                        )
                    }
                },
            )
        }
        is RowConfirmation.Delete -> {
            val message = DeleteWords.message(
                descendants = dialog.row.card.descendants,
                warning = dialog.warning,
            )
            AlertDialog(
                onDismissRequest = { confirming = null },
                containerColor = colors.raised,
                titleContentColor = colors.ink,
                textContentColor = colors.inkMuted,
                title = {
                    Text(
                        text = stringResource(R.string.home_delete_title, dialog.row.name),
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
                            confirming = null
                            onAction(
                                HomeAction.Delete(
                                    agent = dialog.row.id,
                                    discardingChanges = DeleteWords.discards(dialog.warning),
                                ),
                            )
                        },
                        enabled = !DeleteWords.awaitsAnswer(dialog.warning),
                        modifier = Modifier.testTag(TestTags.HOME_DELETE_CONFIRM),
                    ) {
                        Text(
                            text = stringResource(R.string.home_action_delete),
                            style = AmuxTheme.typography.bodyEmphasis,
                            color = colors.removed,
                        )
                    }
                },
                dismissButton = {
                    TextButton(onClick = { confirming = null }) {
                        Text(
                            text = stringResource(R.string.action_cancel),
                            style = AmuxTheme.typography.body,
                            color = colors.inkMuted,
                        )
                    }
                },
            )
        }
        null -> Unit
    }
}

@Composable
private fun FleetSearchBar(
    filter: FleetFilter,
    needYouCount: Int,
    machines: List<HostView>,
    onFilterChange: (FleetFilter) -> Unit,
) {
    val colors = AmuxTheme.colors
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .padding(bottom = Space.s),
        verticalArrangement = Arrangement.spacedBy(Space.s),
    ) {
        OutlinedTextField(
            value = filter.words,
            onValueChange = { onFilterChange(filter.copy(words = it)) },
            singleLine = true,
            placeholder = {
                Text(
                    text = stringResource(R.string.home_search_placeholder),
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
                if (filter.words.isNotEmpty()) {
                    val clearLabel = stringResource(R.string.home_search_clear)
                    IconButton(
                        onClick = { onFilterChange(filter.copy(words = "")) },
                        modifier = Modifier
                            .thumbTarget()
                            .testTag(TestTags.HOME_SEARCH_CLEAR),
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
            shape = RoundedCornerShape(AmuxTheme.metrics.controlRadius),
            colors = OutlinedTextFieldDefaults.colors(
                focusedContainerColor = colors.raised,
                unfocusedContainerColor = colors.raised,
                focusedBorderColor = colors.ink,
                unfocusedBorderColor = colors.hairline,
                focusedTextColor = colors.ink,
                unfocusedTextColor = colors.ink,
            ),
            textStyle = AmuxTheme.typography.body,
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = Space.margin)
                .testTag(TestTags.HOME_SEARCH),
        )

        val chipColors = FilterChipDefaults.filterChipColors(
            containerColor = colors.raised,
            labelColor = colors.inkMuted,
            selectedContainerColor = colors.ink,
            selectedLabelColor = colors.ground,
        )
        val chipBorder = FilterChipDefaults.filterChipBorder(
            enabled = true,
            selected = false,
            borderColor = colors.hairline,
            selectedBorderColor = colors.ink,
        )

        Row(
            modifier = Modifier
                .fillMaxWidth()
                .horizontalScroll(rememberScrollState())
                .padding(horizontal = Space.margin),
            horizontalArrangement = Arrangement.spacedBy(Space.s),
        ) {
            FilterChip(
                selected = filter.only == null,
                onClick = { onFilterChange(filter.copy(only = null)) },
                label = {
                    Text(
                        text = stringResource(R.string.home_filter_all),
                        style = AmuxTheme.typography.detail,
                    )
                },
                colors = chipColors,
                border = chipBorder,
                modifier = Modifier.testTag(TestTags.homeFilter("all")),
            )
            FilterChip(
                selected = filter.only == FleetOnly.NeedsYou,
                onClick = {
                    val next = if (filter.only == FleetOnly.NeedsYou) null else FleetOnly.NeedsYou
                    onFilterChange(filter.copy(only = next))
                },
                label = {
                    Text(
                        text = if (needYouCount > 0) {
                            stringResource(R.string.home_filter_needs_you_count, needYouCount)
                        } else {
                            stringResource(R.string.home_filter_needs_you)
                        },
                        style = AmuxTheme.typography.detail,
                    )
                },
                colors = chipColors,
                border = chipBorder,
                modifier = Modifier.testTag(TestTags.homeFilter("needs-you")),
            )
            FilterChip(
                selected = filter.only == FleetOnly.Working,
                onClick = {
                    val next = if (filter.only == FleetOnly.Working) null else FleetOnly.Working
                    onFilterChange(filter.copy(only = next))
                },
                label = {
                    Text(
                        text = stringResource(R.string.home_filter_working),
                        style = AmuxTheme.typography.detail,
                    )
                },
                colors = chipColors,
                border = chipBorder,
                modifier = Modifier.testTag(TestTags.homeFilter("working")),
            )
            if (machines.size > 1) {
                for (host in machines.sortedBy { it.name.lowercase() }) {
                    val selected = (filter.only as? FleetOnly.Host)?.host == host.hostId
                    FilterChip(
                        selected = selected,
                        onClick = {
                            val next = if (selected) null else FleetOnly.Host(host.hostId)
                            onFilterChange(filter.copy(only = next))
                        },
                        label = {
                            Text(
                                text = PlaceNames.host(host.name),
                                style = AmuxTheme.typography.detail,
                            )
                        },
                        colors = chipColors,
                        border = chipBorder,
                        modifier = Modifier.testTag(TestTags.homeFilter("host.${host.name}")),
                    )
                }
            }
        }
    }
}

@Composable
private fun FleetListContent(
    state: AgentsHomeState,
    callouts: List<HomeCallout>,
    listState: androidx.compose.foundation.lazy.LazyListState,
    contentPadding: PaddingValues,
    onOpenRow: (FleetRow) -> Unit,
    onOpenRowMenu: (FleetRow) -> Unit,
    onToggleFamily: (FleetRow) -> Unit,
    onAction: (HomeAction) -> Unit,
) {
    val colors = AmuxTheme.colors
    val namesHosts = remember(state.rows) {
        state.rows.map { it.hostName }.toSet().size > 1
    }
    val nowInstant = remember(state.orderedAtMs) {
        Instant.ofEpochMilli(state.orderedAtMs)
    }

    LazyColumn(
        state = listState,
        modifier = Modifier
            .fillMaxSize()
            .testTag(TestTags.HOME_LIST),
        contentPadding = PaddingValues(
            top = contentPadding.calculateTopPadding() + Space.xs,
            bottom = contentPadding.calculateBottomPadding() + 96.dp,
        ),
    ) {
        if (state.scope == HomeScope.Fleet && callouts.isNotEmpty()) {
            item(key = "fleet-callouts") {
                Column(
                    modifier = Modifier.padding(horizontal = Space.margin, vertical = Space.s),
                    verticalArrangement = Arrangement.spacedBy(Space.s),
                ) {
                    for (callout in callouts) {
                        HomeCalloutView(callout = callout, onAction = onAction)
                    }
                }
            }
        }

        if (state.filtering && state.rows.isEmpty()) {
            item(key = "fleet-no-matches") {
                Column(
                    modifier = Modifier
                        .fillMaxWidth()
                        .padding(horizontal = Space.margin, vertical = Space.xl),
                    verticalArrangement = Arrangement.spacedBy(Space.l),
                ) {
                    Column(verticalArrangement = Arrangement.spacedBy(Space.s)) {
                        Text(
                            text = stringResource(R.string.home_no_matches_title),
                            style = AmuxTheme.typography.screenTitle,
                            color = colors.ink,
                            modifier = Modifier.testTag(TestTags.HOME_NO_MATCHES),
                        )
                        Text(
                            text = stringResource(R.string.home_no_matches_detail),
                            style = AmuxTheme.typography.detail,
                            color = colors.inkMuted,
                        )
                    }
                    AmuxActionButton(
                        label = stringResource(R.string.action_clear),
                        kind = ActionButtonKind.Outline,
                        onClick = {
                            onAction(HomeAction.Filter(FleetFilter(words = "", only = null)))
                        },
                        modifier = Modifier.testTag(TestTags.HOME_NO_MATCHES_CLEAR),
                    )
                }
            }
        }

        if (state.scope == HomeScope.Closed) {
            val closedRows = state.closedSection?.rows.orEmpty()
            if (closedRows.isEmpty()) {
                item(key = "settled-empty") {
                    Column(
                        modifier = Modifier
                            .fillMaxWidth()
                            .padding(horizontal = Space.margin, vertical = Space.xl)
                            .testTag(TestTags.HOME_SETTLED_EMPTY),
                        verticalArrangement = Arrangement.spacedBy(Space.s),
                    ) {
                        Text(
                            text = stringResource(R.string.home_settled_empty_title),
                            style = AmuxTheme.typography.screenTitle,
                            color = colors.ink,
                        )
                        Text(
                            text = stringResource(R.string.home_settled_empty_detail),
                            style = AmuxTheme.typography.detail,
                            color = colors.inkMuted,
                        )
                    }
                }
            } else {
                itemsIndexed(
                    items = closedRows,
                    key = { _, row -> "closed-${row.card.agent.host}-${row.card.agent.agent}" },
                ) { index, row ->
                    AgentListRow(
                        row = row,
                        namesHosts = namesHosts,
                        now = nowInstant,
                        showDivider = index < closedRows.lastIndex,
                        onOpen = { onOpenRow(row) },
                        onOpenMenu = { onOpenRowMenu(row) },
                        onToggleFamily = { onToggleFamily(row) },
                    )
                }
            }
        } else {
            val visibleSections = state.liveSections
            for (section in visibleSections) {
                val showHeader = section.kind == SectionKind.NeedsYou ||
                    section.kind == SectionKind.Pinned ||
                    (visibleSections.size > 1 && section.kind == SectionKind.Live)
                if (showHeader) {
                    item(key = "section-header-${section.kind.name}") {
                        AmuxSectionHeader(
                            title = sectionTitle(section.kind),
                            modifier = Modifier.padding(top = Space.s),
                        )
                    }
                }
                itemsIndexed(
                    items = section.rows,
                    key = { _, row -> "${section.kind.name}-${row.card.agent.host}-${row.card.agent.agent}" },
                ) { index, row ->
                    AgentListRow(
                        row = row,
                        namesHosts = namesHosts,
                        now = nowInstant,
                        showDivider = index < section.rows.lastIndex,
                        onOpen = { onOpenRow(row) },
                        onOpenMenu = { onOpenRowMenu(row) },
                        onToggleFamily = { onToggleFamily(row) },
                    )
                }
            }

            val closedSection = state.closedSection
            if (closedSection != null && closedSection.families > 0) {
                if (state.filtering) {
                    item(key = "section-header-closed-search") {
                        AmuxSectionHeader(
                            title = stringResource(R.string.home_section_settled),
                            modifier = Modifier.padding(top = Space.m),
                        )
                    }
                    itemsIndexed(
                        items = closedSection.rows,
                        key = { _, row -> "search-closed-${row.card.agent.host}-${row.card.agent.agent}" },
                    ) { index, row ->
                        AgentListRow(
                            row = row,
                            namesHosts = namesHosts,
                            now = nowInstant,
                            showDivider = index < closedSection.rows.lastIndex,
                            onOpen = { onOpenRow(row) },
                            onOpenMenu = { onOpenRowMenu(row) },
                            onToggleFamily = { onToggleFamily(row) },
                        )
                    }
                } else {
                    item(key = "settled-footer-row") {
                        SettledFooterRow(
                            count = closedSection.families,
                            onClick = { onAction(HomeAction.ShowSettled) },
                        )
                    }
                }
            }
        }
    }
}

@Composable
private fun sectionTitle(kind: SectionKind): String = when (kind) {
    SectionKind.Pinned -> stringResource(R.string.home_section_pinned)
    SectionKind.NeedsYou -> stringResource(R.string.home_section_needs_you)
    SectionKind.Live -> stringResource(R.string.home_section_live)
    SectionKind.Closed -> stringResource(R.string.home_section_settled)
}

@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun AgentListRow(
    row: FleetRow,
    namesHosts: Boolean,
    now: Instant,
    showDivider: Boolean,
    onOpen: () -> Unit,
    onOpenMenu: () -> Unit,
    onToggleFamily: () -> Unit,
) {
    val colors = AmuxTheme.colors
    val typography = AmuxTheme.typography
    val rowState = remember(row) { RowState.of(row) }
    val fontScale = LocalDensity.current.fontScale
    val isLargeText = fontScale >= 1.3f
    val indent = (row.depth * 24).dp
    val hasFamilyToggle = row.depth == 0 && row.children > 0

    val secondLine: FleetWords.Line? = if (row.card.closeHeld) {
        FleetWords.Line(
            text = stringResource(R.string.home_row_settle_held, row.hostName),
            ink = FleetWords.Ink.Warning,
        )
    } else {
        FleetWords.secondLine(
            line = row.secondLine,
            kind = row.card.kind,
            now = now,
            backgroundJobs = row.card.backgroundJobs,
        ) ?: fallbackPreview(rowState)
    }

    val hostLabel = if (namesHosts) PlaceNames.host(row.hostName) else null
    val facts = listOfNotNull(
        row.card.repository?.name?.takeIf { it.isNotEmpty() },
        hostLabel,
        row.age(at = now),
    ).joinToString(" · ")

    val stopped = rowState is RowState.Exited || rowState is RowState.Settled
    val previewAnnotated = remember(row, secondLine, typography) {
        secondLine?.let { styledSecondLine(row = row, text = it.text, monoSpan = typography.mono.toSpanStyle()) }
    }

    val rowClickableModifier = if (row.readable) {
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
            .testTag(TestTags.homeRow(row.name)),
    ) {
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .then(rowClickableModifier)
                .padding(
                    start = Space.margin + indent,
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
                        color = if (stopped) colors.inkMuted else colors.ink,
                        maxLines = if (isLargeText) 2 else 1,
                        overflow = TextOverflow.Ellipsis,
                        modifier = Modifier.weight(1f, fill = true),
                    )
                    if (!isLargeText) {
                        Text(
                            text = facts,
                            style = typography.caption,
                            color = colors.inkFaint,
                            maxLines = 1,
                        )
                    }
                    if (row.pinned) {
                        val pinnedDesc = stringResource(R.string.home_row_pinned)
                        Icon(
                            imageVector = Icons.Outlined.PushPin,
                            contentDescription = pinnedDesc,
                            tint = colors.inkFaint,
                            modifier = Modifier.size(14.dp),
                        )
                    }
                }

                if (isLargeText && facts.isNotEmpty()) {
                    Text(
                        text = facts,
                        style = typography.caption,
                        color = colors.inkFaint,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                }

                if (previewAnnotated != null && secondLine != null) {
                    val secondColor = when (secondLine.ink) {
                        FleetWords.Ink.Ask -> colors.ink
                        FleetWords.Ink.Quiet -> colors.inkMuted
                        FleetWords.Ink.Warning -> colors.accent
                        FleetWords.Ink.Error -> colors.removed
                    }
                    Text(
                        text = previewAnnotated,
                        style = typography.detail,
                        color = secondColor,
                        maxLines = if (rowState.needsYou || isLargeText) 2 else 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
            }

            if (hasFamilyToggle) {
                val toggleDesc = if (row.expanded) {
                    stringResource(R.string.home_family_hide, row.children, row.name)
                } else {
                    stringResource(R.string.home_family_show, row.children, row.name)
                }
                Row(
                    modifier = Modifier
                        .thumbTarget()
                        .clickable(role = Role.Button, onClick = onToggleFamily)
                        .padding(horizontal = Space.s)
                        .semantics { contentDescription = toggleDesc }
                        .testTag(TestTags.homeFamily(row.name)),
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(Space.xs),
                ) {
                    if (row.speaksForAMember) {
                        AmuxAttentionDot(size = 7.dp)
                    }
                    Text(
                        text = row.children.toString(),
                        style = typography.caption,
                        color = colors.inkMuted,
                    )
                    Icon(
                        imageVector = if (row.expanded) {
                            Icons.Outlined.KeyboardArrowUp
                        } else {
                            Icons.Outlined.KeyboardArrowDown
                        },
                        contentDescription = null,
                        tint = colors.inkFaint,
                        modifier = Modifier.size(16.dp),
                    )
                }
            }

            val moreDesc = stringResource(R.string.action_more_options)
            IconButton(
                onClick = onOpenMenu,
                modifier = Modifier
                    .thumbTarget()
                    .testTag(TestTags.homeRowMenu(row.name)),
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
            AmuxInsetHairline(leadingInset = Space.margin + 20.dp + indent)
        }
    }
}

private fun fallbackPreview(state: RowState): FleetWords.Line? {
    if (state.namesTheHost) return null
    return when (state) {
        is RowState.Exited, RowState.Unsupported, RowState.Starting -> {
            val words = listOfNotNull(state.word, state.elaboration)
            if (words.isEmpty()) null else FleetWords.Line(words.joinToString(" · "), FleetWords.Ink.Quiet)
        }
        else -> null
    }
}

private fun styledSecondLine(
    row: FleetRow,
    text: String,
    monoSpan: SpanStyle,
): AnnotatedString {
    val codeFragment: String? = when (val second = row.secondLine) {
        is SecondLine.Said -> {
            val ask = second.value as? Said.Ask
            when (val want = ask?.wants) {
                is Wants.Run -> ChatWords.firstLine(want.command)
                is Wants.Change -> if (want.files <= 1) want.path else null
                else -> null
            }
        }
        is SecondLine.Step -> {
            val step = second.value.step
            if (second.value.activity.kind is ActivityKind.Running && !step.isNullOrEmpty()) {
                ChatWords.firstLine(step)
            } else {
                null
            }
        }
        else -> null
    }
    if (codeFragment.isNullOrEmpty()) return AnnotatedString(text)
    val start = text.indexOf(codeFragment)
    if (start < 0) return AnnotatedString(text)
    return buildAnnotatedString {
        append(text)
        addStyle(monoSpan, start = start, end = start + codeFragment.length)
    }
}

@Composable
private fun SettledFooterRow(
    count: Int,
    onClick: () -> Unit,
) {
    val colors = AmuxTheme.colors
    Column(modifier = Modifier.fillMaxWidth()) {
        AmuxHairline(
            modifier = Modifier.padding(top = Space.s),
            color = colors.hairline.copy(alpha = 0.65f),
        )
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .thumbTarget(minHeight = 56.dp)
                .clickable(role = Role.Button, onClick = onClick)
                .padding(horizontal = Space.margin, vertical = Space.m)
                .testTag(TestTags.HOME_SETTLED_ROW),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(Space.m),
        ) {
            Icon(
                imageVector = Icons.Outlined.Archive,
                contentDescription = null,
                tint = colors.inkMuted,
                modifier = Modifier.size(18.dp),
            )
            Text(
                text = stringResource(R.string.home_section_settled),
                style = AmuxTheme.typography.body,
                color = colors.inkMuted,
                modifier = Modifier.weight(1f),
            )
            Text(
                text = count.toString(),
                style = AmuxTheme.typography.caption,
                color = colors.inkMuted,
            )
        }
    }
}

@Composable
private fun rememberHomeCallouts(state: AgentsHomeState): List<HomeCallout> {
    val items = mutableListOf<HomeCallout>()
    for (host in state.discoveredHosts) {
        val offer = rememberPairOffer(host = host, accountPro = state.accountPro)
        items += HomeCallout(
            id = "pair.${host.name}",
            title = offer.title,
            detail = offer.detail,
            icon = Icons.Outlined.Add,
            buttonLabel = offer.buttonLabel,
            cardTestTag = TestTags.homeFound(host.name),
            actionTestTag = if (offer.subscribe) TestTags.homeSubscribe(host.name) else TestTags.homePair(host.name),
            action = if (offer.waiting) null else {
                if (offer.subscribe) HomeAction.Subscribe else HomeAction.Pair(host.hostId)
            },
        )
    }
    if (state.signedIn && state.relayTrouble != null) {
        val act = if (state.relay == RelayLink.SignInAgain) HomeAction.SignIn else HomeAction.OpenHosts
        items += HomeCallout(
            id = "relay-trouble",
            title = state.relayTrouble,
            detail = null,
            icon = if (act == HomeAction.SignIn) Icons.Outlined.PersonOff else Icons.Outlined.WifiOff,
            working = act == HomeAction.SignIn && state.signingIn,
            action = act,
        )
    } else if (state.signedIn && state.accountPro != true && state.heldAwayHost != null) {
        items += HomeCallout(
            id = "subscribe-away",
            title = stringResource(R.string.callout_away_title, state.heldAwayHost),
            detail = stringResource(R.string.callout_away_detail),
            icon = Icons.Outlined.PersonOff,
            buttonLabel = stringResource(R.string.callout_subscribe),
            cardTestTag = TestTags.HOME_CALLOUT_SUBSCRIBE_CARD,
            actionTestTag = TestTags.HOME_CALLOUT_SUBSCRIBE,
            action = HomeAction.Subscribe,
        )
    } else if (!state.signedIn && state.unreachableHost != null) {
        items += HomeCallout(
            id = "sign-in-unreachable",
            title = stringResource(R.string.callout_unreachable_title, state.unreachableHost),
            detail = stringResource(R.string.callout_unreachable_detail),
            icon = Icons.Outlined.PersonOff,
            working = state.signingIn,
            buttonLabel = if (state.signingIn) null else stringResource(R.string.callout_sign_in),
            cardTestTag = TestTags.HOME_CALLOUT_SIGN_IN_CARD,
            actionTestTag = TestTags.HOME_CALLOUT_SIGN_IN,
            action = HomeAction.SignIn,
        )
    }
    if (state.offerNotifications) {
        items += HomeCallout(
            id = "notifications-offer",
            title = stringResource(R.string.callout_notifications_title),
            detail = stringResource(R.string.callout_notifications_detail),
            icon = Icons.Outlined.NotificationsNone,
            buttonLabel = stringResource(R.string.callout_notifications_turn_on),
            cardTestTag = TestTags.HOME_CALLOUT_NOTIFICATIONS_CARD,
            actionTestTag = TestTags.HOME_CALLOUT_NOTIFICATIONS,
            action = HomeAction.TurnOnNotifications,
            dismiss = HomeAction.DismissNotifications,
        )
    }
    return items
}

@Composable
internal fun HomeCalloutView(
    callout: HomeCallout,
    onAction: (HomeAction) -> Unit,
) {
    AmuxEmptyCard(
        title = callout.title,
        detail = callout.detail,
        icon = callout.icon,
        working = callout.working,
        buttonLabel = callout.buttonLabel,
        buttonTestTag = callout.actionTestTag,
        onButtonClick = callout.action?.let { act -> { onAction(act) } },
        onCardClick = if (callout.buttonLabel == null) callout.action?.let { act -> { onAction(act) } } else null,
        onDismiss = callout.dismiss?.let { act -> { onAction(act) } },
        modifier = if (callout.cardTestTag != null) Modifier.testTag(callout.cardTestTag) else Modifier,
    )
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun AgentRowActionSheet(
    row: FleetRow,
    onDismiss: () -> Unit,
    onPin: () -> Unit,
    onUnpin: () -> Unit,
    onRename: () -> Unit,
    onSettle: () -> Unit,
    onUnsettle: () -> Unit,
    onCancelSettle: () -> Unit,
    onDelete: () -> Unit,
) {
    val colors = AmuxTheme.colors
    val sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true)
    ModalBottomSheet(
        onDismissRequest = onDismiss,
        sheetState = sheetState,
        containerColor = colors.raised,
        contentColor = colors.ink,
    ) {
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .padding(bottom = Space.xl)
                .testTag(TestTags.HOME_ROW_ACTIONS_SHEET),
        ) {
            Text(
                text = row.name,
                style = AmuxTheme.typography.identifier,
                color = colors.ink,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.padding(horizontal = Space.margin, vertical = Space.s),
            )
            AmuxHairline()
            if (row.pinnable) {
                if (row.pinned) {
                    SheetActionItem(
                        label = stringResource(R.string.home_action_unpin),
                        icon = Icons.Outlined.PushPin,
                        testTag = TestTags.HOME_ACTION_UNPIN,
                        onClick = onUnpin,
                    )
                } else {
                    SheetActionItem(
                        label = stringResource(R.string.home_action_pin),
                        icon = Icons.Outlined.PushPin,
                        testTag = TestTags.HOME_ACTION_PIN,
                        onClick = onPin,
                    )
                }
            }
            SheetActionItem(
                label = stringResource(R.string.home_action_rename),
                icon = Icons.Outlined.Edit,
                testTag = TestTags.HOME_ACTION_RENAME,
                onClick = onRename,
            )
            when {
                row.closed -> {
                    SheetActionItem(
                        label = stringResource(R.string.home_action_unsettle),
                        icon = Icons.Outlined.Unarchive,
                        testTag = TestTags.HOME_ACTION_UNSETTLE,
                        onClick = onUnsettle,
                    )
                    if (row.deletable) {
                        SheetActionItem(
                            label = stringResource(R.string.home_action_delete_agent),
                            icon = Icons.Outlined.DeleteOutline,
                            destructive = true,
                            testTag = TestTags.HOME_ACTION_DELETE,
                            onClick = onDelete,
                        )
                    }
                }
                row.card.closeHeld -> {
                    SheetActionItem(
                        label = stringResource(R.string.home_action_cancel_settle),
                        icon = Icons.AutoMirrored.Outlined.Undo,
                        testTag = TestTags.HOME_ACTION_CANCEL_SETTLE,
                        onClick = onCancelSettle,
                    )
                }
                else -> {
                    SheetActionItem(
                        label = stringResource(R.string.home_action_settle),
                        icon = Icons.Outlined.CheckCircle,
                        testTag = TestTags.HOME_ACTION_SETTLE,
                        onClick = onSettle,
                    )
                }
            }
        }
    }
}

@Composable
private fun SheetActionItem(
    label: String,
    icon: ImageVector,
    testTag: String,
    destructive: Boolean = false,
    onClick: () -> Unit,
) {
    val colors = AmuxTheme.colors
    val tint = if (destructive) colors.removed else colors.ink
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .thumbTarget(minHeight = 52.dp)
            .clickable(role = Role.Button, onClick = onClick)
            .padding(horizontal = Space.margin, vertical = Space.m)
            .testTag(testTag),
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

/**
 * Account switcher M3 [ModalBottomSheet] opened from the top-right account avatar.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun AccountSwitcherSheet(
    accounts: List<AccountUi>,
    onSelectAccount: (String) -> Unit,
    onSignInAgain: (String) -> Unit,
    onAddAccount: () -> Unit,
    onOpenSettings: () -> Unit,
    onDismiss: () -> Unit,
) {
    val colors = AmuxTheme.colors
    val sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true)
    ModalBottomSheet(
        onDismissRequest = onDismiss,
        sheetState = sheetState,
        containerColor = colors.raised,
        contentColor = colors.ink,
    ) {
        AccountSwitcherContent(
            accounts = accounts,
            onSelectAccount = onSelectAccount,
            onSignInAgain = onSignInAgain,
            onAddAccount = onAddAccount,
            onOpenSettings = onOpenSettings,
        )
    }
}

/**
 * Inner content of [AccountSwitcherSheet], exposed separately for direct testing
 * and storybook rendering.
 */
@Composable
fun AccountSwitcherContent(
    accounts: List<AccountUi>,
    onSelectAccount: (String) -> Unit,
    onSignInAgain: (String) -> Unit,
    onAddAccount: () -> Unit,
    onOpenSettings: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val colors = AmuxTheme.colors
    Column(
        modifier = modifier
            .fillMaxWidth()
            .padding(bottom = Space.xl)
            .testTag(TestTags.ACCOUNTS_SHEET),
    ) {
        Text(
            text = stringResource(R.string.accounts_sheet_title),
            style = AmuxTheme.typography.identifier,
            color = colors.ink,
            modifier = Modifier.padding(horizontal = Space.margin, vertical = Space.s),
        )
        AmuxHairline()

        for (account in accounts) {
            val detail = if (account.signedIn) {
                account.line
            } else {
                stringResource(R.string.accounts_signed_out_tap)
            }
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .thumbTarget(minHeight = 56.dp)
                    .clickable(role = Role.Button) {
                        if (account.signedIn) {
                            onSelectAccount(account.id)
                        } else {
                            onSignInAgain(account.id)
                        }
                    }
                    .padding(horizontal = Space.margin, vertical = Space.s)
                    .testTag(TestTags.accountRow(account.email)),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(Space.m),
            ) {
                AmuxAccountAvatar(
                    account = account,
                    onClick = null,
                )
                Column(
                    modifier = Modifier.weight(1f),
                    verticalArrangement = Arrangement.spacedBy(Space.xxs),
                ) {
                    Text(
                        text = account.name,
                        style = AmuxTheme.typography.bodyEmphasis,
                        color = if (account.signedIn) colors.ink else colors.inkMuted,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                    Text(
                        text = detail,
                        style = AmuxTheme.typography.detail,
                        color = colors.inkMuted,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
                if (account.selected) {
                    val selectedDesc = stringResource(R.string.accounts_selected)
                    Icon(
                        imageVector = Icons.Outlined.Check,
                        contentDescription = selectedDesc,
                        tint = colors.ink,
                        modifier = Modifier.size(20.dp),
                    )
                }
            }
        }

        if (accounts.isNotEmpty()) {
            AmuxHairline(modifier = Modifier.padding(vertical = Space.xs))
        }

        SheetActionItem(
            label = stringResource(
                if (accounts.isEmpty()) R.string.accounts_sign_in else R.string.accounts_add,
            ),
            icon = Icons.Outlined.PersonAdd,
            testTag = if (accounts.isEmpty()) TestTags.ACCOUNTS_SIGN_IN else TestTags.ACCOUNTS_ADD,
            onClick = onAddAccount,
        )

        AmuxHairline(modifier = Modifier.padding(vertical = Space.xs))

        SheetActionItem(
            label = stringResource(R.string.accounts_settings),
            icon = Icons.Outlined.Settings,
            testTag = TestTags.ACCOUNTS_SETTINGS,
            onClick = onOpenSettings,
        )
    }
}
