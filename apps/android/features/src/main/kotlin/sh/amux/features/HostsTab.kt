package sh.amux.features

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
import androidx.compose.material.icons.outlined.ChevronRight
import androidx.compose.material.icons.outlined.Computer
import androidx.compose.material.icons.outlined.Dns
import androidx.compose.material.icons.outlined.PersonOff
import androidx.compose.material.icons.outlined.PhoneAndroid
import androidx.compose.material.icons.outlined.PhoneIphone
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.TopAppBarDefaults
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
import sh.amux.design.thumbTarget
import sh.amux.values.HostView
import sh.amux.values.Offline
import sh.amux.values.Platform
import sh.amux.values.Reach
import sh.amux.words.Elapsed

/**
 * Computers tab listing nearby unpaired candidates above paired hosts grouped
 * by reach (`On this network`, `Through the relay`, `Away`, `Offline`).
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun HostsTab(
    state: HostsTabState,
    onAction: (HostsAction) -> Unit,
    modifier: Modifier = Modifier,
) {
    val colors = AmuxTheme.colors
    val subtitle = hostsSubtitle(state)

    AmuxGround(
        modifier = modifier.testTag(TestTags.HOSTS_SCREEN),
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
                        ),
                        title = {
                            Column {
                                Text(
                                    text = stringResource(R.string.hosts_title),
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
                                        modifier = Modifier.testTag(TestTags.HOSTS_SUBTITLE),
                                    )
                                }
                            }
                        },
                    )
                    AmuxHairline(color = colors.hairline.copy(alpha = 0.6f))
                }
            },
        ) { innerPadding ->
            if (state.hosts.isEmpty() && state.discovered.isEmpty()) {
                HostsEmptyContent(contentPadding = innerPadding)
            } else {
                HostsListContent(
                    state = state,
                    onAction = onAction,
                    contentPadding = innerPadding,
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
) {
    val grouped = HostReachGroup.entries.mapNotNull { group ->
        val matching = state.hosts.filter { host ->
            val effectiveGroup = if (state.isConnecting(host)) {
                HostReachGroup.ThroughTheRelay
            } else {
                host.reachGroup()
            }
            effectiveGroup == group
        }.sortedBy { it.name.lowercase() }
        if (matching.isEmpty()) null else group to matching
    }

    LazyColumn(
        modifier = Modifier
            .fillMaxSize()
            .testTag(TestTags.HOSTS_LIST),
        contentPadding = PaddingValues(
            top = contentPadding.calculateTopPadding() + Space.s,
            bottom = contentPadding.calculateBottomPadding() + Space.xxl,
        ),
        verticalArrangement = Arrangement.spacedBy(Space.m),
    ) {
        if (state.discovered.isNotEmpty()) {
            item(key = "hosts-found-section") {
                Column(verticalArrangement = Arrangement.spacedBy(Space.s)) {
                    AmuxSectionHeader(title = stringResource(R.string.hosts_group_found))
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
                    }
                }
            }
        }

        if (state.accountPro == false && state.awayHostName != null) {
            item(key = "hosts-away-callout") {
                AmuxEmptyCard(
                    title = stringResource(R.string.callout_away_title, state.awayHostName),
                    detail = stringResource(R.string.callout_away_detail),
                    icon = Icons.Outlined.PersonOff,
                    buttonLabel = stringResource(R.string.callout_subscribe),
                    buttonTestTag = TestTags.HOSTS_SUBSCRIBE,
                    onButtonClick = { onAction(HostsAction.Subscribe) },
                    modifier = Modifier
                        .padding(horizontal = Space.margin)
                        .testTag(TestTags.HOSTS_SUBSCRIBE),
                )
            }
        }

        for ((group, hostsInGroup) in grouped) {
            item(key = "hosts-group-${group.name}") {
                Column {
                    AmuxSectionHeader(title = reachGroupTitle(group))
                    AmuxRowCard(modifier = Modifier.padding(horizontal = Space.margin)) {
                        hostsInGroup.forEachIndexed { index, host ->
                            HostRowItem(
                                host = host,
                                state = state,
                                onClick = { onAction(HostsAction.Open(host.hostId)) },
                            )
                            if (index < hostsInGroup.lastIndex) {
                                AmuxHairline(
                                    modifier = Modifier.padding(start = 48.dp, end = Space.l),
                                    color = AmuxTheme.colors.hairline.copy(alpha = 0.65f),
                                )
                            }
                        }
                    }
                }
            }
        }
    }
}

@Composable
private fun reachGroupTitle(group: HostReachGroup): String = when (group) {
    HostReachGroup.OnThisNetwork -> stringResource(R.string.hosts_group_on_network)
    HostReachGroup.ThroughTheRelay -> stringResource(R.string.hosts_group_relay)
    HostReachGroup.Away -> stringResource(R.string.hosts_group_away)
    HostReachGroup.Offline -> stringResource(R.string.hosts_group_offline)
}

@Composable
private fun HostRowItem(
    host: HostView,
    state: HostsTabState,
    onClick: () -> Unit,
) {
    val colors = AmuxTheme.colors
    val reachable = host.isOnline || state.isConnecting(host)
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .thumbTarget(minHeight = 56.dp)
            .clickable(role = Role.Button, onClick = onClick)
            .padding(horizontal = Space.l, vertical = Space.m)
            .testTag(TestTags.hostsRow(host.name)),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(Space.m),
    ) {
        Icon(
            imageVector = hostGlyph(host.platform),
            contentDescription = null,
            tint = if (reachable) colors.inkMuted else colors.inkFaint,
            modifier = Modifier.size(20.dp),
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
                text = hostStatusLine(host = host, state = state),
                style = AmuxTheme.typography.detail,
                color = colors.inkMuted,
                maxLines = 2,
                overflow = TextOverflow.Ellipsis,
            )
        }
        Icon(
            imageVector = Icons.Outlined.ChevronRight,
            contentDescription = null,
            tint = colors.inkFaint,
            modifier = Modifier.size(18.dp),
        )
    }
}

private fun hostGlyph(platform: Platform?): ImageVector = when (platform) {
    Platform.Linux -> Icons.Outlined.Dns
    Platform.Ios -> Icons.Outlined.PhoneIphone
    Platform.Android -> Icons.Outlined.PhoneAndroid
    Platform.Macos, Platform.Windows, Platform.Unspecified, null -> Icons.Outlined.Computer
}

@Composable
private fun hostStatusLine(host: HostView, state: HostsTabState): String {
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
                if (goneAt != null && state.nowMs >= goneAt) {
                    val age = Elapsed.spelled((state.nowMs - goneAt) / 1_000.0)
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
