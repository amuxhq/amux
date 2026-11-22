package sh.amux.features

import sh.amux.design.backdrop
import sh.amux.design.ThumbTarget
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.animation.animateContentSize
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.input.ImeAction
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
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.withStyle
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
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
    AmuxSheet(
        onDismissRequest = { onAction(PairingAction.Cancel) },
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

    val (sheetTag, sheetReading) = pairingSheetReading(state)
    Column(
        modifier = modifier
            .fillMaxWidth()
            .background(AmuxTheme.backdrop)
            .padding(bottom = Space.xl)
            .semantics { reportedValue = sheetReading }
            .testTag(sheetTag),
    ) {
        AmuxSheetHeader(
            title = headerTitle,
            subtitle = headerSubtitle,
            onClose = { onAction(PairingAction.Cancel) },
            closeModifier = Modifier.testTag(TestTags.PAIRING_CLOSE),
        )

        // Scrolls, so at the largest text or in landscape the buttons that
        // decide the pairing stay reachable under the steps above them.
        Box(
            modifier = Modifier
                .fillMaxWidth()
                .verticalScroll(rememberScrollState())
                .animateContentSize()
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
                    route = confirmingRoute(phase.pending, state.machine),
                    platformName = phase.platformName ?: state.machine?.platform?.label,
                    nowMs = state.nowMs,
                    onConfirm = { onAction(PairingAction.Confirm(phase.pending)) },
                    onAbandon = { onAction(PairingAction.Abandon(phase.pending)) },
                )
                is PairingPhaseUi.Trusted -> TrustedContent(name = phase.name)
                PairingPhaseUi.NeedsSubscription -> PinEntryContent(
                    state = state,
                    checking = false,
                    refusedMessage = null,
                    onAction = onAction,
                    subscribe = true,
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

/**
 * The sheet's name and reading as the other clients report them, which show
 * the code and the device that answered as two pages: the code typed so far
 * while it is being entered or checked, and once the device has answered,
 * its name or how the pairing ended.
 */
internal fun pairingSheetReading(state: PairingSheetState): Pair<String, String> =
    when (val phase = state.phase) {
        PairingPhaseUi.Entering, PairingPhaseUi.NeedsSubscription -> TestTags.PIN_SHEET to state.digits
        is PairingPhaseUi.Checking ->
            if (phase.viaLink) TestTags.PAIR_CONFIRM_SHEET to "checking" else TestTags.PIN_SHEET to state.digits
        is PairingPhaseUi.Refused ->
            if (phase.viaLink) TestTags.PAIR_CONFIRM_SHEET to "refused" else TestTags.PIN_SHEET to state.digits
        is PairingPhaseUi.Confirming -> TestTags.PAIR_CONFIRM_SHEET to phase.pending.name
        is PairingPhaseUi.Trusted -> TestTags.PAIR_CONFIRM_SHEET to "trusted ${phase.name}"
        is PairingPhaseUi.NeedsAccount -> TestTags.PAIR_NEEDS_ACCOUNT_SHEET to ""
    }

@Composable
private fun pairingHeaderTitle(state: PairingSheetState): String {
    // An invitation for a device this phone cannot see names nothing it
    // could pair yet.
    if (state.phase is PairingPhaseUi.NeedsAccount) return stringResource(R.string.pairing_title_generic)
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
private fun pairingHeaderSubtitle(state: PairingSheetState): String? =
    pairingRoute(state)?.let { routeTitle(it) }

@Composable
private fun routeTitle(route: PairingRoute): String = when (route) {
    PairingRoute.Local -> stringResource(R.string.pairing_route_local)
    PairingRoute.Direct -> stringResource(R.string.pairing_route_direct)
    PairingRoute.Relayed -> stringResource(R.string.pairing_route_relay)
    PairingRoute.Ssh -> stringResource(R.string.pairing_route_ssh)
}

/** How this phone would reach the machine it is pairing with. */
internal enum class PairingRoute { Local, Direct, Relayed, Ssh }

/**
 * A candidate discovery found on this network, the only one that is Local:
 * not one only the relay reaches, nor one a paired device announced, which
 * has no address here.
 */
internal fun HostView.foundHere(): Boolean =
    candidate && !relayOnly && reach != Reach.Online(RouteKind.Relay) && addrs.isNotEmpty()

/**
 * The route under the machine's name in the sheet's header. Said because it
 * is the difference between a code that works with no account at all and
 * one carried across the relay: two machines of one name on two routes are
 * otherwise one name.
 */
internal fun pairingRoute(state: PairingSheetState): PairingRoute? {
    val phase = state.phase
    if (phase is PairingPhaseUi.Confirming) return confirmingRoute(phase.pending, state.machine)
    // An invitation this phone cannot take up has no route.
    if (phase is PairingPhaseUi.NeedsAccount) return null
    val machine = state.machine ?: return null
    // One only the relay reaches, or one the code reached across the relay.
    if (machine.relayOnly || phase == PairingPhaseUi.NeedsSubscription) return PairingRoute.Relayed
    if (machine.candidate) {
        if (machine.foundHere()) return PairingRoute.Local
        // One a paired device announced goes through that device, which is
        // neither. Any other has no address here, so only the relay reaches
        // it, which is also why a signed-out phone needs an account.
        return if (machine.through == null) PairingRoute.Relayed else null
    }
    return when (val reach = machine.reach) {
        is Reach.Online -> when (reach.value) {
            RouteKind.Direct -> if (machine.sameNetwork) PairingRoute.Local else PairingRoute.Direct
            RouteKind.Relay -> PairingRoute.Relayed
            RouteKind.Ssh -> PairingRoute.Ssh
            RouteKind.Unspecified -> null
        }
        Reach.Away, is Reach.Offline -> null
    }
}

/**
 * How the code just entered reached the machine. A direct dial to the
 * machine discovery found here stays Local, as the header said before the
 * code was entered.
 */
internal fun confirmingRoute(pending: PendingPair, machine: HostView?): PairingRoute? = when (pending.via) {
    RouteKind.Direct ->
        if (machine != null && machine.hostId == pending.hostId && machine.foundHere()) {
            PairingRoute.Local
        } else {
            PairingRoute.Direct
        }
    RouteKind.Relay -> PairingRoute.Relayed
    RouteKind.Ssh -> PairingRoute.Ssh
    RouteKind.Unspecified -> null
}

@Composable
private fun PinEntryContent(
    state: PairingSheetState,
    checking: Boolean,
    refusedMessage: String?,
    onAction: (PairingAction) -> Unit,
    subscribe: Boolean = false,
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
        // One sentence with the command in it, so translation keeps its
        // spaces; the command is then set in the code face where it falls.
        val cmd = stringResource(R.string.pairing_instruction_command_literal)
        val sentence = if (machine != null) {
            stringResource(R.string.pairing_instruction_command_host, cmd, machine.name)
        } else {
            stringResource(R.string.pairing_instruction_command_generic, cmd)
        }
        val at = sentence.indexOf(cmd)
        buildAnnotatedString {
            append(sentence)
            if (at >= 0) {
                addStyle(typography.monoSmall.toSpanStyle().copy(color = colors.ink), at, at + cmd.length)
            }
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
            modifier = Modifier
                .semantics { reportedValue = headline }
                .testTag(TestTags.PIN_TITLE),
        )
        Text(
            text = instruction,
            style = typography.body,
            color = colors.inkMuted,
        )

        Spacer(modifier = Modifier.height(Space.xs))

        if (subscribe) {
            // The code was right and the route is missing: no number typed
            // here can change that, so the offer stands in the boxes' place.
            SubscribeCallToAction(
                hostName = machine?.name,
                onSubscribe = { onAction(PairingAction.Subscribe) },
                onCancel = { onAction(PairingAction.Cancel) },
            )
        } else {
            CodeEntry(state = state, checking = checking, refusedMessage = refusedMessage, focusRequester = focusRequester, onAction = onAction)
        }
    }
}

@Composable
private fun CodeEntry(
    state: PairingSheetState,
    checking: Boolean,
    refusedMessage: String?,
    focusRequester: FocusRequester,
    onAction: (PairingAction) -> Unit,
) {
    val colors = AmuxTheme.colors
    val typography = AmuxTheme.typography
    val machine = state.machine
    Column(
        modifier = Modifier.fillMaxWidth(),
        verticalArrangement = Arrangement.spacedBy(Space.l),
    ) {

        // Six digit boxes over a field drawn clear: it takes the code, typed
        // or pasted as "418 207", and submits on the sixth digit; a tap
        // anywhere on the boxes lands on it and brings the keypad back.
        val codeLabel = stringResource(R.string.pairing_code_label)
        val spokenDigits = state.digits.toCharArray().joinToString(" ").ifEmpty {
            stringResource(R.string.pairing_code_empty)
        }
        Box(
            modifier = Modifier
                .fillMaxWidth()
                .heightIn(min = ThumbTarget.MinSize),
        ) {
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .clearAndSetSemantics {},
                horizontalArrangement = Arrangement.spacedBy(Space.s),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                for (index in 0 until 6) {
                    if (index == 3) {
                        // The two threes read as groups, as a code is read out.
                        Spacer(modifier = Modifier.width(Space.l))
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
                        val cleaned = buildString(6) {
                            for (ch in typed) {
                                val digit = Character.digit(ch, 10)
                                if (digit in 0..9) {
                                    append(('0'.code + digit).toChar())
                                    if (length == 6) break
                                }
                            }
                        }
                        onAction(PairingAction.Digits(cleaned))
                    }
                },
                enabled = state.takingDigits,
                singleLine = true,
                // The PIN pad: digits only, no punctuation keys. Nothing
                // hides the digits, which stay visible in the boxes.
                keyboardOptions = KeyboardOptions(
                    keyboardType = KeyboardType.NumberPassword,
                    imeAction = ImeAction.Done,
                    autoCorrectEnabled = false,
                ),
                // Clear text and caret: the boxes draw the code, and nothing
                // depends on a near-invisible alpha staying invisible.
                textStyle = TextStyle(color = Color.Transparent),
                cursorBrush = SolidColor(Color.Transparent),
                modifier = Modifier
                    .matchParentSize()
                    .focusRequester(focusRequester)
                    .semantics {
                        contentDescription = codeLabel
                        stateDescription = spokenDigits
                    }
                    .testTag(TestTags.PIN_CODE),
            )
        }

        when {
            checking -> {
                Row(
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(Space.s),
                    modifier = Modifier.testTag(TestTags.PIN_CHECKING),
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
                    modifier = Modifier.testTag(TestTags.PIN_REFUSED),
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
                    modifier = Modifier
                        .semantics { reportedValue = state.offerWindow }
                        .testTag(TestTags.PIN_EXPIRY),
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
    route: PairingRoute?,
    platformName: String?,
    nowMs: Long,
    onConfirm: () -> Unit,
    onAbandon: () -> Unit,
) {
    val colors = AmuxTheme.colors
    val typography = AmuxTheme.typography
    // The same word as the header above it.
    val routePart = when (route) {
        PairingRoute.Local -> stringResource(R.string.pairing_confirm_route_local)
        PairingRoute.Direct -> stringResource(R.string.pairing_confirm_route_direct)
        PairingRoute.Relayed -> stringResource(R.string.pairing_confirm_route_relay)
        PairingRoute.Ssh -> stringResource(R.string.pairing_confirm_route_ssh)
        null -> null
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
    val headline = stringResource(R.string.pairing_headline_host, pending.name)
    val fingerprintLabel = stringResource(R.string.pairing_fingerprint_label)

    Column(
        modifier = Modifier.fillMaxWidth(),
        verticalArrangement = Arrangement.spacedBy(Space.m),
    ) {
        // The device by its own name, read whether or not there is a line
        // about it to show: it is what the person agrees to.
        Column(
            modifier = Modifier
                .semantics {
                    reportedLabel = about.ifEmpty { pending.name }
                    reportedValue = pending.name
                }
                .testTag(TestTags.PAIR_CONFIRM_NAME),
            verticalArrangement = Arrangement.spacedBy(Space.xs),
        ) {
            Text(
                text = headline,
                style = typography.screenTitle,
                color = colors.ink,
                modifier = Modifier
                    .semantics { reportedValue = headline }
                    .testTag(TestTags.PAIR_CONFIRM_TITLE),
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
            modifier = Modifier
                .semantics { reportedValue = expiry }
                .testTag(TestTags.PAIR_CONFIRM_EXPIRY),
        )

        // The key, whole, for anybody who wants to check it against the
        // device's; quiet, since most people pair without reading it.
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .padding(top = Space.s, bottom = Space.xs)
                .semantics(mergeDescendants = true) {
                    reportedLabel = fingerprintLabel
                    reportedValue = groupedFingerprint
                }
                .testTag(TestTags.PAIR_CONFIRM_FINGERPRINT),
            verticalArrangement = Arrangement.spacedBy(Space.xs),
        ) {
            Text(
                text = fingerprintLabel,
                style = typography.detail,
                color = colors.inkMuted,
            )
            SelectionContainer {
                Text(
                    text = groupedFingerprint,
                    style = typography.monoSmall,
                    color = colors.inkFaint,
                )
            }
        }

        Column(verticalArrangement = Arrangement.spacedBy(Space.m)) {
            AmuxActionButton(
                label = stringResource(R.string.pairing_button_pair),
                kind = ActionButtonKind.Primary,
                fillWidth = true,
                onClick = onConfirm,
                modifier = Modifier.testTag(TestTags.PAIR_CONFIRM_TRUST),
            )
            AmuxActionButton(
                label = stringResource(R.string.pairing_button_not_now),
                kind = ActionButtonKind.Outline,
                fillWidth = true,
                onClick = onAbandon,
                modifier = Modifier.testTag(TestTags.PAIR_CONFIRM_ABANDON),
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
            .testTag(TestTags.PAIR_CONFIRM_CHECKING),
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
            .testTag(TestTags.PAIR_CONFIRM_TRUSTED),
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
            .testTag(TestTags.PAIR_CONFIRM_REFUSED),
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
            modifier = Modifier.testTag(TestTags.PAIR_CONFIRM_BACK),
        )
    }
}

/**
 * Why a code that was right cannot pair yet, and the offer that would: the
 * relay sees the device, and only Pro relays the connection to it.
 */
@Composable
private fun SubscribeCallToAction(
    hostName: String?,
    onSubscribe: () -> Unit,
    onCancel: () -> Unit,
) {
    val colors = AmuxTheme.colors
    Column(
        modifier = Modifier.fillMaxWidth(),
        verticalArrangement = Arrangement.spacedBy(Space.m),
    ) {
        Column(verticalArrangement = Arrangement.spacedBy(Space.xxs)) {
            Text(
                text = stringResource(R.string.pairing_subscribe_title),
                style = AmuxTheme.typography.bodyEmphasis,
                color = colors.ink,
            )
            Text(
                text = if (hostName != null) {
                    stringResource(R.string.pairing_subscribe_detail_host, hostName)
                } else {
                    stringResource(R.string.pairing_subscribe_detail_generic)
                },
                style = AmuxTheme.typography.caption,
                color = colors.inkMuted,
            )
        }
        AmuxActionButton(
            label = stringResource(R.string.callout_subscribe),
            kind = ActionButtonKind.Primary,
            fillWidth = true,
            onClick = onSubscribe,
            modifier = Modifier.testTag(TestTags.PIN_SUBSCRIBE),
        )
        AmuxActionButton(
            label = stringResource(R.string.pairing_button_not_now),
            kind = ActionButtonKind.Outline,
            fillWidth = true,
            onClick = onCancel,
            modifier = Modifier.testTag(TestTags.PIN_NOT_NOW),
        )
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
        Column(verticalArrangement = Arrangement.spacedBy(Space.m)) {
            if (signingIn) {
                Row(
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(Space.s),
                    modifier = Modifier
                        .thumbTarget()
                        .testTag(TestTags.PAIR_NEEDS_ACCOUNT_SIGN_IN),
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
                    kind = ActionButtonKind.Accent,
                    fillWidth = true,
                    onClick = onSignIn,
                    modifier = Modifier.testTag(TestTags.PAIR_NEEDS_ACCOUNT_SIGN_IN),
                )
            }
            AmuxActionButton(
                label = stringResource(R.string.pairing_button_not_now),
                kind = ActionButtonKind.Outline,
                fillWidth = true,
                onClick = onCancel,
                modifier = Modifier.testTag(TestTags.PAIR_NEEDS_ACCOUNT_CANCEL),
            )
            Text(
                text = stringResource(R.string.pairing_needs_account_kept),
                style = AmuxTheme.typography.detail,
                color = colors.inkMuted,
            )
        }
    }
}
