package sh.amux.features

import androidx.compose.runtime.remember
import androidx.compose.runtime.getValue
import androidx.compose.runtime.derivedStateOf
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.lazy.LazyListState
import androidx.compose.foundation.layout.heightIn
import sh.amux.design.IconSize
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.DesktopMac
import androidx.compose.material.icons.outlined.DesktopWindows
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.material.icons.outlined.Dns
import androidx.compose.material.icons.outlined.PersonOff
import androidx.compose.material.icons.outlined.PhoneAndroid
import androidx.compose.material.icons.outlined.PhoneIphone
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import sh.amux.design.AmuxGround
import sh.amux.design.AmuxHairline
import sh.amux.design.AmuxTheme
import sh.amux.design.Space
import sh.amux.values.HostView
import sh.amux.values.Offline
import sh.amux.values.Platform
import sh.amux.values.Reach
import sh.amux.words.Elapsed

/**
 * Computers tab: offers to pair the computers found, then every paired
 * computer in one list, the reachable ones first.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun HostsTab(
    state: HostsTabState,
    onAction: (HostsAction) -> Unit,
    modifier: Modifier = Modifier,
    clock: ElapsedClock = ElapsedClock { state.nowMs },
) {
    val colors = AmuxTheme.colors
    val subtitle = hostsSubtitle(state)
    val listState = rememberLazyListState()

    AmuxGround(
        modifier = modifier.testTag(TestTags.HOSTS_SCREEN),
    ) {
        Scaffold(
            containerColor = Color.Transparent,
            contentColor = colors.ink,
            topBar = {
                val scrolled by remember { derivedStateOf { listState.canScrollBackward } }
                AmuxTopBar(
                    title = stringResource(R.string.hosts_title),
                    large = true,
                    subtitle = subtitle,
                    subtitleModifier = Modifier.testTag(TestTags.HOSTS_SUBTITLE),
                    scrolled = scrolled,
                )
            },
        ) { innerPadding ->
            if (state.hosts.isEmpty() && state.discovered.isEmpty()) {
                HostsEmptyContent(contentPadding = innerPadding)
            } else {
                HostsListContent(
                    state = state,
                    onAction = onAction,
                    contentPadding = innerPadding,
                    clock = clock,
                    listState = listState,
                )
            }
        }
    }
}

@Composable
private fun hostsSubtitle(state: HostsTabState): String? {
    if (state.hosts.isEmpty()) return null
    val connecting = state.hosts.count {
        it.reachGroup() == HostReachGroup.Offline && state.isConnecting(it)
    }
    val reachable = state.hosts.count {
        it.reachGroup() == HostReachGroup.OnThisNetwork ||
            it.reachGroup() == HostReachGroup.ThroughTheRelay
    } + connecting
    val away = state.hosts.count { it.reachGroup() == HostReachGroup.Away }
    val lost = state.hosts.count { it.reachGroup() == HostReachGroup.Offline } - connecting
    val parts = buildList {
        if (reachable > 0 || (away == 0 && lost == 0)) {
            add(stringResource(R.string.hosts_subtitle_reachable, reachable))
        }
        if (away > 0) {
            add(stringResource(R.string.hosts_subtitle_away, away))
        }
        if (lost > 0) {
            add(stringResource(R.string.hosts_subtitle_offline, lost))
        }
    }
    return parts.joinToString(" · ")
}

@Composable
private fun HostsEmptyContent(
    contentPadding: PaddingValues,
) {
    val colors = AmuxTheme.colors
    LazyColumn(
        modifier = Modifier
            .fillMaxSize()
            .testTag(TestTags.HOSTS_LIST),
        contentPadding = PaddingValues(
            start = Space.margin,
            end = Space.margin,
            top = contentPadding.calculateTopPadding() + Space.xl,
            bottom = contentPadding.calculateBottomPadding() + Space.xxl,
        ),
        verticalArrangement = Arrangement.spacedBy(Space.l),
    ) {
        item(key = "hosts-empty") {
            Column(verticalArrangement = Arrangement.spacedBy(Space.s)) {
                Text(
                    text = stringResource(R.string.hosts_empty_title),
                    style = AmuxTheme.typography.screenTitle,
                    color = colors.ink,
                    modifier = Modifier.testTag(TestTags.HOSTS_EMPTY),
                )
                Text(
                    text = stringResource(R.string.hosts_empty_detail),
                    style = AmuxTheme.typography.detail,
                    color = colors.inkMuted,
                )
            }
        }
        item(key = "hosts-empty-hint") {
            Text(
                text = stringResource(R.string.hosts_empty_hint),
                style = AmuxTheme.typography.detail,
                color = colors.inkMuted,
            )
        }
    }
}

@Composable
private fun HostsListContent(
    state: HostsTabState,
    onAction: (HostsAction) -> Unit,
    contentPadding: PaddingValues,
    clock: ElapsedClock,
    listState: LazyListState,
) {
    // One list, reachable first (nearest route first), then away, then
    // offline, by name within each: each row's second line already says how
    // it is reached, so headings over the groups would only repeat it. A
    // computer still being dialled ranks with the relay's, as it did before
    // the app was put away.
    val ordered = remember(state) {
        state.hosts.sortedWith(
            compareBy<HostView>(
                { host -> (if (state.isConnecting(host)) HostReachGroup.ThroughTheRelay else host.reachGroup()).ordinal },
                { host -> host.name.lowercase() },
            ),
        )
    }
    val showAway = state.accountPro == false && state.awayHostName != null

    LazyColumn(
        state = listState,
        modifier = Modifier
            .fillMaxSize()
            .testTag(TestTags.HOSTS_LIST),
        contentPadding = PaddingValues(
            top = contentPadding.calculateTopPadding() + Space.s,
            bottom = contentPadding.calculateBottomPadding() + Space.xxl,
        ),
        verticalArrangement = Arrangement.spacedBy(Space.l),
    ) {
        if (state.discovered.isNotEmpty() || showAway) {
            item(key = "hosts-offers") {
                Column(
                    modifier = Modifier.padding(horizontal = Space.margin),
                    verticalArrangement = Arrangement.spacedBy(Space.s),
                ) {
                    for (host in state.discovered) {
                        val offer = rememberPairOffer(host = host, accountPro = state.accountPro)
                        val actionTag = when {
                            offer.waiting -> TestTags.hostsFound(host.name)
                            offer.subscribe -> TestTags.hostsSubscribe(host.name)
                            else -> TestTags.hostsPair(host.name)
                        }
                        PairOfferCard(
                            host = host,
                            accountPro = state.accountPro,
                            cardTestTag = TestTags.hostsFound(host.name),
                            actionTestTag = actionTag,
                            onPair = { onAction(HostsAction.Pair(host.hostId)) },
                            onSubscribe = { onAction(HostsAction.Subscribe) },
                        )
                    }
                    if (showAway) {
                        AmuxEmptyCard(
                            title = stringResource(R.string.callout_away_title, state.awayHostName.orEmpty()),
                            detail = stringResource(R.string.callout_away_detail),
                            icon = Icons.Outlined.PersonOff,
                            buttonLabel = stringResource(R.string.callout_subscribe),
                            buttonTestTag = TestTags.HOSTS_SUBSCRIBE,
                            onButtonClick = { onAction(HostsAction.Subscribe) },
                            modifier = Modifier.testTag(TestTags.HOSTS_SUBSCRIBE_CARD),
                        )
                    }
                }
            }
        }

        if (ordered.isNotEmpty()) {
            item(key = "hosts-paired") {
                AmuxRowCard(modifier = Modifier.padding(horizontal = Space.margin)) {
                    ordered.forEachIndexed { index, host ->
                        HostRowItem(
                            host = host,
                            state = state,
                            clock = clock,
                            onClick = { onAction(HostsAction.Open(host.hostId)) },
                        )
                        if (index < ordered.lastIndex) {
                            AmuxInsetHairline(leadingInset = HostTextInset)
                        }
                    }
                }
            }
        }
    }
}

/** Where a computer row's name starts inside its card: past its glyph. */
private val HostTextInset = 48.dp

@Composable
private fun HostRowItem(
    host: HostView,
    state: HostsTabState,
    clock: ElapsedClock,
    onClick: () -> Unit,
) {
    val colors = AmuxTheme.colors
    val reachable = host.isOnline || state.isConnecting(host)
    val spoken = hostSpoken(host = host, state = state, clock = clock)
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .heightIn(min = 56.dp)
            .clickable(role = Role.Button, onClick = onClick)
            .semantics(mergeDescendants = true) { contentDescription = spoken }
            .padding(horizontal = Space.l, vertical = Space.m)
            .testTag(TestTags.hostsRow(host.name)),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(Space.m),
    ) {
        Icon(
            imageVector = hostGlyph(host.platform),
            contentDescription = null,
            tint = if (reachable) colors.inkMuted else colors.inkFaint,
            modifier = Modifier.size(IconSize.Row),
        )
        Column(
            modifier = Modifier.weight(1f),
            verticalArrangement = Arrangement.spacedBy(Space.xxs),
        ) {
            Text(
                text = host.name,
                style = AmuxTheme.typography.identifier,
                color = if (reachable) colors.ink else colors.inkMuted,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            Text(
                text = hostStatusLine(host = host, state = state, clock = clock),
                style = AmuxTheme.typography.detail,
                color = colors.inkMuted,
            )
        }
        AmuxDisclosureChevron()
    }
}

private fun hostGlyph(platform: Platform?): ImageVector = when (platform) {
    Platform.Linux -> Icons.Outlined.Dns
    Platform.Ios -> Icons.Outlined.PhoneIphone
    Platform.Android -> Icons.Outlined.PhoneAndroid
    Platform.Windows -> Icons.Outlined.DesktopWindows
    Platform.Macos, Platform.Unspecified, null -> Icons.Outlined.DesktopMac
}

/**
 * What a computer row says to somebody who cannot see it: its name, what it
 * is, how it is reached, and, when it is not, what that means for its agents.
 */
@Composable
private fun hostSpoken(host: HostView, state: HostsTabState, clock: ElapsedClock): String {
    val parts = mutableListOf(host.name)
    host.platform?.label?.let { parts.add(it) }
    when (host.reachGroup()) {
        HostReachGroup.OnThisNetwork -> parts.add(stringResource(R.string.hosts_spoken_direct))
        HostReachGroup.ThroughTheRelay -> parts.add(stringResource(R.string.hosts_spoken_relay))
        HostReachGroup.Away -> {
            parts.add(stringResource(R.string.hosts_spoken_away))
            parts.add(stringResource(R.string.hosts_spoken_away_needs_pro))
        }
        HostReachGroup.Offline -> if (state.isConnecting(host)) {
            parts.add(stringResource(R.string.hosts_spoken_connecting))
        } else {
            parts.add(
                when {
                    host.reach == Reach.Offline(Offline.Revoked) -> stringResource(R.string.hosts_spoken_revoked)
                    host.hostId in state.foundButUnreachableIds -> stringResource(R.string.hosts_spoken_not_answering)
                    host.reach == Reach.Offline(Offline.SignedOut) -> stringResource(R.string.hosts_spoken_signed_out)
                    else -> {
                        val goneAt = state.wentOfflineAtMs[host.hostId]
                        val nowMs = if (goneAt != null) clock.nowMs() else 0L
                        if (goneAt != null && nowMs >= goneAt) {
                            stringResource(R.string.hosts_spoken_offline_for, Elapsed.spelled((nowMs - goneAt) / 1_000.0))
                        } else {
                            stringResource(R.string.hosts_spoken_offline)
                        }
                    }
                },
            )
            parts.add(stringResource(R.string.hosts_spoken_unknown))
        }
    }
    return parts.joinToString(", ")
}

@Composable
private fun hostStatusLine(host: HostView, state: HostsTabState, clock: ElapsedClock): String {
    val parts = mutableListOf<String>()
    host.platform?.label?.let { parts.add(it) }
    val status = when (host.reachGroup()) {
        HostReachGroup.OnThisNetwork -> stringResource(R.string.hosts_status_direct)
        HostReachGroup.ThroughTheRelay -> stringResource(R.string.hosts_status_relay)
        HostReachGroup.Away -> stringResource(R.string.hosts_status_away)
        HostReachGroup.Offline -> when {
            state.isConnecting(host) -> stringResource(R.string.hosts_status_connecting)
            host.reach == Reach.Offline(Offline.Revoked) -> stringResource(R.string.hosts_status_revoked)
            host.hostId in state.foundButUnreachableIds -> stringResource(R.string.hosts_status_not_answering)
            host.reach == Reach.Offline(Offline.SignedOut) -> stringResource(R.string.hosts_status_signed_out)
            else -> {
                val goneAt = state.wentOfflineAtMs[host.hostId]
                // Read here, in the row, so the clock's tick redraws this row only.
                val nowMs = if (goneAt != null) clock.nowMs() else 0L
                if (goneAt != null && nowMs >= goneAt) {
                    val age = Elapsed.spelled((nowMs - goneAt) / 1_000.0)
                    stringResource(R.string.hosts_status_offline_for, age)
                } else {
                    stringResource(R.string.hosts_status_offline)
                }
            }
        }
    }
    parts.add(status)
    return parts.joinToString(" · ")
}
