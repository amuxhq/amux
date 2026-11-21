package sh.amux.features

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Close
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.Text
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.draw.clip
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.withStyle
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import sh.amux.design.AmuxHairline
import sh.amux.design.AmuxPanel
import sh.amux.design.AmuxTheme
import sh.amux.design.LocalAmuxPhotographed
import sh.amux.design.Space
import sh.amux.design.thumbTarget
import sh.amux.values.HostView
import sh.amux.values.PendingPair
import sh.amux.values.Reach
import sh.amux.values.RouteKind
import sh.amux.words.Elapsed
import sh.amux.words.RowState

/**
 * M3 [ModalBottomSheet] presenting the two-step pairing flow (PIN code entry
 * or QR/App Link confirmation).
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun PairingModalSheet(
    state: PairingSheetState,
    onAction: (PairingAction) -> Unit,
) {
    val colors = AmuxTheme.colors
    val sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true)
    ModalBottomSheet(
        onDismissRequest = { onAction(PairingAction.Cancel) },
        sheetState = sheetState,
        containerColor = colors.ground,
        contentColor = colors.ink,
    ) {
        PairingSheetContent(
            state = state,
            onAction = onAction,
        )
    }
}

/**
 * Pure Compose body of the pairing sheet. Exposed directly so headless
 * Storybook and Robolectric tests can render every pairing phase cleanly.
 */
@Composable
fun PairingSheetContent(
    state: PairingSheetState,
    onAction: (PairingAction) -> Unit,
    modifier: Modifier = Modifier,
) {
    val colors = AmuxTheme.colors
    val headerTitle = pairingHeaderTitle(state)
    val headerSubtitle = pairingHeaderSubtitle(state)

    Column(
        modifier = modifier
            .fillMaxWidth()
            .background(colors.ground)
            .padding(bottom = Space.xl)
            .testTag(TestTags.PAIRING_SHEET),
    ) {
        // Sheet header bar
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = Space.s, vertical = Space.xs),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            val closeLabel = stringResource(R.string.action_close)
            IconButton(
                onClick = { onAction(PairingAction.Cancel) },
                modifier = Modifier
                    .thumbTarget()
                    .testTag(TestTags.PAIRING_CLOSE),
            ) {
                Icon(
                    imageVector = Icons.Outlined.Close,
                    contentDescription = closeLabel,
                    tint = colors.inkMuted,
                )
            }
            Column(modifier = Modifier.weight(1f)) {
                Text(
                    text = headerTitle,
                    style = AmuxTheme.typography.identifier,
                    color = colors.ink,
                    maxLines = 1,
                )
                if (headerSubtitle != null) {
                    Text(
                        text = headerSubtitle,
                        style = AmuxTheme.typography.caption,
                        color = colors.inkMuted,
                        maxLines = 1,
                    )
                }
            }
        }
        AmuxHairline(color = colors.hairline.copy(alpha = 0.6f))

        Box(
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = Space.margin, vertical = Space.l),
        ) {
            when (val phase = state.phase) {
                PairingPhaseUi.Entering -> PinEntryContent(
                    state = state,
                    checking = false,
                    refusedMessage = null,
                    onAction = onAction,
                )
                is PairingPhaseUi.Checking -> if (phase.viaLink) {
                    CheckingInvitationContent()
                } else {
                    PinEntryContent(
                        state = state,
                        checking = true,
                        refusedMessage = null,
                        onAction = onAction,
                    )
                }
                is PairingPhaseUi.Refused -> if (phase.viaLink) {
                    RefusedInvitationContent(
                        message = phase.message,
                        onClose = { onAction(PairingAction.Cancel) },
                    )
                } else {
                    PinEntryContent(
                        state = state,
                        checking = false,
                        refusedMessage = phase.message,
                        onAction = onAction,
                    )
                }
                is PairingPhaseUi.Confirming -> ConfirmPairingContent(
                    pending = phase.pending,
                    platformName = phase.platformName ?: state.machine?.platform?.label,
                    nowMs = state.nowMs,
                    onConfirm = { onAction(PairingAction.Confirm(phase.pending)) },
                    onAbandon = { onAction(PairingAction.Abandon(phase.pending)) },
                )
                is PairingPhaseUi.Trusted -> TrustedContent(name = phase.name)
                PairingPhaseUi.NeedsSubscription -> NeedsSubscriptionContent(
                    hostName = state.machine?.name,
                    onSubscribe = { onAction(PairingAction.Subscribe) },
                    onCancel = { onAction(PairingAction.Cancel) },
                )
                is PairingPhaseUi.NeedsAccount -> NeedsAccountContent(
                    signingIn = phase.signingIn,
                    onSignIn = { onAction(PairingAction.SignIn) },
                    onCancel = { onAction(PairingAction.Cancel) },
                )
            }
        }
    }
}

@Composable
private fun pairingHeaderTitle(state: PairingSheetState): String {
    val hostName = when (val phase = state.phase) {
        is PairingPhaseUi.Confirming -> phase.pending.name
        is PairingPhaseUi.Trusted -> phase.name
        else -> state.machine?.name
    }
    return if (hostName != null) {
        stringResource(R.string.pairing_title_host, hostName)
    } else {
        stringResource(R.string.pairing_title_generic)
    }
}

@Composable
private fun pairingHeaderSubtitle(state: PairingSheetState): String? {
    val phase = state.phase
    if (phase is PairingPhaseUi.Confirming) {
        return routeTitle(phase.pending.via)
    }
    val machine = state.machine ?: return null
    if (machine.candidate && machine.reach != Reach.Online(RouteKind.Relay)) {
        return stringResource(R.string.pairing_route_direct)
    }
    return when (val reach = machine.reach) {
        is Reach.Online -> routeTitle(reach.value)
        Reach.Away, is Reach.Offline -> null
    }
}

@Composable
private fun routeTitle(route: RouteKind): String? = when (route) {
    RouteKind.Direct -> stringResource(R.string.pairing_route_direct)
    RouteKind.Relay -> stringResource(R.string.pairing_route_relay)
    RouteKind.Ssh -> stringResource(R.string.pairing_route_ssh)
    RouteKind.Unspecified -> null
}

@Composable
private fun PinEntryContent(
    state: PairingSheetState,
    checking: Boolean,
    refusedMessage: String?,
    onAction: (PairingAction) -> Unit,
) {
    val colors = AmuxTheme.colors
    val typography = AmuxTheme.typography
    val focusRequester = remember { FocusRequester() }
    val photographed = LocalAmuxPhotographed.current

    LaunchedEffect(state.takingDigits, photographed) {
        if (state.takingDigits && !photographed) {
            runCatching { focusRequester.requestFocus() }
        }
    }

    val machine = state.machine
    val headline = if (machine != null && machine.hasOpenWindow) {
        stringResource(R.string.pairing_headline_host, machine.name)
    } else {
        stringResource(R.string.pairing_headline_code)
    }

    val instruction = if (machine != null && machine.hasOpenWindow) {
        buildAnnotatedString {
            append(stringResource(R.string.pairing_instruction_open, machine.name))
        }
    } else {
        val prefix = stringResource(R.string.pairing_instruction_command_prefix)
        val cmd = stringResource(R.string.pairing_instruction_command_literal)
        val suffix = if (machine != null) {
            stringResource(R.string.pairing_instruction_command_host, machine.name)
        } else {
            stringResource(R.string.pairing_instruction_command_generic)
        }
        buildAnnotatedString {
            append(prefix)
            withStyle(typography.monoSmall.toSpanStyle().copy(color = colors.ink)) {
                append(cmd)
            }
            append(suffix)
        }
    }

    Column(
        modifier = Modifier.fillMaxWidth(),
        verticalArrangement = Arrangement.spacedBy(Space.s),
    ) {
        Text(
            text = headline,
            style = typography.screenTitle,
            color = colors.ink,
            modifier = Modifier.testTag(TestTags.PAIRING_TITLE),
        )
        Text(
            text = instruction,
            style = typography.body,
            color = colors.inkMuted,
        )

        Spacer(modifier = Modifier.height(Space.m))

        // Six digit boxes backed by a real number-pad BasicTextField that
        // accepts pasting "418 207" and automatically submits on the 6th digit.
        val codeLabel = stringResource(R.string.pairing_code_label)
        Box(
            modifier = Modifier
                .fillMaxWidth()
                .clickable { runCatching { focusRequester.requestFocus() } },
        ) {
            Row(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.spacedBy(Space.s),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                for (index in 0 until 6) {
                    if (index == 3) {
                        Spacer(modifier = Modifier.width(Space.s))
                    }
                    DigitBox(
                        digit = state.digits.getOrNull(index)?.toString().orEmpty(),
                        active = index == state.digits.length && state.takingDigits,
                        modifier = Modifier.weight(1f),
                    )
                }
            }
            BasicTextField(
                value = state.digits,
                onValueChange = { typed ->
                    if (state.takingDigits) {
                        val cleaned = typed.filter { it.isDigit() }.take(6)
                        onAction(PairingAction.Digits(cleaned))
                    }
                },
                enabled = state.takingDigits,
                singleLine = true,
                keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number),
                cursorBrush = SolidColor(Color.Unspecified),
                modifier = Modifier
                    .matchParentSize()
                    .alpha(0.01f)
                    .focusRequester(focusRequester)
                    .semantics { contentDescription = codeLabel }
                    .testTag(TestTags.PAIRING_DIGITS),
            )
        }

        Spacer(modifier = Modifier.height(Space.xs))

        when {
            checking -> {
                Row(
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(Space.s),
                    modifier = Modifier.testTag(TestTags.PAIRING_CHECKING),
                ) {
                    AmuxStateMark(state = RowState.Working, size = 10.dp)
                    Text(
                        text = stringResource(R.string.pairing_checking),
                        style = typography.detail,
                        color = colors.inkMuted,
                    )
                }
            }
            state.phase is PairingPhaseUi.Refused -> {
                val fallback = if (machine != null) {
                    stringResource(R.string.pairing_refused_host, machine.name)
                } else {
                    stringResource(R.string.pairing_refused_generic)
                }
                Text(
                    text = refusedMessage ?: fallback,
                    style = typography.detail,
                    color = colors.removed,
                    modifier = Modifier.testTag(TestTags.PAIRING_REFUSED),
                )
            }
            else -> {
                val expiryText = if (machine != null) {
                    stringResource(R.string.pairing_expiry_host, state.offerWindow, machine.name)
                } else {
                    stringResource(R.string.pairing_expiry_generic, state.offerWindow)
                }
                Text(
                    text = expiryText,
                    style = typography.detail,
                    color = colors.inkFaint,
                )
            }
        }
    }
}

@Composable
private fun DigitBox(
    digit: String,
    active: Boolean,
    modifier: Modifier = Modifier,
) {
    val colors = AmuxTheme.colors
    val shape = RoundedCornerShape(AmuxTheme.metrics.controlRadius)
    Box(
        modifier = modifier
            .height(58.dp)
            .clip(shape)
            .background(colors.raised)
            .border(
                width = if (active) 1.5.dp else AmuxTheme.metrics.hairline,
                color = if (active) colors.accent else colors.hairline,
                shape = shape,
            ),
        contentAlignment = Alignment.Center,
    ) {
        Text(
            text = digit,
            style = AmuxTheme.typography.mono.copy(
                fontSize = 26.sp,
                lineHeight = 32.sp,
                fontWeight = FontWeight.Medium,
            ),
            color = colors.ink,
            maxLines = 1,
        )
    }
}

@Composable
private fun ConfirmPairingContent(
    pending: PendingPair,
    platformName: String?,
    nowMs: Long,
    onConfirm: () -> Unit,
    onAbandon: () -> Unit,
) {
    val colors = AmuxTheme.colors
    val typography = AmuxTheme.typography
    val routePart = when (pending.via) {
        RouteKind.Direct -> stringResource(R.string.pairing_confirm_route_direct)
        RouteKind.Relay -> stringResource(R.string.pairing_confirm_route_relay)
        RouteKind.Ssh -> stringResource(R.string.pairing_confirm_route_ssh)
        RouteKind.Unspecified -> null
    }
    val about = listOfNotNull(platformName, routePart).joinToString(" · ")
    val leftSeconds = (pending.expiresAtMs - nowMs) / 1_000.0
    val expiry = if (leftSeconds > 0.0) {
        stringResource(R.string.pairing_confirm_expires_in, Elapsed.spelled(leftSeconds))
    } else {
        stringResource(R.string.pairing_confirm_expires_shortly)
    }
    val groupedFingerprint = remember(pending.fingerprint) {
        FingerprintFormat.grouped(pending.fingerprint)
    }

    Column(
        modifier = Modifier.fillMaxWidth(),
        verticalArrangement = Arrangement.spacedBy(Space.m),
    ) {
        Column(verticalArrangement = Arrangement.spacedBy(Space.xs)) {
            Text(
                text = stringResource(R.string.pairing_headline_host, pending.name),
                style = typography.screenTitle,
                color = colors.ink,
                modifier = Modifier.testTag(TestTags.PAIRING_TITLE),
            )
            if (about.isNotEmpty()) {
                Text(
                    text = about,
                    style = typography.body,
                    color = colors.inkMuted,
                )
            }
        }

        Text(
            text = stringResource(R.string.pairing_confirm_explain, expiry),
            style = typography.detail,
            color = colors.inkMuted,
        )

        AmuxPanel(modifier = Modifier.fillMaxWidth()) {
            Column(
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(Space.m),
                verticalArrangement = Arrangement.spacedBy(Space.xs),
            ) {
                Text(
                    text = stringResource(R.string.pairing_fingerprint_label),
                    style = typography.detail,
                    color = colors.inkMuted,
                )
                SelectionContainer {
                    Text(
                        text = groupedFingerprint,
                        style = typography.monoSmall,
                        color = colors.ink,
                        modifier = Modifier.testTag(TestTags.PAIRING_FINGERPRINT),
                    )
                }
            }
        }

        Spacer(modifier = Modifier.height(Space.xs))

        Column(verticalArrangement = Arrangement.spacedBy(Space.s)) {
            AmuxActionButton(
                label = stringResource(R.string.pairing_button_pair),
                kind = ActionButtonKind.Primary,
                fillWidth = true,
                onClick = onConfirm,
                modifier = Modifier.testTag(TestTags.PAIRING_CONFIRM),
            )
            AmuxActionButton(
                label = stringResource(R.string.pairing_button_not_now),
                kind = ActionButtonKind.Outline,
                fillWidth = true,
                onClick = onAbandon,
                modifier = Modifier.testTag(TestTags.PAIRING_CANCEL),
            )
        }
    }
}

@Composable
private fun CheckingInvitationContent() {
    val colors = AmuxTheme.colors
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .testTag(TestTags.PAIRING_CHECKING),
        verticalArrangement = Arrangement.spacedBy(Space.s),
    ) {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(Space.m),
        ) {
            AmuxStateMark(state = RowState.Working, size = 12.dp)
            Text(
                text = stringResource(R.string.pairing_checking_title),
                style = AmuxTheme.typography.screenTitle,
                color = colors.ink,
            )
        }
        Text(
            text = stringResource(R.string.pairing_checking_detail),
            style = AmuxTheme.typography.body,
            color = colors.inkMuted,
        )
    }
}

@Composable
private fun TrustedContent(name: String) {
    val colors = AmuxTheme.colors
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .testTag(TestTags.PAIRING_TRUSTED),
        verticalArrangement = Arrangement.spacedBy(Space.s),
    ) {
        Text(
            text = stringResource(R.string.pairing_trusted_title, name),
            style = AmuxTheme.typography.screenTitle,
            color = colors.ink,
        )
        Text(
            text = stringResource(R.string.pairing_trusted_detail),
            style = AmuxTheme.typography.body,
            color = colors.inkMuted,
        )
    }
}

@Composable
private fun RefusedInvitationContent(
    message: String?,
    onClose: () -> Unit,
) {
    val colors = AmuxTheme.colors
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .testTag(TestTags.PAIRING_REFUSED),
        verticalArrangement = Arrangement.spacedBy(Space.m),
    ) {
        Column(verticalArrangement = Arrangement.spacedBy(Space.xs)) {
            Text(
                text = stringResource(
                    if (message == null) {
                        R.string.pairing_refused_invitation_title
                    } else {
                        R.string.pairing_refused_not_paired_title
                    },
                ),
                style = AmuxTheme.typography.screenTitle,
                color = colors.ink,
            )
            Text(
                text = message ?: stringResource(R.string.pairing_refused_invitation_detail),
                style = AmuxTheme.typography.body,
                color = colors.inkMuted,
            )
        }
        AmuxActionButton(
            label = stringResource(R.string.action_close),
            kind = ActionButtonKind.Outline,
            fillWidth = true,
            onClick = onClose,
            modifier = Modifier.testTag(TestTags.PAIRING_CANCEL),
        )
    }
}

@Composable
private fun NeedsSubscriptionContent(
    hostName: String?,
    onSubscribe: () -> Unit,
    onCancel: () -> Unit,
) {
    val colors = AmuxTheme.colors
    Column(
        modifier = Modifier.fillMaxWidth(),
        verticalArrangement = Arrangement.spacedBy(Space.m),
    ) {
        Column(verticalArrangement = Arrangement.spacedBy(Space.xs)) {
            Text(
                text = stringResource(R.string.pairing_subscribe_title),
                style = AmuxTheme.typography.screenTitle,
                color = colors.ink,
            )
            Text(
                text = if (hostName != null) {
                    stringResource(R.string.pairing_subscribe_detail_host, hostName)
                } else {
                    stringResource(R.string.pairing_subscribe_detail_generic)
                },
                style = AmuxTheme.typography.body,
                color = colors.inkMuted,
            )
        }
        Column(verticalArrangement = Arrangement.spacedBy(Space.s)) {
            AmuxActionButton(
                label = stringResource(R.string.callout_subscribe),
                kind = ActionButtonKind.Primary,
                fillWidth = true,
                onClick = onSubscribe,
                modifier = Modifier.testTag(TestTags.PAIRING_SUBSCRIBE),
            )
            AmuxActionButton(
                label = stringResource(R.string.pairing_button_not_now),
                kind = ActionButtonKind.Outline,
                fillWidth = true,
                onClick = onCancel,
                modifier = Modifier.testTag(TestTags.PAIRING_CANCEL),
            )
        }
    }
}

@Composable
private fun NeedsAccountContent(
    signingIn: Boolean,
    onSignIn: () -> Unit,
    onCancel: () -> Unit,
) {
    val colors = AmuxTheme.colors
    Column(
        modifier = Modifier.fillMaxWidth(),
        verticalArrangement = Arrangement.spacedBy(Space.m),
    ) {
        Column(verticalArrangement = Arrangement.spacedBy(Space.xs)) {
            Text(
                text = stringResource(R.string.pairing_needs_account_title),
                style = AmuxTheme.typography.screenTitle,
                color = colors.ink,
            )
            Text(
                text = stringResource(R.string.pairing_needs_account_detail),
                style = AmuxTheme.typography.body,
                color = colors.inkMuted,
            )
        }
        Column(verticalArrangement = Arrangement.spacedBy(Space.s)) {
            if (signingIn) {
                Row(
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(Space.s),
                    modifier = Modifier
                        .thumbTarget()
                        .testTag(TestTags.PAIRING_SIGN_IN),
                ) {
                    AmuxStateMark(state = RowState.Working, size = 10.dp)
                    Text(
                        text = stringResource(R.string.callout_signing_in),
                        style = AmuxTheme.typography.bodyEmphasis,
                        color = colors.inkMuted,
                    )
                }
            } else {
                AmuxActionButton(
                    label = stringResource(R.string.callout_sign_in),
                    kind = ActionButtonKind.Primary,
                    fillWidth = true,
                    onClick = onSignIn,
                    modifier = Modifier.testTag(TestTags.PAIRING_SIGN_IN),
                )
            }
            AmuxActionButton(
                label = stringResource(R.string.pairing_button_not_now),
                kind = ActionButtonKind.Outline,
                fillWidth = true,
                onClick = onCancel,
                modifier = Modifier.testTag(TestTags.PAIRING_CANCEL),
            )
            Text(
                text = stringResource(R.string.pairing_needs_account_kept),
                style = AmuxTheme.typography.detail,
                color = colors.inkMuted,
            )
        }
    }
}
