package sh.amux.features

import sh.amux.design.ThumbTarget
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.layout.Spacer
import sh.amux.design.IconSize
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.border
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
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
import androidx.compose.foundation.lazy.LazyListState
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.Undo
import androidx.compose.material.icons.automirrored.outlined.ArrowBack
import androidx.compose.material.icons.outlined.Inventory2
import androidx.compose.foundation.layout.offset
import androidx.compose.ui.semantics.CustomAccessibilityAction
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.onClick
import androidx.compose.ui.semantics.role
import androidx.compose.ui.semantics.customActions
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.text.style.TextAlign
import sh.amux.words.workingDirectory
import androidx.compose.material.icons.outlined.Add
import androidx.compose.material.icons.outlined.Archive
import androidx.compose.material.icons.outlined.CheckCircle
import androidx.compose.material.icons.outlined.Close
import androidx.compose.material.icons.outlined.DeleteOutline
import androidx.compose.material.icons.outlined.Edit
import androidx.compose.material.icons.outlined.KeyboardArrowDown
import androidx.compose.material.icons.outlined.KeyboardArrowUp
import androidx.compose.material.icons.outlined.MoreVert
import androidx.compose.material.icons.outlined.NotificationsNone
import androidx.compose.material.icons.outlined.PersonOff
import androidx.compose.material.icons.outlined.PushPin
import androidx.compose.material.icons.outlined.Search
import androidx.compose.material.icons.outlined.Unarchive
import androidx.compose.material.icons.outlined.WifiOff
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.ExtendedFloatingActionButton
import androidx.compose.material3.FilterChip
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.derivedStateOf
import androidx.compose.runtime.getValue
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
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
import sh.amux.design.AmuxGround
import sh.amux.design.AmuxHairline
import sh.amux.design.AmuxTheme
import sh.amux.design.Space
import androidx.compose.ui.draw.clip
import sh.amux.design.spillTarget
import androidx.compose.material.icons.filled.PushPin
import androidx.compose.foundation.layout.heightIn
import sh.amux.push.Said
import sh.amux.push.Wants
import sh.amux.values.ActivityKind
import sh.amux.values.AgentKey
import sh.amux.values.AgentNameProblem
import sh.amux.values.DeleteWarning
import sh.amux.values.FleetFilter
import sh.amux.values.FleetOnly
import sh.amux.values.FleetRow
import sh.amux.values.HostView
import sh.amux.values.RelayLink
import sh.amux.values.SecondLine
import sh.amux.values.SectionKind
import sh.amux.words.ChatWords
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
    data class Delete(val row: FleetRow) : RowConfirmation
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
    validateAgentName: (String) -> AgentNameProblem? = ::defaultValidateAgentName,
    listState: LazyListState = rememberLazyListState(),
) {
    val colors = AmuxTheme.colors
    val fabExpanded by remember {
        derivedStateOf {
            listState.firstVisibleItemIndex == 0 && listState.firstVisibleItemScrollOffset < 24
        }
    }

    var actionRow by remember { mutableStateOf<FleetRow?>(null) }
    var renamingRow by remember { mutableStateOf<FleetRow?>(null) }
    var confirming by remember { mutableStateOf<RowConfirmation?>(null) }

    fun runRowAction(row: FleetRow, action: RowMenuAction) {
        when (action) {
            RowMenuAction.Pin -> onAction(HomeAction.Pin(row.id))
            RowMenuAction.Unpin, RowMenuAction.Unsettle -> onAction(HomeAction.Unfile(row.id))
            RowMenuAction.Rename -> renamingRow = row
            RowMenuAction.Settle -> {
                val cuts = ChatWords.closeCuts(
                    name = row.name,
                    attention = row.card.attention,
                    jobs = row.card.backgroundJobs,
                    subAgents = row.card.descendants,
                )
                if (cuts != null) {
                    confirming = RowConfirmation.Settle(row, cuts)
                } else {
                    onAction(HomeAction.Settle(row.id))
                }
            }
            RowMenuAction.CancelSettle -> onAction(HomeAction.CancelSettle(row.id))
            RowMenuAction.Delete -> confirming = RowConfirmation.Delete(row)
        }
    }

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
                val scrolled by remember { derivedStateOf { listState.canScrollBackward } }
                AmuxTopBar(
                    title = stringResource(
                        if (state.scope == HomeScope.Fleet) R.string.home_title else R.string.home_settled_title,
                    ),
                    large = state.scope == HomeScope.Fleet,
                    subtitle = subtitleText,
                    // A count is never the accent: the rows that need you
                    // carry it, and the line only says how many.
                    subtitleModifier = Modifier.testTag(TestTags.HOME_SUBTITLE),
                    scrolled = scrolled,
                    onNavigate = if (state.scope == HomeScope.Closed) {
                        { onAction(HomeAction.Back) }
                    } else {
                        null
                    },
                    navigationModifier = Modifier.testTag(TestTags.SETTLED_BACK),
                    below = if (searchVisible) {
                        {
                            FleetSearchBar(
                                filter = state.filter,
                                needYouCount = state.needYou,
                                machines = state.machines,
                                onFilterChange = { onAction(HomeAction.Filter(it)) },
                                onClose = {
                                    if (state.filtering) {
                                        onAction(HomeAction.Filter(FleetFilter(words = "", only = null)))
                                    }
                                    onSearchExpandedChange(false)
                                },
                            )
                        }
                    } else {
                        null
                    },
                ) {
                    // While the search is open its field carries the way
                    // out, so the bar offers no second cross beside it.
                    if (showSearchControl && !searchVisible) {
                        IconButton(
                            onClick = { onSearchExpandedChange(true) },
                            modifier = Modifier.testTag(TestTags.HOME_SEARCH_TOGGLE),
                        ) {
                            Icon(
                                imageVector = Icons.Outlined.Search,
                                contentDescription = stringResource(R.string.home_search_button),
                                tint = colors.inkMuted,
                            )
                        }
                    }
                    if (state.scope == HomeScope.Fleet) {
                        AmuxAccountAvatar(
                            account = state.selectedAccount,
                            onClick = { onAction(HomeAction.OpenAccountMenu) },
                            modifier = Modifier.testTag(TestTags.HOME_ACCOUNT_MENU),
                        )
                    }
                }
            },
            floatingActionButton = {
                if (state.offersNewAgent && (state.rows.isNotEmpty() || state.filtering)) {
                    val newAgentLabel = stringResource(R.string.home_new_agent)
                    ExtendedFloatingActionButton(
                        onClick = { onAction(HomeAction.NewAgent) },
                        expanded = fabExpanded,
                        // New Agent is the screen's one way to start
                        // something, so it takes the interactive tint.
                        containerColor = colors.accentFill,
                        contentColor = colors.onAccent,
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
                        // Said once, whether the label is showing or the
                        // button has folded to its plus.
                        // The tag goes first: semantics after the clear
                        // would be cleared with the rest.
                        modifier = Modifier
                            .testTag(TestTags.HOME_FAB_NEW_AGENT)
                            .clearAndSetSemantics {
                                contentDescription = newAgentLabel
                                role = Role.Button
                                onClick {
                                    onAction(HomeAction.NewAgent)
                                    true
                                }
                            },
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
                        onRowMenuAction = ::runRowAction,
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
                        onRowMenuAction = ::runRowAction,
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

    // Long-press action sheet for an agent row; TalkBack offers the same
    // actions on the row itself.
    val currentActionRow = actionRow
    if (currentActionRow != null) {
        AgentRowActionSheet(
            row = currentActionRow,
            onDismiss = { actionRow = null },
            onAction = { action ->
                actionRow = null
                runRowAction(currentActionRow, action)
            },
        )
    }

    val currentRenameRow = renamingRow
    if (currentRenameRow != null) {
        RenameAgentDialog(
            currentName = currentRenameRow.name,
            initialDraft = currentRenameRow.card.handle.ifEmpty { currentRenameRow.name },
            validateName = validateAgentName,
            onDismiss = { renamingRow = null },
            onConfirm = { name ->
                renamingRow = null
                onAction(HomeAction.Rename(currentRenameRow.id, name))
            },
            tags = AgentDialogTags.Home,
        )
    }

    when (val dialog = confirming) {
        is RowConfirmation.Settle -> {
            SettleAgentDialog(
                agentName = dialog.row.name,
                cuts = dialog.cuts,
                onDismiss = { confirming = null },
                onConfirm = {
                    confirming = null
                    onAction(HomeAction.Settle(dialog.row.id))
                },
                tags = AgentDialogTags.Home,
            )
        }
        is RowConfirmation.Delete -> {
            // Keyed by the row, so a second delete asks the host afresh.
            key(dialog.row.id) {
                DeleteAgentDialog(
                    agentName = dialog.row.name,
                    descendants = dialog.row.card.descendants,
                    onPreviewDelete = onPreviewDelete?.let { preview -> { preview(dialog.row.id) } },
                    onDismiss = { confirming = null },
                    onConfirm = { discardingChanges ->
                        confirming = null
                        onAction(HomeAction.Delete(agent = dialog.row.id, discardingChanges = discardingChanges))
                    },
                    tags = AgentDialogTags.Home,
                )
            }
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
    onClose: () -> Unit,
) {
    val colors = AmuxTheme.colors
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .padding(bottom = Space.s),
        verticalArrangement = Arrangement.spacedBy(Space.s),
    ) {
        AmuxField(
            value = filter.words,
            onValueChange = { onFilterChange(filter.copy(words = it)) },
            placeholder = stringResource(R.string.home_search_placeholder),
            // Material's search bar: the leading arrow closes the search and
            // forgets it, and the trailing cross only empties the words.
            leadingIcon = {
                IconButton(
                    onClick = onClose,
                    modifier = Modifier.testTag(TestTags.HOME_SEARCH_CLOSE),
                ) {
                    Icon(
                        imageVector = Icons.AutoMirrored.Outlined.ArrowBack,
                        contentDescription = stringResource(R.string.home_search_close),
                        tint = colors.inkMuted,
                    )
                }
            },
            trailingIcon = if (filter.words.isNotEmpty()) {
                {
                    val clearLabel = stringResource(R.string.home_search_clear)
                    IconButton(
                        onClick = { onFilterChange(filter.copy(words = "")) },
                        modifier = Modifier.testTag(TestTags.HOME_SEARCH_CLEAR),
                    ) {
                        Icon(
                            imageVector = Icons.Outlined.Close,
                            contentDescription = clearLabel,
                            tint = colors.inkMuted,
                            modifier = Modifier.size(IconSize.Control),
                        )
                    }
                }
            } else {
                null
            },
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = Space.margin)
                .testTag(TestTags.HOME_SEARCH),
        )

        val chipColors = amuxFilterChipColors()

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
                        style = AmuxTheme.typography.caption,
                    )
                },
                colors = chipColors,
                border = amuxFilterChipBorder(filter.only == null),
                modifier = Modifier
                    .testTag(TestTags.homeFilter("all")),
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
                        style = AmuxTheme.typography.caption,
                    )
                },
                colors = chipColors,
                border = amuxFilterChipBorder(filter.only == FleetOnly.NeedsYou),
                modifier = Modifier
                    .testTag(TestTags.homeFilter("needs-you")),
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
                        style = AmuxTheme.typography.caption,
                    )
                },
                colors = chipColors,
                border = amuxFilterChipBorder(filter.only == FleetOnly.Working),
                modifier = Modifier
                    .testTag(TestTags.homeFilter("working")),
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
                                style = AmuxTheme.typography.caption,
                            )
                        },
                        colors = chipColors,
                        border = amuxFilterChipBorder(selected),
                        modifier = Modifier
                            .testTag(TestTags.homeFilter("host.${host.name}")),
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
    onRowMenuAction: (FleetRow, RowMenuAction) -> Unit,
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
                        rowTag = TestTags.settledRow(row.name),
                        onOpen = { onOpenRow(row) },
                        onOpenMenu = { onOpenRowMenu(row) },
                        onMenuAction = { action -> onRowMenuAction(row, action) },
                        onToggleFamily = { onToggleFamily(row) },
                    )
                }
            }
        } else {
            // At the head of the list, as a chat app keeps its archived
            // chats: within reach whatever the length of the list under it,
            // and never under the agents that are wanted.
            val closedSection = state.closedSection
            if (!state.filtering && closedSection != null && closedSection.families > 0) {
                item(key = "settled-head-row") {
                    SettledHeadRow(
                        count = closedSection.families,
                        onClick = { onAction(HomeAction.ShowSettled) },
                    )
                }
            }

            // One list, pinned first, then the agents that need you, then the
            // rest: each row already says which it is, so headings would only
            // repeat it and split the list into groups.
            val liveRows = state.liveSections.flatMap { it.rows }
            itemsIndexed(
                items = liveRows,
                key = { _, row -> "live-${row.card.agent.host}-${row.card.agent.agent}" },
            ) { index, row ->
                AgentListRow(
                    row = row,
                    namesHosts = namesHosts,
                    now = nowInstant,
                    showDivider = index < liveRows.lastIndex,
                    onOpen = { onOpenRow(row) },
                    onOpenMenu = { onOpenRowMenu(row) },
                    onMenuAction = { action -> onRowMenuAction(row, action) },
                    onToggleFamily = { onToggleFamily(row) },
                )
            }

            if (state.filtering && closedSection != null && closedSection.families > 0) {
                // Found by the search, under the rest, as a chat app finds
                // archived chats.
                item(key = "section-header-closed-search") {
                    Text(
                        text = stringResource(R.string.home_section_settled),
                        style = AmuxTheme.typography.sectionTitle,
                        color = colors.inkMuted,
                        modifier = Modifier
                            .padding(
                                start = Space.margin + AgentMarkColumn,
                                top = Space.l,
                                bottom = Space.xs,
                            )
                            .semantics { heading() },
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
                        onMenuAction = { action -> onRowMenuAction(row, action) },
                        onToggleFamily = { onToggleFamily(row) },
                    )
                }
            }
        }
    }
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
    onMenuAction: (RowMenuAction) -> Unit,
    onToggleFamily: () -> Unit,
    rowTag: String = TestTags.homeRow(row.name),
) {
    val colors = AmuxTheme.colors
    val typography = AmuxTheme.typography
    val density = LocalDensity.current
    val rowState = remember(row) { RowState.of(row) }
    val isLargeText = density.fontScale >= 1.3f
    val indent = AgentMarkColumn * row.depth
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
    val age = row.age(at = now)
    val facts = listOfNotNull(
        row.card.repository?.name?.takeIf { it.isNotEmpty() },
        hostLabel,
        age,
    ).joinToString(" · ")

    val stopped = rowState is RowState.Exited || rowState is RowState.Settled
    val previewAnnotated = remember(row, secondLine, typography) {
        secondLine?.let { styledSecondLine(row = row, text = it.text, monoSpan = typography.mono.toSpanStyle()) }
    }

    // What the row says to somebody who cannot see it, in the order it says
    // it: who, what, where, how long, and what it needs.
    val spokenPinned = stringResource(R.string.home_row_spoken_pinned)
    val spokenMember = stringResource(R.string.home_row_spoken_member_needs_you)
    val spokenAgo = stringResource(R.string.home_row_spoken_ago, age)
    val spoken = remember(row, rowState, secondLine, spokenAgo) {
        buildList {
            add(row.name)
            if (row.pinned) add(spokenPinned)
            rowState.spoken?.let(::add)
            secondLine?.text?.let(::add)
            if (row.speaksForAMember) add(spokenMember)
            add(listOf(PlaceNames.host(row.hostName), row.workingDirectory).filter { it.isNotEmpty() }.joinToString(", "))
            add(spokenAgo)
        }.joinToString(", ")
    }
    val menuActions = remember(row) { rowMenuActions(row) }
    val menuLabels = menuActions.map { rowMenuLabel(it) }
    val customActions = menuActions.zip(menuLabels) { action, label ->
        CustomAccessibilityAction(label) {
            onMenuAction(action)
            true
        }
    }
    val moreLabel = stringResource(R.string.action_more)

    // An agent of a kind this build has no chat for is listed and not
    // offered to open; it still answers to a long press.
    val rowClickableModifier = Modifier.combinedClickable(
        onClick = if (row.readable) onOpen else ({}),
        onLongClick = onOpenMenu,
        onLongClickLabel = moreLabel,
        role = if (row.readable) Role.Button else null,
    )

    // The mark sits centred on the name's lowercase letters rather than on
    // the row, so a two-line row keeps it beside the name, as iOS does.
    val markOnBaseline = with(density) { MarkAboveBaseline.roundToPx() }

    Column(
        modifier = Modifier
            .fillMaxWidth()
            .testTag(rowTag),
    ) {
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .then(rowClickableModifier)
                .semantics {
                    contentDescription = spoken
                    this.customActions = customActions
                }
                .heightIn(min = AgentRowMinHeight)
                .padding(
                    start = Space.margin + indent,
                    end = Space.margin,
                    top = Space.m,
                    // The family toggle's 48 dp target spills past its line;
                    // the row keeps room for it, so a press there never
                    // lands on the row below.
                    bottom = if (hasFamilyToggle) Space.m + FamilyToggleSpill else Space.m,
                ),
            verticalAlignment = Alignment.Top,
        ) {
            Box(
                modifier = Modifier
                    .width(AgentMarkColumn)
                    .alignBy { it.measuredHeight / 2 + markOnBaseline },
                contentAlignment = Alignment.CenterStart,
            ) {
                AmuxStateMark(state = rowState, size = 10.dp)
            }

            Column(
                modifier = Modifier
                    .weight(1f)
                    .alignByBaseline(),
                verticalArrangement = Arrangement.spacedBy(Space.xxs),
            ) {
                NameLine(
                    name = row.name,
                    facts = if (isLargeText) null else facts,
                    pinned = row.pinned,
                    nameColor = if (stopped) colors.inkMuted else colors.ink,
                    largeText = isLargeText,
                )

                if (isLargeText && facts.isNotEmpty()) {
                    Text(
                        text = facts,
                        style = typography.caption,
                        color = colors.inkFaint,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                }

                if ((previewAnnotated != null && secondLine != null) || hasFamilyToggle) {
                    Row(
                        modifier = Modifier.fillMaxWidth(),
                        verticalAlignment = Alignment.Bottom,
                        horizontalArrangement = Arrangement.spacedBy(Space.s),
                    ) {
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
                                modifier = Modifier.weight(1f),
                            )
                        } else {
                            Spacer(modifier = Modifier.weight(1f))
                        }
                        if (hasFamilyToggle) {
                            FamilyToggle(row = row, onToggle = onToggleFamily)
                        }
                    }
                }
            }
        }

        if (showDivider) {
            AmuxInsetHairline(leadingInset = Space.margin + AgentMarkColumn + indent)
        }
    }
}

/**
 * The name over the row's facts and pin, on one baseline. The name keeps its
 * room and the facts give way to it, ending in "…", as iOS lays the line out.
 */
@Composable
private fun NameLine(
    name: String,
    facts: String?,
    pinned: Boolean,
    nameColor: Color,
    largeText: Boolean,
) {
    val colors = AmuxTheme.colors
    val typography = AmuxTheme.typography
    Row(
        modifier = Modifier.fillMaxWidth(),
        horizontalArrangement = Arrangement.spacedBy(Space.s),
    ) {
        Text(
            text = name,
            style = typography.identifier,
            color = nameColor,
            maxLines = if (largeText) 2 else 1,
            overflow = TextOverflow.Ellipsis,
            // Unweighted, so it is measured first and keeps its room.
            modifier = Modifier.alignByBaseline(),
        )
        if (facts.isNullOrEmpty()) {
            Spacer(modifier = Modifier.weight(1f))
        } else {
            Text(
                text = facts,
                style = typography.caption,
                color = colors.inkFaint,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                textAlign = TextAlign.End,
                modifier = Modifier
                    .alignByBaseline()
                    .weight(1f, fill = true),
            )
        }
        if (pinned) {
            Icon(
                imageVector = Icons.Filled.PushPin,
                contentDescription = null,
                tint = colors.inkFaint,
                modifier = Modifier
                    .align(Alignment.CenterVertically)
                    .size(12.dp),
            )
        }
    }
}

/**
 * How many agents a head started, and the control that lists them, under the
 * row's age. Its target grows to 48 dp around the count and chevron without
 * pushing the row's lines apart.
 */
private val FamilyToggleSpill = 4.dp

@Composable
private fun FamilyToggle(row: FleetRow, onToggle: () -> Unit) {
    val colors = AmuxTheme.colors
    val toggleDesc = if (row.expanded) {
        stringResource(R.string.home_family_hide, row.children, row.name)
    } else {
        stringResource(R.string.home_family_show, row.children, row.name)
    }
    val foldState = stringResource(if (row.expanded) R.string.fold_state_open else R.string.fold_state_folded)
    Box(
        modifier = Modifier
            .spillTarget()
            .clip(RoundedCornerShape(AmuxTheme.metrics.controlRadius))
            .clickable(role = Role.Button, onClick = onToggle)
            .semantics {
                contentDescription = toggleDesc
                stateDescription = foldState
            }
            .testTag(TestTags.homeFamily(row.name)),
        contentAlignment = Alignment.Center,
    ) {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(Space.xs),
        ) {
            if (row.speaksForAMember) {
                AmuxAttentionDot(size = 7.dp)
            }
            Text(
                text = row.children.toString(),
                style = AmuxTheme.typography.caption,
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
                modifier = Modifier.size(IconSize.Inline),
            )
        }
    }
}

/** The state mark's column, and the step a subagent is indented by. */
private val AgentMarkColumn = 20.dp

/** How far above the name's baseline the mark's centre sits: on its lowercase letters. */
private val MarkAboveBaseline = 5.dp

/** iOS's agent row minimum: a one-line row is no shorter than a two-line one. */
private val AgentRowMinHeight = 64.dp

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

/**
 * The settled agents, behind one row at the head of the list: drawn as a
 * row, so it reads as a place to go, on the agents' columns, with the rule
 * under it inset to their names.
 */
@Composable
private fun SettledHeadRow(
    count: Int,
    onClick: () -> Unit,
) {
    val colors = AmuxTheme.colors
    val title = stringResource(R.string.home_settled_row, count)
    Column(modifier = Modifier.fillMaxWidth()) {
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .heightIn(min = 56.dp)
                .clickable(role = Role.Button, onClick = onClick)
                .semantics(mergeDescendants = true) { contentDescription = title }
                .padding(horizontal = Space.margin, vertical = Space.m)
                .testTag(TestTags.HOME_SETTLED_ROW),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Box(modifier = Modifier.width(AgentMarkColumn), contentAlignment = Alignment.CenterStart) {
                Icon(
                    imageVector = Icons.Outlined.Inventory2,
                    contentDescription = null,
                    tint = colors.inkMuted,
                    modifier = Modifier
                        .offset(x = -SettledGlyphInset)
                        .size(IconSize.Control),
                )
            }
            // A way to somewhere, not an agent: quieter than the names.
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
        AmuxInsetHairline(leadingInset = Space.margin + AgentMarkColumn)
    }
}

/** Moves the archive glyph back so its centre is on the state marks' centre. */
private val SettledGlyphInset = 4.dp

@Composable
private fun rememberHomeCallouts(state: AgentsHomeState): List<HomeCallout> {
    val items = mutableListOf<HomeCallout>()
    for (host in state.discoveredHosts) {
        val offer = rememberPairOffer(host = host, accountPro = state.accountPro)
        items += HomeCallout(
            id = "pair.${host.name}",
            title = offer.title,
            detail = offer.detail,
            icon = pairOfferIcon(offer),
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

/** What can be done to an agent from the list, in the order the sheet offers it. */
internal enum class RowMenuAction { Pin, Unpin, Rename, Settle, Unsettle, CancelSettle, Delete }

/** The actions [row] offers: the long-press sheet's rows and TalkBack's actions on the row. */
internal fun rowMenuActions(row: FleetRow): List<RowMenuAction> = buildList {
    if (row.pinnable) add(if (row.pinned) RowMenuAction.Unpin else RowMenuAction.Pin)
    add(RowMenuAction.Rename)
    when {
        row.closed -> {
            add(RowMenuAction.Unsettle)
            if (row.deletable) add(RowMenuAction.Delete)
        }
        row.card.closeHeld -> add(RowMenuAction.CancelSettle)
        else -> add(RowMenuAction.Settle)
    }
}

@Composable
private fun rowMenuLabel(action: RowMenuAction): String = stringResource(
    when (action) {
        RowMenuAction.Pin -> R.string.home_action_pin
        RowMenuAction.Unpin -> R.string.home_action_unpin
        RowMenuAction.Rename -> R.string.home_action_rename
        RowMenuAction.Settle -> R.string.home_action_settle
        RowMenuAction.Unsettle -> R.string.home_action_unsettle
        RowMenuAction.CancelSettle -> R.string.home_action_cancel_settle
        RowMenuAction.Delete -> R.string.home_action_delete_agent
    },
)

@Composable
private fun AgentRowActionSheet(
    row: FleetRow,
    onDismiss: () -> Unit,
    onAction: (RowMenuAction) -> Unit,
) {
    AmuxSheet(
        onDismissRequest = onDismiss,
    ) {
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .padding(bottom = Space.xl)
                .testTag(TestTags.HOME_ROW_ACTIONS_SHEET),
        ) {
            AmuxSheetHeader(title = row.name)
            for (action in rowMenuActions(row)) {
                AmuxActionRow(
                    icon = when (action) {
                        RowMenuAction.Pin, RowMenuAction.Unpin -> Icons.Outlined.PushPin
                        RowMenuAction.Rename -> Icons.Outlined.Edit
                        RowMenuAction.Settle -> Icons.Outlined.CheckCircle
                        RowMenuAction.Unsettle -> Icons.Outlined.Unarchive
                        RowMenuAction.CancelSettle -> Icons.AutoMirrored.Outlined.Undo
                        RowMenuAction.Delete -> Icons.Outlined.DeleteOutline
                    },
                    label = rowMenuLabel(action),
                    onClick = { onAction(action) },
                    destructive = action == RowMenuAction.Delete,
                    modifier = Modifier.testTag(
                        when (action) {
                            RowMenuAction.Pin -> TestTags.HOME_ACTION_PIN
                            RowMenuAction.Unpin -> TestTags.HOME_ACTION_UNPIN
                            RowMenuAction.Rename -> TestTags.HOME_ACTION_RENAME
                            RowMenuAction.Settle -> TestTags.HOME_ACTION_SETTLE
                            RowMenuAction.Unsettle -> TestTags.HOME_ACTION_UNSETTLE
                            RowMenuAction.CancelSettle -> TestTags.HOME_ACTION_CANCEL_SETTLE
                            RowMenuAction.Delete -> TestTags.HOME_ACTION_DELETE
                        },
                    ),
                )
            }
        }
    }
}
