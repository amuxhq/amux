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
                            )
                        }
                    } else {
                        null
                    },
                ) {
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
                            modifier = Modifier.testTag(TestTags.HOME_SEARCH_TOGGLE),
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
                        modifier = Modifier
                            .semantics { contentDescription = newAgentLabel }
                            .testTag(TestTags.HOME_FAB_NEW_AGENT),
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
                confirming = RowConfirmation.Delete(currentActionRow)
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
                    workspaceGoes = dialog.row.card.workspaceGoes,
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
            leadingIcon = {
                Icon(
                    imageVector = Icons.Outlined.Search,
                    contentDescription = null,
                    tint = colors.inkMuted,
                )
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
                        menuTag = TestTags.settledRowMenu(row.name),
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
    rowTag: String = TestTags.homeRow(row.name),
    menuTag: String = TestTags.homeRowMenu(row.name),
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
            .testTag(rowTag),
    ) {
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .then(rowClickableModifier)
                .heightIn(min = AgentRowMinHeight)
                .padding(
                    start = Space.margin + indent,
                    end = Space.margin,
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
                            imageVector = Icons.Filled.PushPin,
                            contentDescription = pinnedDesc,
                            tint = colors.inkFaint,
                            modifier = Modifier.size(12.dp),
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
                // The toggle's 48 dp target reaches left over the row's text
                // rather than right, so it ends where the More button's
                // target begins and a tap between them has one owner.
                Box(
                    modifier = Modifier
                        .padding(start = Space.s)
                        .widthIn(min = ThumbTarget.MinSize)
                        .heightIn(min = ThumbTarget.MinSize)
                        .clip(RoundedCornerShape(AmuxTheme.metrics.controlRadius))
                        .clickable(role = Role.Button, onClick = onToggleFamily)
                        .semantics { contentDescription = toggleDesc }
                        .testTag(TestTags.homeFamily(row.name)),
                    contentAlignment = Alignment.CenterEnd,
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
                        modifier = Modifier.size(IconSize.Inline),
                    )
                }
                }
            }

            val moreDesc = stringResource(R.string.action_more_options)
            // Room for the More button's target to spill into, so it meets
            // the family toggle's target instead of covering it.
            Spacer(modifier = Modifier.width(AmuxEdgeSpill))
            AmuxEdgeIconButton(
                icon = Icons.Outlined.MoreVert,
                contentDescription = moreDesc,
                onClick = onOpenMenu,
                size = IconSize.Control,
                ink = GlyphInk.MoreVert,
                tint = colors.inkFaint,
                modifier = Modifier.testTag(menuTag),
            )
        }

        if (showDivider) {
            AmuxInsetHairline(leadingInset = Space.margin + 20.dp + indent)
        }
    }
}

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
                .heightIn(min = 56.dp)
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
                modifier = Modifier.size(IconSize.Control),
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
            if (row.pinnable) {
                if (row.pinned) {
                    AmuxActionRow(
                        icon = Icons.Outlined.PushPin,
                        label = stringResource(R.string.home_action_unpin),
                        onClick = onUnpin,
                        modifier = Modifier.testTag(TestTags.HOME_ACTION_UNPIN),
                    )
                } else {
                    AmuxActionRow(
                        icon = Icons.Outlined.PushPin,
                        label = stringResource(R.string.home_action_pin),
                        onClick = onPin,
                        modifier = Modifier.testTag(TestTags.HOME_ACTION_PIN),
                    )
                }
            }
            AmuxActionRow(
                icon = Icons.Outlined.Edit,
                label = stringResource(R.string.home_action_rename),
                onClick = onRename,
                modifier = Modifier.testTag(TestTags.HOME_ACTION_RENAME),
            )
            when {
                row.closed -> {
                    AmuxActionRow(
                        icon = Icons.Outlined.Unarchive,
                        label = stringResource(R.string.home_action_unsettle),
                        onClick = onUnsettle,
                        modifier = Modifier.testTag(TestTags.HOME_ACTION_UNSETTLE),
                    )
                    if (row.deletable) {
                        AmuxActionRow(
                            icon = Icons.Outlined.DeleteOutline,
                            label = stringResource(R.string.home_action_delete_agent),
                            onClick = onDelete,
                            destructive = true,
                            modifier = Modifier.testTag(TestTags.HOME_ACTION_DELETE),
                        )
                    }
                }
                row.card.closeHeld -> {
                    AmuxActionRow(
                        icon = Icons.AutoMirrored.Outlined.Undo,
                        label = stringResource(R.string.home_action_cancel_settle),
                        onClick = onCancelSettle,
                        modifier = Modifier.testTag(TestTags.HOME_ACTION_CANCEL_SETTLE),
                    )
                }
                else -> {
                    AmuxActionRow(
                        icon = Icons.Outlined.CheckCircle,
                        label = stringResource(R.string.home_action_settle),
                        onClick = onSettle,
                        modifier = Modifier.testTag(TestTags.HOME_ACTION_SETTLE),
                    )
                }
            }
        }
    }
}

