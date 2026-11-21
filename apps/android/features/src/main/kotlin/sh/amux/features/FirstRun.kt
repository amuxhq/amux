package sh.amux.features

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
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
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.AddCircleOutline
import androidx.compose.material.icons.outlined.Computer
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import sh.amux.design.AmuxTheme
import sh.amux.design.Space
import sh.amux.design.thumbTarget
import sh.amux.values.HostView
import sh.amux.words.RowState

/**
 * Shared presentation of an unpaired computer discovered on this network,
 * through a paired host, or over the relay.
 */
internal data class PairOfferPresentation(
    val title: String,
    val detail: String,
    val buttonLabel: String?,
    val subscribe: Boolean,
    val waiting: Boolean,
)

@Composable
internal fun rememberPairOffer(host: HostView, accountPro: Boolean?): PairOfferPresentation {
    val waiting = host.hasClosedWindow
    val through = host.through
    return when {
        host.hasOpenWindow -> PairOfferPresentation(
            title = stringResource(R.string.pair_offer_window_open_title, host.name),
            detail = stringResource(R.string.pair_offer_window_open_detail),
            buttonLabel = stringResource(R.string.pair_offer_button_pair),
            subscribe = false,
            waiting = false,
        )
        waiting -> PairOfferPresentation(
            title = stringResource(R.string.pair_offer_nearby_title, host.name),
            detail = stringResource(R.string.pair_offer_nearby_detail, host.name),
            buttonLabel = null,
            subscribe = false,
            waiting = true,
        )
        through != null -> PairOfferPresentation(
            title = stringResource(R.string.pair_offer_online_title, host.name),
            detail = stringResource(R.string.pair_offer_through_detail, through),
            buttonLabel = stringResource(R.string.pair_offer_button_pair),
            subscribe = false,
            waiting = false,
        )
        host.relayOnly -> {
            val needsPro = accountPro == false
            PairOfferPresentation(
                title = stringResource(R.string.pair_offer_online_title, host.name),
                detail = if (needsPro) {
                    stringResource(R.string.pair_offer_relay_free_detail)
                } else {
                    stringResource(R.string.pair_offer_relay_detail)
                },
                buttonLabel = if (needsPro) {
                    stringResource(R.string.pair_offer_button_subscribe)
                } else {
                    stringResource(R.string.pair_offer_button_pair)
                },
                subscribe = needsPro,
                waiting = false,
            )
        }
        else -> PairOfferPresentation(
            title = stringResource(R.string.pair_offer_generic_title, host.name),
            detail = stringResource(R.string.pair_offer_generic_detail),
            buttonLabel = stringResource(R.string.pair_offer_button_pair),
            subscribe = false,
            waiting = false,
        )
    }
}

@Composable
internal fun PairOfferCard(
    host: HostView,
    accountPro: Boolean?,
    cardTestTag: String,
    actionTestTag: String,
    onPair: () -> Unit,
    onSubscribe: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val offer = rememberPairOffer(host = host, accountPro = accountPro)
    AmuxEmptyCard(
        title = offer.title,
        detail = offer.detail,
        icon = if (offer.waiting) Icons.Outlined.Computer else Icons.Outlined.AddCircleOutline,
        buttonLabel = offer.buttonLabel,
        buttonTestTag = actionTestTag,
        onButtonClick = if (offer.waiting) null else {
            if (offer.subscribe) onSubscribe else onPair
        },
        modifier = modifier.testTag(cardTestTag),
    )
}

/**
 * First-run Agents screen when no computer has been paired yet.
 *
 * Android does not require an iOS-style local network permission interstitial,
 * so discovery starts immediately and either shows nearby computers to pair
 * with one tap or guides the user through two clear steps on their computer.
 */
@Composable
internal fun FirstRunContent(
    state: AgentsHomeState,
    onAction: (HomeAction) -> Unit,
    contentPadding: PaddingValues,
    modifier: Modifier = Modifier,
) {
    val colors = AmuxTheme.colors
    LazyColumn(
        modifier = modifier
            .fillMaxSize()
            .testTag(TestTags.HOME_LIST),
        contentPadding = PaddingValues(
            start = Space.margin,
            end = Space.margin,
            top = contentPadding.calculateTopPadding() + Space.xl,
            bottom = contentPadding.calculateBottomPadding() + Space.xxl,
        ),
        verticalArrangement = Arrangement.spacedBy(Space.xl),
    ) {
        item(key = "first-run-header") {
            Column(verticalArrangement = Arrangement.spacedBy(Space.s)) {
                Text(
                    text = stringResource(R.string.first_run_title),
                    style = AmuxTheme.typography.screenTitle,
                    color = colors.ink,
                    modifier = Modifier.testTag(TestTags.HOME_EMPTY_FIRST_RUN),
                )
                Text(
                    text = stringResource(R.string.first_run_explain),
                    style = AmuxTheme.typography.detail,
                    color = colors.inkMuted,
                )
            }
        }

        if (state.discoveredHosts.isNotEmpty()) {
            item(key = "first-run-found") {
                Column(verticalArrangement = Arrangement.spacedBy(Space.s)) {
                    for (host in state.discoveredHosts) {
                        val offer = rememberPairOffer(host = host, accountPro = state.accountPro)
                        val actionTag = when {
                            offer.waiting -> TestTags.homeFound(host.name)
                            offer.subscribe -> TestTags.homeSubscribe(host.name)
                            else -> TestTags.homePair(host.name)
                        }
                        PairOfferCard(
                            host = host,
                            accountPro = state.accountPro,
                            cardTestTag = TestTags.homeFound(host.name),
                            actionTestTag = actionTag,
                            onPair = { onAction(HomeAction.Pair(host.hostId)) },
                            onSubscribe = { onAction(HomeAction.Subscribe) },
                        )
                    }
                }
            }
        } else {
            item(key = "first-run-looking") {
                Row(
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(Space.m),
                ) {
                    AmuxStateMark(state = RowState.Working, size = 10.dp)
                    Text(
                        text = stringResource(R.string.first_run_looking),
                        style = AmuxTheme.typography.detail,
                        color = colors.inkMuted,
                        modifier = Modifier.testTag(TestTags.HOME_EMPTY_LOOKING),
                    )
                }
            }

            item(key = "first-run-steps") {
                Column(verticalArrangement = Arrangement.spacedBy(Space.l)) {
                    FirstRunStep(number = stringResource(R.string.first_run_step_1_number)) {
                        Column(verticalArrangement = Arrangement.spacedBy(Space.m)) {
                            Text(
                                text = stringResource(R.string.first_run_step_1),
                                style = AmuxTheme.typography.body,
                                color = colors.ink,
                            )
                            AmuxActionButton(
                                label = stringResource(R.string.first_run_send_link),
                                kind = ActionButtonKind.Outline,
                                onClick = { onAction(HomeAction.SendInstallLink) },
                                modifier = Modifier.testTag(TestTags.HOME_EMPTY_SEND_LINK),
                            )
                        }
                    }
                    FirstRunStep(number = stringResource(R.string.first_run_step_2_number)) {
                        Text(
                            text = stringResource(R.string.first_run_step_2),
                            style = AmuxTheme.typography.body,
                            color = colors.ink,
                        )
                    }
                }
            }
        }

        if (!state.signedIn) {
            item(key = "first-run-account") {
                Row(
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(Space.xs),
                ) {
                    Text(
                        text = stringResource(R.string.first_run_have_account),
                        style = AmuxTheme.typography.detail,
                        color = colors.inkMuted,
                    )
                    if (state.signingIn) {
                        Row(
                            verticalAlignment = Alignment.CenterVertically,
                            horizontalArrangement = Arrangement.spacedBy(Space.s),
                            modifier = Modifier
                                .thumbTarget()
                                .testTag(TestTags.HOME_EMPTY_SIGN_IN),
                        ) {
                            AmuxStateMark(state = RowState.Working, size = 8.dp)
                            Text(
                                text = stringResource(R.string.callout_signing_in),
                                style = AmuxTheme.typography.detail,
                                color = colors.inkMuted,
                            )
                        }
                    } else {
                        Box(
                            modifier = Modifier
                                .thumbTarget()
                                .clip(CircleShape)
                                .clickable(role = Role.Button) { onAction(HomeAction.SignIn) }
                                .padding(horizontal = Space.s)
                                .testTag(TestTags.HOME_EMPTY_SIGN_IN),
                            contentAlignment = Alignment.Center,
                        ) {
                            Text(
                                text = stringResource(R.string.callout_sign_in),
                                style = AmuxTheme.typography.detail.copy(fontWeight = FontWeight.SemiBold),
                                color = colors.accent,
                            )
                        }
                    }
                }
            }
        }
    }
}

@Composable
private fun FirstRunStep(
    number: String,
    content: @Composable () -> Unit,
) {
    val colors = AmuxTheme.colors
    Row(
        verticalAlignment = Alignment.Top,
        horizontalArrangement = Arrangement.spacedBy(Space.m),
        modifier = Modifier.fillMaxWidth(),
    ) {
        Box(
            modifier = Modifier
                .size(24.dp)
                .clip(CircleShape)
                .background(colors.sunken),
            contentAlignment = Alignment.Center,
        ) {
            Text(
                text = number,
                style = AmuxTheme.typography.caption.copy(fontWeight = FontWeight.SemiBold),
                color = colors.ink,
            )
        }
        Box(modifier = Modifier.weight(1f)) {
            content()
        }
    }
}

/**
 * Shown when at least one computer is paired, but no agents are running yet.
 */
@Composable
internal fun NoAgentsYetContent(
    state: AgentsHomeState,
    callouts: List<HomeCallout>,
    onAction: (HomeAction) -> Unit,
    contentPadding: PaddingValues,
    modifier: Modifier = Modifier,
) {
    val colors = AmuxTheme.colors
    LazyColumn(
        modifier = modifier
            .fillMaxSize()
            .testTag(TestTags.HOME_LIST),
        contentPadding = PaddingValues(
            start = Space.margin,
            end = Space.margin,
            top = contentPadding.calculateTopPadding() + Space.l,
            bottom = contentPadding.calculateBottomPadding() + 96.dp,
        ),
        verticalArrangement = Arrangement.spacedBy(Space.l),
    ) {
        if (callouts.isNotEmpty()) {
            item(key = "no-agents-callouts") {
                Column(verticalArrangement = Arrangement.spacedBy(Space.s)) {
                    for (callout in callouts) {
                        HomeCalloutView(callout = callout, onAction = onAction)
                    }
                }
            }
        }

        item(key = "no-agents-body") {
            Column(verticalArrangement = Arrangement.spacedBy(Space.l)) {
                Spacer(modifier = Modifier.height(Space.xs))
                Column(verticalArrangement = Arrangement.spacedBy(Space.s)) {
                    Text(
                        text = stringResource(R.string.home_no_agents_title),
                        style = AmuxTheme.typography.screenTitle,
                        color = colors.ink,
                        modifier = Modifier.testTag(TestTags.HOME_EMPTY_NO_AGENTS),
                    )
                    Text(
                        text = stringResource(R.string.home_no_agents_detail),
                        style = AmuxTheme.typography.detail,
                        color = colors.inkMuted,
                    )
                }
                if (state.offersNewAgent) {
                    AmuxActionButton(
                        label = stringResource(R.string.home_new_agent),
                        kind = ActionButtonKind.Primary,
                        fillWidth = true,
                        onClick = { onAction(HomeAction.NewAgent) },
                        modifier = Modifier.testTag(TestTags.HOME_EMPTY_NEW_AGENT),
                    )
                }
            }
        }
    }
}
