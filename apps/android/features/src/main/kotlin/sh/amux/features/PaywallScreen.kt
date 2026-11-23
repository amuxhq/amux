package sh.amux.features

import androidx.compose.foundation.layout.Box

import sh.amux.design.spillTarget

import sh.amux.design.ThumbTarget
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.navigationBars
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.RadioButton
import androidx.compose.material3.RadioButtonDefaults
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Immutable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import sh.amux.design.AmuxGround
import sh.amux.design.AmuxHairline
import sh.amux.design.AmuxPanel
import sh.amux.design.AmuxTheme
import sh.amux.design.Space
import sh.amux.words.RowState

/** Semantic test tags for [PaywallScreen] and its interactive controls. */
object PaywallTags {
    const val SCREEN: String = TestTags.SCREEN_PAYWALL
    const val BACK: String = TestTags.NAV_BACK
    const val TITLE: String = "paywall.title"
    const val EXPLANATION: String = "paywall.explanation"

    const val PLAN_MONTHLY: String = "paywall.plan.monthly"
    const val PLAN_YEARLY: String = "paywall.plan.yearly"

    const val SUBSCRIBE_BUTTON: String = "paywall.subscribe"
    const val WAITING_LINE: String = "paywall.waiting"
    const val RESTORE_BUTTON: String = "paywall.restore"
    const val RETRY_BUTTON: String = "paywall.retry"
    const val MANAGE_WEB_BUTTON: String = "paywall.manage.web"
    const val MANAGE_PLAY_BUTTON: String = "paywall.manage.play"
    const val PRIVACY_LINK: String = "paywall.privacy"

    const val ENTITLED_CARD: String = "paywall.entitled"
    const val UNAVAILABLE_CARD: String = "paywall.unavailable"
    const val PENDING_BANNER: String = "paywall.banner.pending"
    const val UNCONFIRMED_BANNER: String = "paywall.banner.unconfirmed"
    const val FAILED_BANNER: String = "paywall.banner.failed"
}

/** Billing cadence for a selectable subscription plan. */
enum class PaywallPlanPeriod {
    Monthly,
    Yearly,
}

/** One subscription plan offered on [PaywallScreen]. */
@Immutable
data class PaywallPlanUi(
    val id: String,
    val period: PaywallPlanPeriod,
    val price: String,
    val saving: String? = null,
)

/** Where an active entitlement originated. */
enum class PaywallGrantSource {
    PlayStore,
    AppStore,
    Web,
    Granted,
    Trial,
}

/** Why a completed store purchase has not yet turned into an active entitlement. */
@Immutable
sealed interface PaywallUnconfirmedReason {
    data object Unreachable : PaywallUnconfirmedReason
    data class Refused(val reason: String) : PaywallUnconfirmedReason
    data object SwitchingOn : PaywallUnconfirmedReason
}

/** Current paywall phase rendered by [PaywallScreen]. */
@Immutable
sealed interface PaywallPhaseUi {
    data object Ready : PaywallPhaseUi
    data object Buying : PaywallPhaseUi
    data object AwaitingApproval : PaywallPhaseUi
    data object Confirming : PaywallPhaseUi
    data class Unconfirmed(val reason: PaywallUnconfirmedReason) : PaywallPhaseUi
    data class Failed(val reason: String) : PaywallPhaseUi
    data class StoreUnavailable(val message: String? = null) : PaywallPhaseUi
    data class Entitled(
        val source: PaywallGrantSource,
        val renewalOrExpiryText: String? = null,
    ) : PaywallPhaseUi
}

/** Pure UI state rendered by [PaywallScreen]. */
@Immutable
data class PaywallScreenState(
    val plans: List<PaywallPlanUi> = emptyList(),
    val chosen: PaywallPlanPeriod = PaywallPlanPeriod.Yearly,
    val phase: PaywallPhaseUi = PaywallPhaseUi.Ready,
    val manageUrl: String = "https://amux.sh/account",
    val privacyUrl: String = "https://amux.sh/privacy-policy/",
    val accountEmail: String? = null,
) {
    val selectedPlan: PaywallPlanUi?
        get() = plans.firstOrNull { it.period == chosen } ?: plans.firstOrNull()

    val entitled: Boolean
        get() = phase is PaywallPhaseUi.Entitled

    val working: Boolean
        get() = phase == PaywallPhaseUi.Buying || phase == PaywallPhaseUi.Confirming

    val settling: Boolean
        get() = phase == PaywallPhaseUi.Confirming || phase is PaywallPhaseUi.Unconfirmed

    val storeUnavailable: Boolean
        get() = phase is PaywallPhaseUi.StoreUnavailable ||
            (plans.isEmpty() && phase == PaywallPhaseUi.Ready)
}

/** Actions emitted by [PaywallScreen] without touching billing or browser APIs directly. */
sealed interface PaywallAction {
    data object Back : PaywallAction
    data class ChoosePlan(val period: PaywallPlanPeriod) : PaywallAction
    data object Subscribe : PaywallAction
    data object RestorePurchases : PaywallAction
    data object Retry : PaywallAction
    data class ManageOnWeb(val url: String) : PaywallAction
    data object ManageInStore : PaywallAction
    data class OpenLegalUrl(val url: String) : PaywallAction
}

/**
 * Full-screen Pro subscription screen supporting active entitlements, selectable
 * plans, pending/unconfirmed/failed recovery, and honest web-managed fallback
 * when Google Play Billing is not enabled in the current build.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun PaywallScreen(
    state: PaywallScreenState,
    onAction: (PaywallAction) -> Unit,
    modifier: Modifier = Modifier,
) {
    val colors = AmuxTheme.colors
    val backLabel = stringResource(R.string.action_back)
    val largeText = LocalDensity.current.fontScale >= 1.3f
    val scroll = rememberScrollState()

    AmuxGround(modifier = modifier.testTag(PaywallTags.SCREEN)) {
        Scaffold(
            containerColor = Color.Transparent,
            contentColor = colors.ink,
            topBar = {
                AmuxTopBar(
                    title = stringResource(R.string.paywall_title),
                    subtitle = state.accountEmail?.takeIf { it.isNotBlank() },
                    scrolled = scroll.canScrollBackward,
                    onNavigate = { onAction(PaywallAction.Back) },
                    navigationLabel = backLabel,
                    navigationModifier = Modifier.testTag(PaywallTags.BACK),
                )
            },
            bottomBar = {
                PaywallBottomBar(state = state, onAction = onAction)
            },
        ) { innerPadding ->
            Column(
                modifier = Modifier
                    .fillMaxSize()
                    .padding(innerPadding)
                    .verticalScroll(scroll)
                    .padding(horizontal = Space.margin, vertical = Space.l),
                verticalArrangement = Arrangement.spacedBy(Space.l),
            ) {
                Column(verticalArrangement = Arrangement.spacedBy(Space.s)) {
                    Text(
                        text = stringResource(R.string.paywall_headline),
                        style = AmuxTheme.typography.screenTitle,
                        color = colors.ink,
                        modifier = Modifier.testTag(PaywallTags.TITLE),
                    )
                    Text(
                        text = stringResource(R.string.paywall_explanation),
                        style = AmuxTheme.typography.body,
                        color = colors.inkMuted,
                        modifier = Modifier.testTag(PaywallTags.EXPLANATION),
                    )
                }

                when (val phase = state.phase) {
                    is PaywallPhaseUi.Entitled -> {
                        EntitledCard(phase = phase)
                    }
                    is PaywallPhaseUi.StoreUnavailable -> {
                        StoreUnavailableCard(message = phase.message)
                    }
                    else -> {
                        PaywallPhaseBanner(phase = phase)
                        if (!state.settling) {
                            if (state.plans.isEmpty() && phase == PaywallPhaseUi.Ready) {
                                StoreUnavailableCard(message = null)
                            } else if (state.plans.isNotEmpty()) {
                                PlanSelector(
                                    plans = state.plans,
                                    chosen = state.chosen,
                                    enabled = !state.working && phase != PaywallPhaseUi.AwaitingApproval,
                                    largeText = largeText,
                                    onChoose = { onAction(PaywallAction.ChoosePlan(it)) },
                                )
                                PaywallTerms(
                                    privacyUrl = state.privacyUrl,
                                    onAction = onAction,
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
private fun EntitledCard(phase: PaywallPhaseUi.Entitled) {
    val colors = AmuxTheme.colors
    val badgeText = stringResource(
        if (phase.source == PaywallGrantSource.Trial) {
            R.string.paywall_entitled_badge_trial
        } else {
            R.string.paywall_entitled_badge_pro
        },
    )
    val headline = when (phase.source) {
        PaywallGrantSource.PlayStore -> stringResource(R.string.paywall_entitled_play_title)
        PaywallGrantSource.AppStore -> stringResource(R.string.paywall_entitled_appstore_title)
        PaywallGrantSource.Web -> stringResource(R.string.paywall_entitled_web_title)
        PaywallGrantSource.Granted -> stringResource(R.string.paywall_entitled_granted_title)
        PaywallGrantSource.Trial -> stringResource(R.string.paywall_entitled_trial_title)
    }
    val detail = when (phase.source) {
        PaywallGrantSource.PlayStore -> stringResource(R.string.paywall_entitled_play_detail)
        PaywallGrantSource.AppStore -> stringResource(R.string.paywall_entitled_appstore_detail)
        PaywallGrantSource.Web -> stringResource(R.string.paywall_entitled_web_detail)
        PaywallGrantSource.Granted -> stringResource(R.string.paywall_entitled_granted_detail)
        PaywallGrantSource.Trial -> stringResource(R.string.paywall_entitled_trial_detail)
    }

    AmuxPanel(
        modifier = Modifier
            .fillMaxWidth()
            .testTag(PaywallTags.ENTITLED_CARD),
    ) {
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .padding(Space.l),
            verticalArrangement = Arrangement.spacedBy(Space.s),
        ) {
            AmuxStatusChip(text = badgeText)
            Text(
                text = headline,
                style = AmuxTheme.typography.bodyEmphasis,
                color = colors.ink,
            )
            if (!phase.renewalOrExpiryText.isNullOrBlank()) {
                Text(
                    text = phase.renewalOrExpiryText,
                    style = AmuxTheme.typography.body,
                    color = colors.inkMuted,
                )
            }
            Text(
                text = detail,
                style = AmuxTheme.typography.detail,
                color = colors.inkMuted,
            )
            Text(
                text = stringResource(R.string.paywall_entitled_elsewhere),
                style = AmuxTheme.typography.caption,
                color = colors.inkFaint,
            )
        }
    }
}

@Composable
private fun StoreUnavailableCard(message: String?) {
    val colors = AmuxTheme.colors
    AmuxPanel(
        modifier = Modifier
            .fillMaxWidth()
            .testTag(PaywallTags.UNAVAILABLE_CARD),
    ) {
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .padding(Space.l),
            verticalArrangement = Arrangement.spacedBy(Space.s),
        ) {
            Text(
                text = stringResource(R.string.paywall_unavailable_title),
                style = AmuxTheme.typography.bodyEmphasis,
                color = colors.ink,
            )
            Text(
                text = message?.takeIf { it.isNotBlank() }
                    ?: stringResource(R.string.paywall_unavailable_detail),
                style = AmuxTheme.typography.detail,
                color = colors.inkMuted,
            )
        }
    }
}

@Composable
private fun PaywallPhaseBanner(phase: PaywallPhaseUi) {
    val colors = AmuxTheme.colors
    when (phase) {
        PaywallPhaseUi.AwaitingApproval -> {
            AmuxPanel(
                modifier = Modifier
                    .fillMaxWidth()
                    .testTag(PaywallTags.PENDING_BANNER),
            ) {
                Column(
                    modifier = Modifier
                        .fillMaxWidth()
                        .padding(Space.l),
                    verticalArrangement = Arrangement.spacedBy(Space.xs),
                ) {
                    Text(
                        text = stringResource(R.string.paywall_waiting_approval),
                        style = AmuxTheme.typography.bodyEmphasis,
                        color = colors.ink,
                    )
                    Text(
                        text = stringResource(R.string.paywall_waiting_approval_detail),
                        style = AmuxTheme.typography.detail,
                        color = colors.inkMuted,
                    )
                }
            }
        }
        is PaywallPhaseUi.Unconfirmed -> {
            val headline = when (phase.reason) {
                PaywallUnconfirmedReason.SwitchingOn -> stringResource(R.string.paywall_switching_on_title)
                PaywallUnconfirmedReason.Unreachable,
                is PaywallUnconfirmedReason.Refused,
                -> stringResource(R.string.paywall_unconfirmed_title)
            }
            val detail = when (val why = phase.reason) {
                PaywallUnconfirmedReason.Unreachable -> stringResource(R.string.paywall_unconfirmed_unreachable)
                is PaywallUnconfirmedReason.Refused -> stringResource(
                    R.string.paywall_unconfirmed_refused,
                    finishSentence(why.reason),
                )
                PaywallUnconfirmedReason.SwitchingOn -> stringResource(R.string.paywall_switching_on_detail)
            }
            AmuxPanel(
                borderColor = colors.accent.copy(alpha = 0.45f),
                modifier = Modifier
                    .fillMaxWidth()
                    .testTag(PaywallTags.UNCONFIRMED_BANNER),
            ) {
                Column(
                    modifier = Modifier
                        .fillMaxWidth()
                        .padding(Space.l),
                    verticalArrangement = Arrangement.spacedBy(Space.xs),
                ) {
                    Text(
                        text = headline,
                        style = AmuxTheme.typography.bodyEmphasis,
                        color = colors.ink,
                    )
                    Text(
                        text = detail,
                        style = AmuxTheme.typography.detail,
                        color = colors.inkMuted,
                    )
                }
            }
        }
        is PaywallPhaseUi.Failed -> {
            AmuxPanel(
                borderColor = colors.removed.copy(alpha = 0.55f),
                modifier = Modifier
                    .fillMaxWidth()
                    .testTag(PaywallTags.FAILED_BANNER),
            ) {
                Column(
                    modifier = Modifier
                        .fillMaxWidth()
                        .padding(Space.l),
                    verticalArrangement = Arrangement.spacedBy(Space.xs),
                ) {
                    Text(
                        text = stringResource(R.string.paywall_failed_title),
                        style = AmuxTheme.typography.bodyEmphasis,
                        color = colors.removed,
                    )
                    Text(
                        text = phase.reason,
                        style = AmuxTheme.typography.detail,
                        color = colors.inkMuted,
                    )
                }
            }
        }
        PaywallPhaseUi.Ready,
        PaywallPhaseUi.Buying,
        PaywallPhaseUi.Confirming,
        is PaywallPhaseUi.StoreUnavailable,
        is PaywallPhaseUi.Entitled,
        -> Unit
    }
}

@Composable
private fun PlanSelector(
    plans: List<PaywallPlanUi>,
    chosen: PaywallPlanPeriod,
    enabled: Boolean,
    largeText: Boolean,
    onChoose: (PaywallPlanPeriod) -> Unit,
) {
    Column(
        modifier = Modifier.fillMaxWidth(),
        verticalArrangement = Arrangement.spacedBy(Space.s),
    ) {
        plans.forEach { plan ->
            val isSelected = plan.period == chosen
            val tag = when (plan.period) {
                PaywallPlanPeriod.Monthly -> PaywallTags.PLAN_MONTHLY
                PaywallPlanPeriod.Yearly -> PaywallTags.PLAN_YEARLY
            }
            PlanCard(
                plan = plan,
                isSelected = isSelected,
                enabled = enabled,
                largeText = largeText,
                onClick = { onChoose(plan.period) },
                modifier = Modifier.testTag(tag),
            )
        }
    }
}

@Composable
private fun PlanCard(
    plan: PaywallPlanUi,
    isSelected: Boolean,
    enabled: Boolean,
    largeText: Boolean,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val colors = AmuxTheme.colors
    val title = when (plan.period) {
        PaywallPlanPeriod.Monthly -> stringResource(R.string.paywall_period_monthly)
        PaywallPlanPeriod.Yearly -> stringResource(R.string.paywall_period_yearly)
    }
    val shape = RoundedCornerShape(AmuxTheme.metrics.cardRadius)

    Surface(
        shape = shape,
        color = colors.raised,
        contentColor = colors.ink,
        // The radio alone shows the choice, as on iOS; a ring as well made
        // three bright marks for one selection in dark mode.
        border = BorderStroke(AmuxTheme.metrics.hairline, colors.hairline),
        modifier = modifier
            .fillMaxWidth()
            .heightIn(min = ThumbTarget.MinSize)
            .clickable(enabled = enabled, role = Role.RadioButton, onClick = onClick)
            .semantics { selected = isSelected },
    ) {
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = Space.l, vertical = Space.m),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(Space.m),
        ) {
            RadioButton(
                selected = isSelected,
                onClick = null,
                enabled = enabled,
                colors = RadioButtonDefaults.colors(
                    selectedColor = colors.ink,
                    unselectedColor = colors.inkFaint,
                    disabledSelectedColor = colors.inkMuted,
                    disabledUnselectedColor = colors.hairline,
                ),
            )
            if (largeText) {
                Column(
                    modifier = Modifier.weight(1f),
                    verticalArrangement = Arrangement.spacedBy(Space.xxs),
                ) {
                    Text(
                        text = title,
                        style = AmuxTheme.typography.bodyEmphasis,
                        color = colors.ink,
                    )
                    Text(
                        text = plan.price,
                        style = AmuxTheme.typography.body,
                        color = colors.ink,
                    )
                    if (!plan.saving.isNullOrBlank()) {
                        Text(
                            text = plan.saving,
                            style = AmuxTheme.typography.caption,
                            color = colors.accent,
                        )
                    }
                }
            } else {
                Text(
                    text = title,
                    style = AmuxTheme.typography.bodyEmphasis,
                    color = colors.ink,
                    modifier = Modifier.weight(1f),
                )
                Column(
                    horizontalAlignment = Alignment.End,
                    verticalArrangement = Arrangement.spacedBy(Space.xxs),
                ) {
                    Text(
                        text = plan.price,
                        style = AmuxTheme.typography.bodyEmphasis,
                        color = colors.ink,
                    )
                    if (!plan.saving.isNullOrBlank()) {
                        Text(
                            text = plan.saving,
                            style = AmuxTheme.typography.caption,
                            color = colors.accent,
                        )
                    }
                }
            }
        }
    }
}

@Composable
private fun PaywallTerms(
    privacyUrl: String,
    onAction: (PaywallAction) -> Unit,
) {
    val colors = AmuxTheme.colors
    Column(verticalArrangement = Arrangement.spacedBy(Space.xs)) {
        Text(
            text = stringResource(R.string.paywall_terms),
            style = AmuxTheme.typography.caption,
            color = colors.inkFaint,
        )
        // A link on the text column, not inset by a button's padding. Only
        // the privacy policy: amux has no terms of its own to link, and
        // Google Play's are Google's agreement with the person, not amux's.
        LegalLink(
            label = stringResource(R.string.paywall_privacy_policy),
            onClick = { onAction(PaywallAction.OpenLegalUrl(privacyUrl)) },
            modifier = Modifier.testTag(PaywallTags.PRIVACY_LINK),
        )
    }
}

@Composable
private fun LegalLink(label: String, onClick: () -> Unit, modifier: Modifier = Modifier) {
    Box(
        modifier = modifier
            .spillTarget()
            .clickable(role = Role.Button, onClick = onClick),
        contentAlignment = Alignment.Center,
    ) {
        Text(
            text = label,
            style = AmuxTheme.typography.detail,
            color = AmuxTheme.colors.accent,
        )
    }
}

@Composable
private fun PaywallBottomBar(
    state: PaywallScreenState,
    onAction: (PaywallAction) -> Unit,
) {
    val colors = AmuxTheme.colors
    Surface(
        color = colors.ground,
        contentColor = colors.ink,
    ) {
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .windowInsetsPadding(WindowInsets.navigationBars),
        ) {
            AmuxHairline(color = colors.hairline.copy(alpha = 0.6f))
            Column(
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(horizontal = Space.margin, vertical = Space.m),
                verticalArrangement = Arrangement.spacedBy(Space.s),
                horizontalAlignment = Alignment.CenterHorizontally,
            ) {
                when (val phase = state.phase) {
                    is PaywallPhaseUi.Entitled -> {
                        if (phase.source == PaywallGrantSource.PlayStore) {
                            AmuxActionButton(
                                label = stringResource(R.string.paywall_manage_play),
                                kind = ActionButtonKind.Primary,
                                fillWidth = true,
                                onClick = { onAction(PaywallAction.ManageInStore) },
                                modifier = Modifier.testTag(PaywallTags.MANAGE_PLAY_BUTTON),
                            )
                        }
                        if (phase.source != PaywallGrantSource.Granted) {
                            AmuxActionButton(
                                label = stringResource(R.string.paywall_manage_web),
                                kind = if (phase.source == PaywallGrantSource.PlayStore) {
                                    ActionButtonKind.Outline
                                } else {
                                    ActionButtonKind.Primary
                                },
                                fillWidth = true,
                                onClick = { onAction(PaywallAction.ManageOnWeb(state.manageUrl)) },
                                modifier = Modifier.testTag(PaywallTags.MANAGE_WEB_BUTTON),
                            )
                        }
                    }

                    is PaywallPhaseUi.StoreUnavailable -> {
                        AmuxActionButton(
                            label = stringResource(R.string.paywall_manage_web),
                            kind = ActionButtonKind.Primary,
                            fillWidth = true,
                            onClick = { onAction(PaywallAction.ManageOnWeb(state.manageUrl)) },
                            modifier = Modifier.testTag(PaywallTags.MANAGE_WEB_BUTTON),
                        )
                        AmuxActionButton(
                            label = stringResource(R.string.action_retry),
                            kind = ActionButtonKind.Outline,
                            fillWidth = true,
                            onClick = { onAction(PaywallAction.Retry) },
                            modifier = Modifier.testTag(PaywallTags.RETRY_BUTTON),
                        )
                    }

                    PaywallPhaseUi.Buying,
                    PaywallPhaseUi.Confirming,
                    PaywallPhaseUi.AwaitingApproval,
                    -> {
                        val waitingText = when (phase) {
                            PaywallPhaseUi.Buying -> stringResource(R.string.paywall_waiting_store)
                            PaywallPhaseUi.Confirming -> stringResource(R.string.paywall_confirming)
                            PaywallPhaseUi.AwaitingApproval -> stringResource(R.string.paywall_waiting_approval)
                        }
                        Row(
                            modifier = Modifier
                                .fillMaxWidth()
                                .heightIn(min = ThumbTarget.MinSize)
                                .testTag(PaywallTags.WAITING_LINE),
                            verticalAlignment = Alignment.CenterVertically,
                            horizontalArrangement = Arrangement.Center,
                        ) {
                            AmuxStateMark(state = RowState.Working, size = 10.dp)
                            Spacer(modifier = Modifier.padding(horizontal = Space.xs))
                            Text(
                                text = waitingText,
                                style = AmuxTheme.typography.bodyEmphasis,
                                color = colors.inkMuted,
                            )
                        }
                        RestorePurchasesButton(
                            enabled = !state.working,
                            onClick = { onAction(PaywallAction.RestorePurchases) },
                        )
                    }

                    is PaywallPhaseUi.Unconfirmed -> {
                        AmuxActionButton(
                            label = stringResource(R.string.action_retry),
                            kind = ActionButtonKind.Primary,
                            fillWidth = true,
                            onClick = { onAction(PaywallAction.Retry) },
                            modifier = Modifier.testTag(PaywallTags.RETRY_BUTTON),
                        )
                        RestorePurchasesButton(
                            enabled = true,
                            onClick = { onAction(PaywallAction.RestorePurchases) },
                        )
                    }

                    is PaywallPhaseUi.Failed -> {
                        if (state.plans.isNotEmpty()) {
                            AmuxActionButton(
                                label = subscribeLabel(state.selectedPlan),
                                kind = ActionButtonKind.Primary,
                                fillWidth = true,
                                onClick = { onAction(PaywallAction.Subscribe) },
                                modifier = Modifier.testTag(PaywallTags.SUBSCRIBE_BUTTON),
                            )
                            AmuxActionButton(
                                label = stringResource(R.string.action_retry),
                                kind = ActionButtonKind.Outline,
                                fillWidth = true,
                                onClick = { onAction(PaywallAction.Retry) },
                                modifier = Modifier.testTag(PaywallTags.RETRY_BUTTON),
                            )
                            RestorePurchasesButton(
                                enabled = true,
                                onClick = { onAction(PaywallAction.RestorePurchases) },
                            )
                        } else {
                            AmuxActionButton(
                                label = stringResource(R.string.action_retry),
                                kind = ActionButtonKind.Primary,
                                fillWidth = true,
                                onClick = { onAction(PaywallAction.Retry) },
                                modifier = Modifier.testTag(PaywallTags.RETRY_BUTTON),
                            )
                            AmuxActionButton(
                                label = stringResource(R.string.paywall_manage_web),
                                kind = ActionButtonKind.Outline,
                                fillWidth = true,
                                onClick = { onAction(PaywallAction.ManageOnWeb(state.manageUrl)) },
                                modifier = Modifier.testTag(PaywallTags.MANAGE_WEB_BUTTON),
                            )
                        }
                    }

                    PaywallPhaseUi.Ready -> {
                        if (state.plans.isEmpty()) {
                            AmuxActionButton(
                                label = stringResource(R.string.paywall_manage_web),
                                kind = ActionButtonKind.Primary,
                                fillWidth = true,
                                onClick = { onAction(PaywallAction.ManageOnWeb(state.manageUrl)) },
                                modifier = Modifier.testTag(PaywallTags.MANAGE_WEB_BUTTON),
                            )
                        } else {
                            AmuxActionButton(
                                label = subscribeLabel(state.selectedPlan),
                                kind = ActionButtonKind.Primary,
                                fillWidth = true,
                                onClick = { onAction(PaywallAction.Subscribe) },
                                modifier = Modifier.testTag(PaywallTags.SUBSCRIBE_BUTTON),
                            )
                            RestorePurchasesButton(
                                enabled = true,
                                onClick = { onAction(PaywallAction.RestorePurchases) },
                            )
                        }
                    }
                }
            }
        }
    }
}

@Composable
private fun subscribeLabel(plan: PaywallPlanUi?): String {
    if (plan == null) return stringResource(R.string.paywall_continue)
    val spelled = when (plan.period) {
        PaywallPlanPeriod.Monthly -> stringResource(R.string.paywall_period_monthly_spelled)
        PaywallPlanPeriod.Yearly -> stringResource(R.string.paywall_period_yearly_spelled)
    }
    return stringResource(R.string.paywall_subscribe_price, plan.price, spelled)
}

@Composable
private fun RestorePurchasesButton(
    enabled: Boolean,
    onClick: () -> Unit,
) {
    val colors = AmuxTheme.colors
    TextButton(
        onClick = onClick,
        enabled = enabled,
        modifier = Modifier.testTag(PaywallTags.RESTORE_BUTTON),
    ) {
        Text(
            text = stringResource(R.string.paywall_restore),
            style = AmuxTheme.typography.body,
            color = if (enabled) colors.accent else colors.inkFaint,
        )
    }
}

private fun finishSentence(reason: String): String {
    val trimmed = reason.trim()
    if (trimmed.isEmpty()) return trimmed
    val last = trimmed.last()
    return if (last == '.' || last == '?' || last == '!') trimmed else "$trimmed."
}
