package sh.amux.features

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
import androidx.compose.material.icons.automirrored.outlined.ArrowBack
import androidx.compose.material.icons.outlined.Check
import androidx.compose.material.icons.outlined.ContentCopy
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.TopAppBarDefaults
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
import androidx.compose.ui.unit.dp
import sh.amux.design.AmuxGround
import sh.amux.design.AmuxHairline
import sh.amux.design.AmuxTheme
import sh.amux.design.Space
import sh.amux.design.thumbTarget
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
) {
    val colors = AmuxTheme.colors
    val isLargeText = LocalDensity.current.fontScale >= 1.3f
    val host = state.host
    val reachLine = host?.let { hostReachLine(it, state) }
    var confirmingRevoke by remember { mutableStateOf(false) }
    var copiedKey by remember { mutableStateOf<String?>(null) }

    AmuxGround(
        modifier = modifier.testTag(HostPageTags.SCREEN),
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
                                onClick = { onAction(HostPageAction.Back) },
                                modifier = Modifier
                                    .thumbTarget()
                                    .testTag(HostPageTags.BACK),
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
                                    text = host?.name ?: stringResource(R.string.host_page_title_fallback),
                                    style = AmuxTheme.typography.screenTitle,
                                    color = colors.ink,
                                    maxLines = 1,
                                    overflow = TextOverflow.Ellipsis,
                                )
                                if (reachLine != null) {
                                    Text(
                                        text = reachLine,
                                        style = AmuxTheme.typography.caption,
                                        color = colors.inkMuted,
                                        maxLines = 1,
                                        overflow = TextOverflow.Ellipsis,
                                        modifier = Modifier.testTag(HostPageTags.REACH),
                                    )
                                }
                            }
                        },
                    )
                    AmuxHairline(color = colors.hairline.copy(alpha = 0.6f))
                }
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
                        .verticalScroll(rememberScrollState())
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

                    // Status & reach section
                    Column(
                        modifier = Modifier
                            .fillMaxWidth()
                            .testTag(HostPageTags.STATUS_SECTION),
                    ) {
                        AmuxSectionHeader(title = stringResource(R.string.host_page_section_status))
                        AmuxRowCard(modifier = Modifier.padding(horizontal = Space.margin)) {
                            HostFactRow(
                                label = stringResource(R.string.host_page_fact_reach),
                                value = reachLine.orEmpty(),
                                monoValue = false,
                                isLargeText = isLargeText,
                                modifier = Modifier.testTag(HostPageTags.FACT_REACH),
                            )
                            val goneAt = state.wentOfflineAtMs
                            if (!host.isOnline && goneAt != null && state.nowMs >= goneAt) {
                                AmuxInsetHairline()
                                val elapsed = Elapsed.spelled((state.nowMs - goneAt) / 1_000.0)
                                HostFactRow(
                                    label = stringResource(R.string.host_page_fact_last_seen),
                                    value = stringResource(R.string.host_page_fact_last_seen_ago, elapsed),
                                    monoValue = false,
                                    isLargeText = isLargeText,
                                    modifier = Modifier.testTag(HostPageTags.FACT_LAST_SEEN),
                                )
                            }
                            val platform = host.platform?.label
                            if (platform != null) {
                                AmuxInsetHairline()
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
                                AmuxInsetHairline()
                                HostFactRow(
                                    label = stringResource(R.string.host_page_fact_version),
                                    value = version,
                                    monoValue = true,
                                    isLargeText = isLargeText,
                                    modifier = Modifier.testTag(HostPageTags.FACT_VERSION),
                                )
                            }
                            if (!host.isOnline &&
                                host.reach != Reach.Offline(Offline.Revoked) &&
                                !host.lastDialError.isNullOrEmpty()
                            ) {
                                AmuxInsetHairline()
                                HostFactRow(
                                    label = stringResource(R.string.host_page_fact_last_try),
                                    value = stringResource(R.string.host_page_fact_could_not_connect),
                                    monoValue = false,
                                    isLargeText = isLargeText,
                                    modifier = Modifier.testTag(HostPageTags.FACT_LAST_TRY),
                                )
                            }
                        }
                    }

                    // Providers section
                    val knownProviders = host.providers.filter { it.provider != Provider.Unspecified }
                    if (knownProviders.isNotEmpty()) {
                        Column(
                            modifier = Modifier
                                .fillMaxWidth()
                                .testTag(HostPageTags.PROVIDERS_SECTION),
                        ) {
                            AmuxSectionHeader(
                                title = stringResource(R.string.host_page_section_providers),
                                trailing = knownProviders.size.toString(),
                            )
                            AmuxRowCard(modifier = Modifier.padding(horizontal = Space.margin)) {
                                knownProviders.forEachIndexed { index, signIn ->
                                    if (index > 0) AmuxInsetHairline()
                                    val name = if (signIn.provider == Provider.Codex) {
                                        stringResource(R.string.new_agent_provider_codex)
                                    } else {
                                        stringResource(R.string.new_agent_provider_claude)
                                    }
                                    val status = stringResource(
                                        if (signIn.signedIn) {
                                            R.string.host_page_provider_signed_in
                                        } else {
                                            R.string.host_page_provider_not_signed_in
                                        },
                                    )
                                    HostFactRow(
                                        label = name,
                                        value = status,
                                        monoValue = false,
                                        isLargeText = isLargeText,
                                        modifier = Modifier.testTag(HostPageTags.providerRow(name)),
                                    )
                                }
                            }
                        }
                    }

                    // Addresses section with tap-to-copy
                    if (host.addrs.isNotEmpty()) {
                        Column(
                            modifier = Modifier
                                .fillMaxWidth()
                                .testTag(HostPageTags.ADDRESSES_SECTION),
                        ) {
                            AmuxSectionHeader(title = stringResource(R.string.host_page_section_addresses))
                            AmuxRowCard(modifier = Modifier.padding(horizontal = Space.margin)) {
                                host.addrs.forEachIndexed { index, addr ->
                                    if (index > 0) AmuxInsetHairline()
                                    val key = "addr:$index:$addr"
                                    val copied = copiedKey == key
                                    val spoken = stringResource(R.string.host_page_copy_address_spoken, addr)
                                    CopyableMonoRow(
                                        value = addr,
                                        copied = copied,
                                        spokenDescription = spoken,
                                        onClick = {
                                            copiedKey = key
                                            onAction(HostPageAction.CopyAddress(addr))
                                        },
                                        modifier = Modifier.testTag(HostPageTags.addressRow(index)),
                                    )
                                }
                            }
                        }
                    }

                    // Fingerprint / Identity key section with tap-to-copy
                    val fingerprint = state.fingerprint?.takeIf { it.isNotBlank() }
                    if (fingerprint != null) {
                        val grouped = FingerprintFormat.grouped(fingerprint)
                        val key = "fingerprint"
                        val copied = copiedKey == key
                        val spoken = stringResource(R.string.host_page_copy_fingerprint_spoken)
                        Column(
                            modifier = Modifier
                                .fillMaxWidth()
                                .testTag(HostPageTags.FINGERPRINT_SECTION),
                        ) {
                            AmuxSectionHeader(title = stringResource(R.string.host_page_section_fingerprint))
                            AmuxRowCard(modifier = Modifier.padding(horizontal = Space.margin)) {
                                CopyableMonoRow(
                                    value = grouped,
                                    copied = copied,
                                    spokenDescription = spoken,
                                    onClick = {
                                        copiedKey = key
                                        onAction(HostPageAction.CopyFingerprint(fingerprint))
                                    },
                                    modifier = Modifier.testTag(HostPageTags.FINGERPRINT_ROW),
                                )
                            }
                        }
                    }

                    // Actions section: optional sign-in offer, Pair Again, Revoke
                    Column(
                        modifier = Modifier
                            .fillMaxWidth()
                            .padding(horizontal = Space.margin),
                        verticalArrangement = Arrangement.spacedBy(Space.m),
                    ) {
                        if (state.canOfferSignIn) {
                            val offerLabel = if (state.offeringSignIn) {
                                stringResource(R.string.host_page_offering_sign_in)
                            } else {
                                stringResource(R.string.host_page_offer_sign_in, host.name)
                            }
                            AmuxActionButton(
                                label = offerLabel,
                                kind = ActionButtonKind.Outline,
                                fillWidth = true,
                                enabled = !state.offeringSignIn,
                                onClick = { onAction(HostPageAction.OfferSignIn(host.hostId)) },
                                modifier = Modifier.testTag(HostPageTags.OFFER_SIGN_IN),
                            )
                        }

                        val pairAgainSpoken = stringResource(R.string.host_page_pair_again_spoken, host.name)
                        AmuxActionButton(
                            label = stringResource(R.string.host_page_pair_again),
                            kind = ActionButtonKind.Outline,
                            fillWidth = true,
                            onClick = { onAction(HostPageAction.PairAgain(host.hostId)) },
                            modifier = Modifier
                                .semantics { contentDescription = pairAgainSpoken }
                                .testTag(HostPageTags.PAIR_AGAIN),
                        )

                        val revokeSpoken = stringResource(R.string.host_page_revoke_spoken, host.name)
                        AmuxActionButton(
                            label = stringResource(R.string.host_page_revoke),
                            kind = ActionButtonKind.Destructive,
                            fillWidth = true,
                            onClick = { confirmingRevoke = true },
                            modifier = Modifier
                                .semantics { contentDescription = revokeSpoken }
                                .testTag(HostPageTags.REVOKE),
                        )
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
                        text = stringResource(R.string.host_page_revoke),
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
            .thumbTarget()
            .clickable(role = Role.Button, onClick = onClick)
            .padding(horizontal = Space.l, vertical = Space.m)
            .semantics { contentDescription = effectiveDescription },
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(Space.m),
    ) {
        Text(
            text = value,
            style = AmuxTheme.typography.monoSmall,
            color = colors.ink,
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
                    modifier = Modifier.size(16.dp),
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
                modifier = Modifier.size(18.dp),
            )
        }
    }
}

@Composable
private fun hostReachLine(host: HostView, state: HostPageState): String = when (host.reachGroup()) {
    HostReachGroup.OnThisNetwork -> stringResource(R.string.host_page_reach_direct)
    HostReachGroup.ThroughTheRelay -> stringResource(R.string.host_page_reach_relay)
    HostReachGroup.Away -> stringResource(R.string.host_page_reach_away)
    HostReachGroup.Offline -> when {
        state.isConnecting -> stringResource(R.string.host_page_reach_connecting)
        host.reach == Reach.Offline(Offline.Revoked) -> stringResource(R.string.host_page_reach_revoked)
        host.reach == Reach.Offline(Offline.SignedOut) -> stringResource(R.string.host_page_reach_signed_out)
        else -> {
            val goneAt = state.wentOfflineAtMs
            if (goneAt != null && state.nowMs >= goneAt) {
                val age = Elapsed.spelled((state.nowMs - goneAt) / 1_000.0)
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
