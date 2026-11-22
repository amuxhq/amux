package sh.amux.features

import sh.amux.design.ThumbTarget
import androidx.compose.foundation.layout.heightIn
import sh.amux.design.IconSize
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Check
import androidx.compose.material.icons.outlined.ContentCopy
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.Scaffold
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
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import sh.amux.design.AmuxGround
import sh.amux.design.AmuxTheme
import sh.amux.design.Space
import sh.amux.values.HostView
import sh.amux.values.Offline
import sh.amux.values.Provider
import sh.amux.values.Reach
import sh.amux.words.Elapsed

/** Semantic test tags for [HostPage]. */
object HostPageTags {
    const val SCREEN: String = "host"
    const val BACK: String = "host.back"
    const val GONE: String = "host.gone"
    const val REACH: String = "host.reach"
    const val MEANING: String = "host.meaning"
    const val STATUS_SECTION: String = "host.status"
    const val FACT_REACH: String = "host.fact.reach"
    const val FACT_LAST_SEEN: String = "host.fact.last-seen"
    const val FACT_PLATFORM: String = "host.fact.platform"
    const val FACT_VERSION: String = "host.fact.version"
    const val FACT_LAST_TRY: String = "host.fact.last-error"
    const val PROVIDERS_SECTION: String = "host.providers"
    const val ADDRESSES_SECTION: String = "host.addresses"
    const val FINGERPRINT_SECTION: String = "host.fingerprint"
    const val FINGERPRINT_ROW: String = "host.fingerprint.row"
    const val COPIED_BADGE: String = "host.copied"
    const val SUBSCRIBE: String = "host.subscribe"
    const val OFFER_SIGN_IN: String = "host.offer-sign-in"
    const val PAIR_AGAIN: String = "host.pair-again"
    const val REVOKE: String = "host.revoke"
    const val REVOKE_DIALOG: String = "host.revoke.dialog"
    const val REVOKE_CONFIRM: String = "host.revoke.confirm"
    const val REVOKE_CANCEL: String = "host.revoke.cancel"

    fun providerRow(name: String): String = "host.provider.${name.lowercase()}"
    fun addressRow(index: Int): String = "host.address.$index"
}

/** Pure immutable UI state rendered by [HostPage]. */
@Immutable
data class HostPageState(
    val host: HostView? = null,
    val fingerprint: String? = null,
    val isConnecting: Boolean = false,
    val foundButUnreachable: Boolean = false,
    val wentOfflineAtMs: Long? = null,
    val nowMs: Long = 0L,
    val canOfferSignIn: Boolean = false,
    val offeringSignIn: Boolean = false,
    val accountPro: Boolean? = null,
)

/** Pure actions emitted by [HostPage]. */
sealed interface HostPageAction {
    data object Back : HostPageAction
    data class PairAgain(val hostId: List<Int>) : HostPageAction
    data class Revoke(val hostId: List<Int>) : HostPageAction
    data object Subscribe : HostPageAction
    data class OfferSignIn(val hostId: List<Int>) : HostPageAction
    data class CopyAddress(val address: String) : HostPageAction
    data class CopyFingerprint(val fingerprint: String) : HostPageAction
}

/**
 * Full-screen detail page for one paired computer: reach status, platform,
 * version, providers, copyable addresses and identity fingerprint, sign-in
 * offer, Pair Again, and Revoke with confirmation dialog.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun HostPage(
    state: HostPageState,
    onAction: (HostPageAction) -> Unit,
    modifier: Modifier = Modifier,
    clock: ElapsedClock = ElapsedClock { state.nowMs },
) {
    val colors = AmuxTheme.colors
    val isLargeText = LocalDensity.current.fontScale >= 1.3f
    val host = state.host
    val reachLine = host?.let { hostReachLine(it, state, clock) }
    var confirmingRevoke by remember { mutableStateOf(false) }
    var copiedKey by remember { mutableStateOf<String?>(null) }
    val scroll = rememberScrollState()

    AmuxGround(
        modifier = modifier.testTag(HostPageTags.SCREEN),
    ) {
        Scaffold(
            containerColor = Color.Transparent,
            contentColor = colors.ink,
            topBar = {
                AmuxTopBar(
                    title = host?.name ?: stringResource(R.string.host_page_title_fallback),
                    subtitle = reachLine,
                    subtitleModifier = Modifier.testTag(HostPageTags.REACH),
                    scrolled = scroll.canScrollBackward,
                    onNavigate = { onAction(HostPageAction.Back) },
                    navigationModifier = Modifier.testTag(HostPageTags.BACK),
                )
            },
        ) { innerPadding ->
            if (host == null) {
                Column(
                    modifier = Modifier
                        .fillMaxSize()
                        .padding(innerPadding)
                        .padding(horizontal = Space.margin, vertical = Space.xl),
                ) {
                    Text(
                        text = stringResource(R.string.host_page_gone),
                        style = AmuxTheme.typography.detail,
                        color = colors.inkMuted,
                        modifier = Modifier.testTag(HostPageTags.GONE),
                    )
                }
            } else {
                Column(
                    modifier = Modifier
                        .fillMaxSize()
                        .padding(innerPadding)
                        .verticalScroll(scroll)
                        .padding(top = Space.m, bottom = Space.xxl),
                    verticalArrangement = Arrangement.spacedBy(Space.l),
                ) {
                    val meaning = hostReachMeaning(host, state)
                    if (meaning != null) {
                        Text(
                            text = meaning,
                            style = AmuxTheme.typography.detail,
                            color = colors.inkMuted,
                            modifier = Modifier
                                .padding(horizontal = Space.margin)
                                .testTag(HostPageTags.MEANING),
                        )
                    }

                    // The way back for a computer that unpaired this phone,
                    // and so the first thing offered; a healthy computer has
                    // nothing to pair again.
                    if (host.reach == Reach.Offline(Offline.Revoked)) {
                        val pairAgainSpoken = stringResource(R.string.host_page_pair_again_spoken, host.name)
                        AmuxActionButton(
                            label = stringResource(R.string.host_page_pair_again),
                            kind = ActionButtonKind.Primary,
                            fillWidth = true,
                            onClick = { onAction(HostPageAction.PairAgain(host.hostId)) },
                            modifier = Modifier
                                .padding(horizontal = Space.margin)
                                .semantics { contentDescription = pairAgainSpoken }
                                .testTag(HostPageTags.PAIR_AGAIN),
                        )
                    }

                    if (host.reachGroup() == HostReachGroup.Away && state.accountPro != true) {
                        val subscribeSpoken = stringResource(R.string.host_page_subscribe_spoken, host.name)
                        AmuxActionButton(
                            label = stringResource(R.string.host_page_subscribe),
                            kind = ActionButtonKind.Primary,
                            fillWidth = true,
                            onClick = { onAction(HostPageAction.Subscribe) },
                            modifier = Modifier
                                .padding(horizontal = Space.margin)
                                .semantics { contentDescription = subscribeSpoken }
                                .testTag(HostPageTags.SUBSCRIBE),
                        )
                    }

                    // Everything known about it, in one surface: what it is,
                    // what it runs, where it is, and the key to compare.
                    AmuxRowCard(
                        modifier = Modifier
                            .padding(horizontal = Space.margin)
                            .testTag(HostPageTags.STATUS_SECTION),
                    ) {
                        var first = true
                        @Composable
                        fun rule() {
                            if (!first) AmuxInsetHairline(leadingInset = Space.l)
                            first = false
                        }
                        val platform = host.platform?.label
                        if (platform != null) {
                            rule()
                            HostFactRow(
                                label = stringResource(R.string.host_page_fact_platform),
                                value = platform,
                                monoValue = false,
                                isLargeText = isLargeText,
                                modifier = Modifier.testTag(HostPageTags.FACT_PLATFORM),
                            )
                        }
                        val version = host.version
                        if (!version.isNullOrEmpty()) {
                            rule()
                            HostFactRow(
                                label = stringResource(R.string.host_page_fact_version),
                                value = version,
                                monoValue = true,
                                isLargeText = isLargeText,
                                modifier = Modifier.testTag(HostPageTags.FACT_VERSION),
                            )
                        }
                        for (signIn in host.providers.filter { it.provider != Provider.Unspecified }) {
                            rule()
                            val name = if (signIn.provider == Provider.Codex) {
                                stringResource(R.string.new_agent_provider_codex)
                            } else {
                                stringResource(R.string.new_agent_provider_claude)
                            }
                            HostFactRow(
                                label = name,
                                value = stringResource(
                                    if (signIn.signedIn) {
                                        R.string.host_page_provider_signed_in
                                    } else {
                                        R.string.host_page_provider_not_signed_in
                                    },
                                ),
                                monoValue = false,
                                isLargeText = isLargeText,
                                modifier = Modifier.testTag(HostPageTags.providerRow(name)),
                            )
                        }
                        if (!host.isOnline &&
                            host.reach != Reach.Offline(Offline.Revoked) &&
                            !host.lastDialError.isNullOrEmpty()
                        ) {
                            rule()
                            HostFactRow(
                                label = stringResource(R.string.host_page_fact_last_try),
                                value = stringResource(R.string.host_page_fact_could_not_connect),
                                monoValue = false,
                                isLargeText = isLargeText,
                                modifier = Modifier.testTag(HostPageTags.FACT_LAST_TRY),
                            )
                        }
                        if (host.addrs.isNotEmpty()) {
                            rule()
                            StackedLabel(
                                text = stringResource(R.string.host_page_section_addresses),
                                modifier = Modifier.testTag(HostPageTags.ADDRESSES_SECTION),
                            )
                            host.addrs.forEachIndexed { index, addr ->
                                val key = "addr:$index:$addr"
                                CopyableMonoRow(
                                    value = addr,
                                    copied = copiedKey == key,
                                    spokenDescription = stringResource(R.string.host_page_copy_address_spoken, addr),
                                    onClick = {
                                        copiedKey = key
                                        onAction(HostPageAction.CopyAddress(addr))
                                    },
                                    modifier = Modifier.testTag(HostPageTags.addressRow(index)),
                                )
                            }
                        }
                        val fingerprint = state.fingerprint?.takeIf { it.isNotBlank() }
                        if (fingerprint != null) {
                            rule()
                            StackedLabel(
                                text = stringResource(R.string.host_page_section_fingerprint),
                                modifier = Modifier.testTag(HostPageTags.FINGERPRINT_SECTION),
                            )
                            CopyableMonoRow(
                                value = FingerprintFormat.grouped(fingerprint),
                                copied = copiedKey == "fingerprint",
                                spokenDescription = stringResource(R.string.host_page_copy_fingerprint_spoken),
                                onClick = {
                                    copiedKey = "fingerprint"
                                    onAction(HostPageAction.CopyFingerprint(fingerprint))
                                },
                                modifier = Modifier.testTag(HostPageTags.FINGERPRINT_ROW),
                            )
                        }
                    }

                    if (state.canOfferSignIn) {
                        val offerLabel = if (state.offeringSignIn) {
                            stringResource(R.string.host_page_offering_sign_in)
                        } else {
                            stringResource(R.string.host_page_offer_sign_in)
                        }
                        val offerSpoken = stringResource(R.string.host_page_offer_sign_in_spoken, host.name)
                        AmuxActionButton(
                            label = offerLabel,
                            kind = ActionButtonKind.Outline,
                            fillWidth = true,
                            enabled = !state.offeringSignIn,
                            onClick = { onAction(HostPageAction.OfferSignIn(host.hostId)) },
                            modifier = Modifier
                                .padding(horizontal = Space.margin)
                                .semantics { if (!state.offeringSignIn) contentDescription = offerSpoken }
                                .testTag(HostPageTags.OFFER_SIGN_IN),
                        )
                    }

                    // Last on the page, its words in the colour kept for what
                    // cannot be undone, and no slab of colour around them.
                    AmuxRowCard(modifier = Modifier.padding(horizontal = Space.margin)) {
                        val revokeSpoken = stringResource(R.string.host_page_revoke_spoken, host.name)
                        Row(
                            modifier = Modifier
                                .fillMaxWidth()
                                .heightIn(min = ThumbTarget.MinSize)
                                .clickable(role = Role.Button) { confirmingRevoke = true }
                                .semantics { contentDescription = revokeSpoken }
                                .padding(horizontal = Space.l, vertical = Space.m)
                                .testTag(HostPageTags.REVOKE),
                            verticalAlignment = Alignment.CenterVertically,
                        ) {
                            Text(
                                text = stringResource(R.string.host_page_revoke),
                                style = AmuxTheme.typography.body,
                                color = colors.removed,
                            )
                        }
                    }
                }
            }
        }
    }

    if (confirmingRevoke && host != null) {
        AlertDialog(
            onDismissRequest = { confirmingRevoke = false },
            containerColor = colors.raised,
            titleContentColor = colors.ink,
            textContentColor = colors.inkMuted,
            modifier = Modifier.testTag(HostPageTags.REVOKE_DIALOG),
            title = {
                Text(
                    text = stringResource(R.string.host_page_revoke_title, host.name),
                    style = AmuxTheme.typography.identifier,
                )
            },
            text = {
                Text(
                    text = stringResource(R.string.host_page_revoke_detail),
                    style = AmuxTheme.typography.detail,
                    color = colors.inkMuted,
                )
            },
            confirmButton = {
                TextButton(
                    onClick = {
                        confirmingRevoke = false
                        onAction(HostPageAction.Revoke(host.hostId))
                    },
                    modifier = Modifier.testTag(HostPageTags.REVOKE_CONFIRM),
                ) {
                    Text(
                        text = stringResource(R.string.host_page_revoke_confirm),
                        style = AmuxTheme.typography.bodyEmphasis,
                        color = colors.removed,
                    )
                }
            },
            dismissButton = {
                TextButton(
                    onClick = { confirmingRevoke = false },
                    modifier = Modifier.testTag(HostPageTags.REVOKE_CANCEL),
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
private fun HostFactRow(
    label: String,
    value: String,
    monoValue: Boolean,
    isLargeText: Boolean,
    modifier: Modifier = Modifier,
) {
    val colors = AmuxTheme.colors
    val valueStyle = if (monoValue) AmuxTheme.typography.monoSmall else AmuxTheme.typography.body
    if (isLargeText) {
        Column(
            modifier = modifier
                .fillMaxWidth()
                .padding(horizontal = Space.l, vertical = Space.m),
            verticalArrangement = Arrangement.spacedBy(Space.xxs),
        ) {
            Text(
                text = label,
                style = AmuxTheme.typography.body,
                color = colors.ink,
            )
            Text(
                text = value,
                style = valueStyle,
                color = colors.inkMuted,
            )
        }
    } else {
        Row(
            modifier = modifier
                .fillMaxWidth()
                .padding(horizontal = Space.l, vertical = Space.m),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(Space.m),
        ) {
            Text(
                text = label,
                style = AmuxTheme.typography.body,
                color = colors.ink,
            )
            Spacer(modifier = Modifier.weight(1f))
            Text(
                text = value,
                style = valueStyle,
                color = colors.inkMuted,
                maxLines = 2,
                overflow = TextOverflow.Ellipsis,
            )
        }
    }
}

/** A label over values stacked under it, as the addresses and the fingerprint are. */
@Composable
private fun StackedLabel(text: String, modifier: Modifier = Modifier) {
    Text(
        text = text,
        style = AmuxTheme.typography.body,
        color = AmuxTheme.colors.ink,
        modifier = modifier
            .fillMaxWidth()
            .padding(start = Space.l, end = Space.l, top = Space.m),
    )
}

@Composable
private fun CopyableMonoRow(
    value: String,
    copied: Boolean,
    spokenDescription: String,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val colors = AmuxTheme.colors
    val copiedLabel = stringResource(R.string.host_page_copied)
    val effectiveDescription = if (copied) copiedLabel else spokenDescription
    Row(
        modifier = modifier
            .fillMaxWidth()
            .heightIn(min = ThumbTarget.MinSize)
            .clickable(role = Role.Button, onClick = onClick)
            .padding(horizontal = Space.l, vertical = Space.s)
            .semantics { contentDescription = effectiveDescription },
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(Space.m),
    ) {
        Text(
            text = value,
            style = AmuxTheme.typography.monoSmall,
            color = colors.inkMuted,
            modifier = Modifier.weight(1f),
        )
        if (copied) {
            Row(
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(Space.xs),
                modifier = Modifier.testTag(HostPageTags.COPIED_BADGE),
            ) {
                Icon(
                    imageVector = Icons.Outlined.Check,
                    contentDescription = null,
                    tint = colors.inkMuted,
                    modifier = Modifier.size(IconSize.Inline),
                )
                Text(
                    text = stringResource(R.string.host_page_copied),
                    style = AmuxTheme.typography.caption,
                    color = colors.inkMuted,
                )
            }
        } else {
            Icon(
                imageVector = Icons.Outlined.ContentCopy,
                contentDescription = null,
                tint = colors.inkFaint,
                modifier = Modifier.size(IconSize.Control),
            )
        }
    }
}

@Composable
private fun hostReachLine(host: HostView, state: HostPageState, clock: ElapsedClock): String = when (host.reachGroup()) {
    // A direct path on the network both are on reads Local; a direct one
    // across networks, as over Tailscale, reads Direct.
    HostReachGroup.OnThisNetwork -> stringResource(
        if (host.sameNetwork) R.string.host_page_reach_local else R.string.host_page_reach_direct,
    )
    HostReachGroup.ThroughTheRelay -> stringResource(R.string.host_page_reach_relay)
    HostReachGroup.Away -> stringResource(R.string.host_page_reach_away)
    HostReachGroup.Offline -> when {
        state.isConnecting -> stringResource(R.string.host_page_reach_connecting)
        host.reach == Reach.Offline(Offline.Revoked) -> stringResource(R.string.host_page_reach_revoked)
        host.reach == Reach.Offline(Offline.SignedOut) -> stringResource(R.string.host_page_reach_signed_out)
        else -> {
            val goneAt = state.wentOfflineAtMs
            val nowMs = if (goneAt != null) clock.nowMs() else 0L
            if (goneAt != null && nowMs >= goneAt) {
                val age = Elapsed.spelled((nowMs - goneAt) / 1_000.0)
                stringResource(R.string.host_page_reach_offline_for, age)
            } else {
                stringResource(R.string.host_page_reach_offline)
            }
        }
    }
}

@Composable
private fun hostReachMeaning(host: HostView, state: HostPageState): String? = when (host.reachGroup()) {
    HostReachGroup.OnThisNetwork, HostReachGroup.ThroughTheRelay -> null
    HostReachGroup.Away -> stringResource(R.string.host_page_meaning_away)
    HostReachGroup.Offline -> when {
        host.reach == Reach.Offline(Offline.Revoked) -> null
        state.foundButUnreachable -> stringResource(R.string.host_page_meaning_unreachable)
        else -> stringResource(R.string.host_page_meaning_offline)
    }
}
