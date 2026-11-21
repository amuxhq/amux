package sh.amux.features

import sh.amux.design.spillTarget

import androidx.compose.material.icons.outlined.AccountCircle
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.semantics.customActions
import androidx.compose.ui.semantics.CustomAccessibilityAction
import sh.amux.design.AmuxShapes
import androidx.compose.foundation.layout.width

import androidx.compose.ui.text.font.FontWeight
import sh.amux.design.ThumbTarget
import androidx.compose.foundation.layout.heightIn
import sh.amux.design.IconSize
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.navigationBars
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Add
import androidx.compose.material.icons.outlined.BugReport
import androidx.compose.material.icons.outlined.Check
import androidx.compose.material.icons.outlined.ContentCopy
import androidx.compose.material.icons.outlined.Email
import androidx.compose.material.icons.outlined.FileDownload
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Immutable
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import sh.amux.design.AmuxGround
import sh.amux.design.AmuxPanel
import sh.amux.design.AmuxTheme
import sh.amux.design.Space
import sh.amux.design.thumbTarget
import sh.amux.words.RowState

/** Semantic test tags for [SettingsScreen] and its interactive controls. */
object SettingsTags {
    const val SCREEN: String = TestTags.SCREEN_SETTINGS
    const val BACK: String = TestTags.NAV_BACK
    const val SCROLL: String = "settings.scroll"

    const val SIGNED_OUT_CARD: String = "settings.accounts.signed-out"
    const val SIGN_IN_BUTTON: String = "settings.accounts.sign-in"
    const val ADD_ACCOUNT: String = "settings.accounts.add"
    const val MANAGE_SUBSCRIPTION: String = "settings.account.subscription"
    const val SIGN_OUT: String = "settings.account.sign-out"
    const val REMOVE_ACCOUNT: String = "settings.account.remove"
    const val DELETE_ACCOUNT: String = "settings.account.delete"

    const val APPEARANCE_GROUP: String = "settings.appearance"
    const val APPEARANCE_LIGHT: String = "settings.appearance.light"
    const val APPEARANCE_DARK: String = "settings.appearance.dark"
    const val APPEARANCE_SYSTEM: String = "settings.appearance.system"

    const val NOTIFICATIONS_CARD: String = "settings.notifications"
    const val NOTIFICATIONS_STATUS: String = "settings.notifications.status"
    const val NOTIFICATIONS_ACTION: String = "settings.notifications.action"

    const val SHARE_USAGE_ROW: String = "settings.privacy.share-usage"
    const val SHARE_USAGE_SWITCH: String = "settings.privacy.share-usage.switch"
    const val AUTO_REPORTS_ROW: String = "settings.privacy.auto-reports"
    const val AUTO_REPORTS_SWITCH: String = "settings.privacy.auto-reports.switch"
    const val REPORT_PROBLEM: String = "settings.diagnostics.report"
    const val EXPORT_DIAGNOSTICS: String = "settings.diagnostics.export"
    const val EXPORT_DIAGNOSTICS_STATUS: String = "settings.diagnostics.export.status"
    const val CONTACT_SUPPORT: String = "settings.diagnostics.support"

    const val DEVICE_NAME_ROW: String = "settings.phone.name"
    const val DEVICE_FINGERPRINT_ROW: String = "settings.phone.fingerprint"
    const val APP_VERSION_ROW: String = "settings.phone.version"

    fun accountRow(email: String): String = "settings.account.row.$email"
    fun accountSignInAgain(email: String): String = "settings.account.sign-in-again.$email"
    fun accountWaiting(email: String): String = "settings.account.waiting.$email"
}

/** User-selectable appearance option displayed in [SettingsScreen]. */
enum class SettingsAppearance {
    Light,
    Dark,
    System,
}

/** Current Android notification permission state displayed in [SettingsScreen]. */
enum class SettingsNotificationStatus {
    Enabled,
    Off,
    NotAsked,
}

/** Plan/entitlement badge shown beside an account in [SettingsScreen]. */
enum class SettingsEntitlementBadge {
    Pro,
    Trial,
    Included,
    Free,
}

/** Export status for diagnostics bundle creation. */
@Immutable
sealed interface DiagnosticsExportState {
    data object Idle : DiagnosticsExportState
    data object Exporting : DiagnosticsExportState
    data class Exported(val summary: String? = null) : DiagnosticsExportState
    data class Failed(val message: String) : DiagnosticsExportState
}

/** One account entry rendered in the Settings Accounts section. */
@Immutable
data class SettingsAccountItem(
    val id: String,
    val name: String,
    val email: String,
    val line: String = email,
    val signedIn: Boolean = true,
    val selected: Boolean = false,
    val expired: Boolean = false,
    val entitlementBadge: SettingsEntitlementBadge = SettingsEntitlementBadge.Free,
    val entitlementSummary: String? = null,
    val signingIn: Boolean = false,
    /** How many of its agents need you, where something reported it; null where nothing has. */
    val attention: Int? = null,
) {
    val initials: String
        get() = AccountInitials.from(name = name, email = email)

    val needsSignInAgain: Boolean
        get() = !signedIn || expired

    val hasProAccess: Boolean
        get() = entitlementBadge == SettingsEntitlementBadge.Pro ||
            entitlementBadge == SettingsEntitlementBadge.Trial ||
            entitlementBadge == SettingsEntitlementBadge.Included
}

/** Pure UI state rendered by [SettingsScreen]. */
@Immutable
data class SettingsScreenState(
    val accounts: List<SettingsAccountItem> = emptyList(),
    val addingAccount: Boolean = false,
    val appearance: SettingsAppearance = SettingsAppearance.System,
    val notificationStatus: SettingsNotificationStatus = SettingsNotificationStatus.NotAsked,
    val pushDeliveryComingSoon: Boolean = true,
    val shareUsage: Boolean = true,
    val autoReports: Boolean = false,
    val diagnosticsExport: DiagnosticsExportState = DiagnosticsExportState.Idle,
    val deviceName: String = "",
    val deviceFingerprint: String = "",
    val fingerprintCopied: Boolean = false,
    val appVersion: String = "",
    val showContactSupport: Boolean = true,
) {
    val hasAccounts: Boolean
        get() = accounts.isNotEmpty()

    val selectedAccount: SettingsAccountItem?
        get() = accounts.firstOrNull { it.selected } ?: accounts.firstOrNull()
}

/** Actions emitted by [SettingsScreen] without touching navigation or runtime stores. */
sealed interface SettingsAction {
    data object Back : SettingsAction
    data object SignIn : SettingsAction
    data object AddAccount : SettingsAction
    data class SelectAccount(val accountId: String) : SettingsAction
    data class SignInAgain(val accountId: String) : SettingsAction
    data class SignOut(val accountId: String) : SettingsAction
    data class ManageSubscription(val accountId: String) : SettingsAction
    data class RemoveAccount(val accountId: String) : SettingsAction
    data class DeleteAccount(val accountId: String) : SettingsAction
    data class SetAppearance(val appearance: SettingsAppearance) : SettingsAction
    data object EnableNotifications : SettingsAction
    data object OpenNotificationSettings : SettingsAction
    data class SetShareUsage(val enabled: Boolean) : SettingsAction
    data class SetAutoReports(val enabled: Boolean) : SettingsAction
    data object ReportProblem : SettingsAction
    data object ExportDiagnostics : SettingsAction
    data object ContactSupport : SettingsAction
    data class CopyFingerprint(val fingerprint: String) : SettingsAction
}

/**
 * Full-screen Settings, in four groups: the accounts on this phone, the
 * selected account's own actions under them, what belongs to this phone, and
 * help.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SettingsScreen(
    state: SettingsScreenState,
    onAction: (SettingsAction) -> Unit,
    modifier: Modifier = Modifier,
) {
    val colors = AmuxTheme.colors
    val backLabel = stringResource(R.string.action_back)
    val largeText = LocalDensity.current.fontScale >= 1.3f
    val scroll = rememberScrollState()

    AmuxGround(modifier = modifier.testTag(SettingsTags.SCREEN)) {
        Scaffold(
            containerColor = Color.Transparent,
            contentColor = colors.ink,
            topBar = {
                AmuxTopBar(
                    title = stringResource(R.string.settings_title),
                    scrolled = scroll.canScrollBackward,
                    onNavigate = { onAction(SettingsAction.Back) },
                    navigationLabel = backLabel,
                    navigationModifier = Modifier.testTag(SettingsTags.BACK),
                )
            },
        ) { innerPadding ->
            Column(
                modifier = Modifier
                    .fillMaxSize()
                    .padding(innerPadding)
                    .verticalScroll(scroll)
                    .windowInsetsPadding(WindowInsets.navigationBars)
                    .padding(bottom = Space.xxl)
                    .testTag(SettingsTags.SCROLL),
                verticalArrangement = Arrangement.spacedBy(Space.l),
            ) {
                Spacer(modifier = Modifier.height(Space.xxs))
                AccountsSection(state = state, largeText = largeText, onAction = onAction)
                state.selectedAccount?.let { active ->
                    AccountActionsSection(active = active, largeText = largeText, onAction = onAction)
                }
                ThisPhoneSection(state = state, largeText = largeText, onAction = onAction)
                HelpSection(state = state, onAction = onAction)
            }
        }
    }
}

@Composable
private fun AccountsSection(
    state: SettingsScreenState,
    largeText: Boolean,
    onAction: (SettingsAction) -> Unit,
) {
    val colors = AmuxTheme.colors
    Column(modifier = Modifier.fillMaxWidth()) {
        AmuxSectionHeader(title = stringResource(R.string.settings_section_accounts))
        AmuxRowCard(modifier = Modifier.padding(horizontal = Space.margin)) {
            state.accounts.forEachIndexed { index, account ->
                if (index > 0) {
                    AmuxInsetHairline(leadingInset = Space.l)
                }
                SettingsAccountRow(
                    account = account,
                    largeText = largeText,
                    onSelect = { onAction(SettingsAction.SelectAccount(account.id)) },
                    onSignInAgain = { onAction(SettingsAction.SignInAgain(account.id)) },
                    onRemove = { onAction(SettingsAction.RemoveAccount(account.id)) },
                )
            }
            if (state.hasAccounts) {
                AmuxInsetHairline(leadingInset = Space.l)
            }
            when {
                state.addingAccount -> WorkingInlineRow(
                    label = stringResource(R.string.settings_signing_in),
                    modifier = Modifier
                        .fillMaxWidth()
                        .padding(horizontal = Space.l, vertical = Space.m)
                        .testTag(if (state.hasAccounts) SettingsTags.ADD_ACCOUNT else SettingsTags.SIGN_IN_BUTTON),
                )
                // A phone with nobody signed in is not a broken phone: it
                // pairs and runs agents on this network. So the way in is one
                // quiet row, and the caption under it says what an account adds.
                !state.hasAccounts -> AccentIconRow(
                    icon = Icons.Outlined.AccountCircle,
                    label = stringResource(R.string.settings_sign_in),
                    onClick = { onAction(SettingsAction.SignIn) },
                    modifier = Modifier.testTag(SettingsTags.SIGN_IN_BUTTON),
                )
                else -> AccentIconRow(
                    icon = Icons.Outlined.Add,
                    label = stringResource(R.string.settings_add_account),
                    onClick = { onAction(SettingsAction.AddAccount) },
                    modifier = Modifier.testTag(SettingsTags.ADD_ACCOUNT),
                )
            }
        }
        if (!state.hasAccounts) {
            SectionCaption(
                text = stringResource(R.string.settings_signed_out_detail),
                modifier = Modifier.testTag(SettingsTags.SIGNED_OUT_CARD),
            )
        }
    }
}

/** The selected account's own actions, under the list of accounts they continue. */
@Composable
private fun AccountActionsSection(
    active: SettingsAccountItem,
    largeText: Boolean,
    onAction: (SettingsAction) -> Unit,
) {
    val colors = AmuxTheme.colors
    Column(modifier = Modifier.fillMaxWidth()) {
        AmuxSectionHeader(
            title = active.name.ifBlank { stringResource(R.string.settings_section_account_actions) },
        )
        AmuxRowCard(modifier = Modifier.padding(horizontal = Space.margin)) {
            // Headed by what it buys, with whether this account has it.
            SettingsNavigationRow(
                label = stringResource(R.string.settings_pro),
                value = stringResource(
                    if (active.hasProAccess) R.string.settings_pro_subscribed else R.string.settings_pro_not_subscribed,
                ),
                largeText = largeText,
                onClick = { onAction(SettingsAction.ManageSubscription(active.id)) },
                modifier = Modifier.testTag(SettingsTags.MANAGE_SUBSCRIPTION),
            )
            if (active.signedIn && !active.expired) {
                AmuxInsetHairline(leadingInset = Space.l)
                SettingsActionRow(
                    label = stringResource(R.string.settings_sign_out),
                    color = colors.accent,
                    onClick = { onAction(SettingsAction.SignOut(active.id)) },
                    modifier = Modifier.testTag(SettingsTags.SIGN_OUT),
                )
            }
            AmuxInsetHairline(leadingInset = Space.l)
            SettingsActionRow(
                label = stringResource(R.string.settings_remove_from_phone),
                color = colors.accent,
                onClick = { onAction(SettingsAction.RemoveAccount(active.id)) },
                modifier = Modifier.testTag(SettingsTags.REMOVE_ACCOUNT),
            )
            AmuxInsetHairline(leadingInset = Space.l)
            // The one destructive thing on the page, in the colour kept for it.
            SettingsActionRow(
                label = stringResource(R.string.settings_delete_account),
                color = colors.removed,
                onClick = { onAction(SettingsAction.DeleteAccount(active.id)) },
                modifier = Modifier.testTag(SettingsTags.DELETE_ACCOUNT),
            )
        }
    }
}

@Composable
private fun SettingsAccountRow(
    account: SettingsAccountItem,
    largeText: Boolean,
    onSelect: () -> Unit,
    onSignInAgain: () -> Unit,
    onRemove: () -> Unit,
) {
    val colors = AmuxTheme.colors
    val subtitle = when {
        account.expired -> stringResource(R.string.settings_account_expired)
        !account.signedIn -> stringResource(R.string.settings_account_signed_out)
        else -> account.line.ifBlank { account.email }
    }
    val rowClick = {
        if (!account.signingIn) {
            if (account.needsSignInAgain) onSignInAgain() else onSelect()
        }
    }
    val name = account.name.ifBlank { account.email }
    val waiting = account.attention?.takeIf { it > 0 && !account.selected && !account.needsSignInAgain }
    val spokenState = when {
        account.signingIn -> stringResource(R.string.settings_signing_in)
        account.selected -> stringResource(R.string.settings_account_active)
        account.needsSignInAgain -> stringResource(R.string.settings_account_signed_out)
        else -> null
    }
    val spokenWaiting = waiting?.let { stringResource(R.string.settings_account_waiting_spoken, it) }
    val removeLabel = stringResource(R.string.settings_remove_from_phone)

    Row(
        modifier = Modifier
            .fillMaxWidth()
            .heightIn(min = ThumbTarget.MinSize)
            .clickable(enabled = !account.signingIn, role = Role.Button, onClick = rowClick)
            .semantics(mergeDescendants = true) {
                selected = account.selected
                contentDescription = listOfNotNull(name, subtitle, spokenWaiting).joinToString(", ")
                spokenState?.let { stateDescription = it }
                // An account signed out of is the one most likely to be
                // taken off the phone; its own section is only ever the
                // selected account's.
                customActions = listOf(
                    CustomAccessibilityAction(removeLabel) {
                        onRemove()
                        true
                    },
                )
            }
            .padding(horizontal = Space.l, vertical = Space.m)
            .testTag(SettingsTags.accountRow(account.email)),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(Space.m),
    ) {
        AccountMonogram(
            initials = account.initials,
            selected = account.selected,
            signedIn = account.signedIn && !account.expired,
        )
        Column(
            modifier = Modifier.weight(1f),
            verticalArrangement = Arrangement.spacedBy(Space.xxs),
        ) {
            Text(
                text = name,
                style = AmuxTheme.typography.body,
                color = if (account.needsSignInAgain) colors.inkMuted else colors.ink,
                maxLines = if (largeText) 2 else 1,
                overflow = TextOverflow.Ellipsis,
            )
            Text(
                text = subtitle,
                style = AmuxTheme.typography.caption,
                color = if (account.expired) colors.accent else colors.inkFaint,
                maxLines = if (largeText) 2 else 1,
                overflow = TextOverflow.Ellipsis,
            )
        }
        when {
            account.signingIn -> WorkingInlineRow(label = stringResource(R.string.settings_signing_in))
            // Its own button, so it keeps its name inside the row's merged
            // semantics and can be reached by it. A signed-out account offers
            // it even while selected: signing back in is what it needs.
            account.needsSignInAgain -> Box(
                modifier = Modifier
                    .spillTarget()
                    .clickable(role = Role.Button, onClick = onSignInAgain)
                    .testTag(SettingsTags.accountSignInAgain(account.email)),
                contentAlignment = Alignment.Center,
            ) {
                Text(
                    text = stringResource(R.string.settings_sign_in),
                    style = AmuxTheme.typography.caption,
                    color = colors.accent,
                    maxLines = 1,
                )
            }
            account.selected -> AmuxCheckMark()
            waiting != null -> AttentionCount(
                count = waiting,
                modifier = Modifier.testTag(SettingsTags.accountWaiting(account.email)),
            )
        }
    }
}

/**
 * How many agents need you on an account not on screen: drawn only from a
 * count something reported, so an account this phone has not reached says
 * nothing here.
 */
@Composable
private fun AttentionCount(count: Int, modifier: Modifier = Modifier) {
    val colors = AmuxTheme.colors
    Box(
        modifier = modifier
            .clip(AmuxShapes.Pill)
            .background(colors.accentFill)
            .padding(horizontal = 7.dp, vertical = 2.dp),
        contentAlignment = Alignment.Center,
    ) {
        Text(
            text = count.toString(),
            style = AmuxTheme.typography.monoSmall,
            color = colors.onAccent,
            maxLines = 1,
        )
    }
}

/** An accent row that starts something: a glyph on the monograms' column and its words. */
@Composable
private fun AccentIconRow(
    icon: ImageVector,
    label: String,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val colors = AmuxTheme.colors
    Row(
        modifier = modifier
            .fillMaxWidth()
            .heightIn(min = ThumbTarget.MinSize)
            .clickable(role = Role.Button, onClick = onClick)
            .padding(horizontal = Space.l, vertical = Space.m),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(Space.m),
    ) {
        // On the account rows' columns: the glyph centred where their
        // monograms are, the words where their names start.
        Box(modifier = Modifier.size(AccountMonogramSize), contentAlignment = Alignment.Center) {
            Icon(
                imageVector = icon,
                contentDescription = null,
                tint = colors.accent,
                modifier = Modifier.size(IconSize.Row),
            )
        }
        Text(
            text = label,
            style = AmuxTheme.typography.body,
            color = colors.accent,
        )
    }
}

/** A caption under a group, on the group's text column. */
@Composable
private fun SectionCaption(text: String, modifier: Modifier = Modifier) {
    Text(
        text = text,
        style = AmuxTheme.typography.caption,
        color = AmuxTheme.colors.inkFaint,
        modifier = modifier.padding(
            start = Space.margin + Space.l,
            end = Space.margin + Space.l,
            top = Space.s,
        ),
    )
}

private val AccountMonogramSize = 34.dp

@Composable
private fun AccountMonogram(
    initials: String,
    selected: Boolean,
    signedIn: Boolean,
) {
    val colors = AmuxTheme.colors
    // The account in use carries the interactive tint, as the account disc
    // on the Agents screen does.
    val fill = if (selected && signedIn) colors.accentFill else colors.sunken
    val foreground = if (selected && signedIn) colors.onAccent else colors.inkMuted
    // The letters are sized from the disc, not the font scale, so they stay
    // inside it at the largest text; the name beside it carries the words.
    val letters = with(LocalDensity.current) { (AccountMonogramSize * 0.36f).toSp() }
    Box(
        modifier = Modifier
            .size(AccountMonogramSize)
            .clip(CircleShape)
            .background(fill)
            .border(
                width = if (selected && signedIn) 0.dp else AmuxTheme.metrics.hairline,
                color = colors.hairline,
                shape = CircleShape,
            ),
        contentAlignment = Alignment.Center,
    ) {
        Text(
            text = initials,
            style = AmuxTheme.typography.badge.copy(fontSize = letters, lineHeight = letters),
            color = foreground,
            maxLines = 1,
        )
    }
}

/**
 * What belongs to this phone rather than to an account: how it looks, what it
 * sends, its notifications, and what the computers it pairs with know it by.
 */
@Composable
private fun ThisPhoneSection(
    state: SettingsScreenState,
    largeText: Boolean,
    onAction: (SettingsAction) -> Unit,
) {
    val colors = AmuxTheme.colors
    val groupedFingerprint = remember(state.deviceFingerprint) {
        FingerprintFormat.grouped(state.deviceFingerprint)
    }

    Column(modifier = Modifier.fillMaxWidth()) {
        AmuxSectionHeader(title = stringResource(R.string.settings_section_this_phone))
        AmuxRowCard(modifier = Modifier.padding(horizontal = Space.margin)) {
            AppearanceRow(
                selected = state.appearance,
                largeText = largeText,
                onSelect = { onAction(SettingsAction.SetAppearance(it)) },
            )
            AmuxInsetHairline(leadingInset = Space.l)
            SettingsToggleRow(
                title = stringResource(R.string.settings_share_usage_title),
                detail = stringResource(R.string.settings_share_usage_detail),
                checked = state.shareUsage,
                onCheckedChange = { onAction(SettingsAction.SetShareUsage(it)) },
                rowTag = SettingsTags.SHARE_USAGE_ROW,
                switchTag = SettingsTags.SHARE_USAGE_SWITCH,
            )
            AmuxInsetHairline(leadingInset = Space.l)
            SettingsToggleRow(
                title = stringResource(R.string.settings_auto_reports_title),
                detail = stringResource(R.string.settings_auto_reports_detail),
                checked = state.autoReports,
                onCheckedChange = { onAction(SettingsAction.SetAutoReports(it)) },
                rowTag = SettingsTags.AUTO_REPORTS_ROW,
                switchTag = SettingsTags.AUTO_REPORTS_SWITCH,
            )
            if (!state.pushDeliveryComingSoon) {
                AmuxInsetHairline(leadingInset = Space.l)
                NotificationsRow(status = state.notificationStatus, largeText = largeText, onAction = onAction)
            }
            if (state.deviceName.isNotEmpty()) {
                AmuxInsetHairline(leadingInset = Space.l)
                Box(
                    modifier = Modifier
                        .fillMaxWidth()
                        .heightIn(min = ThumbTarget.MinSize)
                        .semantics(mergeDescendants = true) {}
                        .padding(horizontal = Space.l, vertical = Space.m)
                        .testTag(SettingsTags.DEVICE_NAME_ROW),
                    contentAlignment = Alignment.CenterStart,
                ) {
                    AdaptiveLabelValue(
                        label = stringResource(R.string.settings_device_name_label),
                        value = state.deviceName,
                        largeText = largeText,
                    )
                }
            }
            if (groupedFingerprint.isNotEmpty()) {
                AmuxInsetHairline(leadingInset = Space.l)
                FingerprintRow(
                    grouped = groupedFingerprint,
                    copied = state.fingerprintCopied,
                    onCopy = { onAction(SettingsAction.CopyFingerprint(state.deviceFingerprint)) },
                )
            }
            if (state.appVersion.isNotEmpty()) {
                AmuxInsetHairline(leadingInset = Space.l)
                Box(
                    modifier = Modifier
                        .fillMaxWidth()
                        .semantics(mergeDescendants = true) {}
                        .padding(horizontal = Space.l, vertical = Space.m)
                        .testTag(SettingsTags.APP_VERSION_ROW),
                ) {
                    AdaptiveLabelValue(
                        label = stringResource(R.string.settings_app_version_label),
                        value = state.appVersion,
                        largeText = largeText,
                        monoValue = true,
                    )
                }
            }
        }
    }
}

/**
 * Appearance is the value, not a page holding it: the label and a compact
 * choice of three on one row, the choice under the label when the text is
 * too large for both.
 */
@Composable
private fun AppearanceRow(
    selected: SettingsAppearance,
    largeText: Boolean,
    onSelect: (SettingsAppearance) -> Unit,
) {
    val colors = AmuxTheme.colors
    val options = listOf(
        Triple(SettingsAppearance.Light, stringResource(R.string.settings_appearance_light), SettingsTags.APPEARANCE_LIGHT),
        Triple(SettingsAppearance.Dark, stringResource(R.string.settings_appearance_dark), SettingsTags.APPEARANCE_DARK),
        Triple(SettingsAppearance.System, stringResource(R.string.settings_appearance_system), SettingsTags.APPEARANCE_SYSTEM),
    )
    val label = @Composable {
        Text(
            text = stringResource(R.string.settings_appearance_label),
            style = AmuxTheme.typography.body,
            color = colors.ink,
        )
    }
    val choice = @Composable { modifier: Modifier ->
        AmuxSegmentedChoice(
            options = options.map { it.first },
            selected = selected,
            label = { option -> options.first { it.first == option }.second },
            onSelect = onSelect,
            tag = { option -> options.first { it.first == option }.third },
            modifier = modifier,
        )
    }
    if (largeText) {
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = Space.l, vertical = Space.s)
                .testTag(SettingsTags.APPEARANCE_GROUP),
            verticalArrangement = Arrangement.spacedBy(Space.xs),
        ) {
            label()
            choice(Modifier.fillMaxWidth())
        }
    } else {
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = Space.l, vertical = Space.xs)
                .testTag(SettingsTags.APPEARANCE_GROUP),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(Space.m),
        ) {
            label()
            Spacer(modifier = Modifier.weight(1f))
            choice(Modifier.width(AppearanceChoiceWidth))
        }
    }
}

/** Wide enough for "System" and its neighbours at the default text size. */
private val AppearanceChoiceWidth = 240.dp

/**
 * What Android says about notifications, and a press that goes where it can
 * be changed: the system's question the first time, its settings after, the
 * only place they are turned off or on again once asked.
 */
@Composable
private fun NotificationsRow(
    status: SettingsNotificationStatus,
    largeText: Boolean,
    onAction: (SettingsAction) -> Unit,
) {
    val value = stringResource(
        if (status == SettingsNotificationStatus.Enabled) R.string.settings_notifications_on else R.string.settings_notifications_off,
    )
    Box(modifier = Modifier.testTag(SettingsTags.NOTIFICATIONS_CARD)) {
        SettingsNavigationRow(
            label = stringResource(R.string.settings_notifications_label),
            value = value,
            largeText = largeText,
            valueTag = SettingsTags.NOTIFICATIONS_STATUS,
            onClick = {
                onAction(
                    if (status == SettingsNotificationStatus.NotAsked) {
                        SettingsAction.EnableNotifications
                    } else {
                        SettingsAction.OpenNotificationSettings
                    },
                )
            },
            modifier = Modifier.testTag(SettingsTags.NOTIFICATIONS_ACTION),
        )
    }
}

@Composable
private fun FingerprintRow(
    grouped: String,
    copied: Boolean,
    onCopy: () -> Unit,
) {
    val colors = AmuxTheme.colors
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .heightIn(min = ThumbTarget.MinSize)
            .clickable(role = Role.Button, onClickLabel = stringResource(R.string.settings_device_fingerprint_copy), onClick = onCopy)
            .padding(horizontal = Space.l, vertical = Space.m)
            .testTag(SettingsTags.DEVICE_FINGERPRINT_ROW),
        verticalArrangement = Arrangement.spacedBy(Space.xs),
    ) {
        Row(
            modifier = Modifier.fillMaxWidth(),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(Space.s),
        ) {
            Text(
                text = stringResource(R.string.settings_device_fingerprint_label),
                style = AmuxTheme.typography.body,
                color = colors.ink,
                modifier = Modifier.weight(1f),
            )
            Row(
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(Space.xs),
            ) {
                Icon(
                    imageVector = if (copied) Icons.Outlined.Check else Icons.Outlined.ContentCopy,
                    contentDescription = null,
                    tint = colors.inkMuted,
                    modifier = Modifier.size(IconSize.Inline),
                )
                Text(
                    text = stringResource(
                        if (copied) R.string.settings_device_fingerprint_copied else R.string.settings_device_fingerprint_tap,
                    ),
                    style = AmuxTheme.typography.caption,
                    color = colors.inkMuted,
                )
            }
        }
        Text(
            text = grouped,
            style = AmuxTheme.typography.monoSmall,
            color = colors.inkMuted,
        )
    }
}

/** Something wrong is one intent with three ways out: ask, report, or export. */
@Composable
private fun HelpSection(
    state: SettingsScreenState,
    onAction: (SettingsAction) -> Unit,
) {
    Column(modifier = Modifier.fillMaxWidth()) {
        AmuxSectionHeader(title = stringResource(R.string.settings_section_help))
        AmuxRowCard(modifier = Modifier.padding(horizontal = Space.margin)) {
            if (state.showContactSupport) {
                SettingsIconRow(
                    label = stringResource(R.string.settings_contact_support),
                    icon = Icons.Outlined.Email,
                    onClick = { onAction(SettingsAction.ContactSupport) },
                    modifier = Modifier.testTag(SettingsTags.CONTACT_SUPPORT),
                )
                AmuxInsetHairline(leadingInset = Space.l)
            }
            SettingsIconRow(
                label = stringResource(R.string.settings_report_problem),
                icon = Icons.Outlined.BugReport,
                onClick = { onAction(SettingsAction.ReportProblem) },
                modifier = Modifier.testTag(SettingsTags.REPORT_PROBLEM),
            )
            AmuxInsetHairline(leadingInset = Space.l)
            ExportDiagnosticsRow(
                exportState = state.diagnosticsExport,
                onExport = { onAction(SettingsAction.ExportDiagnostics) },
            )
        }
        SectionCaption(text = stringResource(R.string.settings_diagnostics_caption))
    }
}

@Composable
private fun SettingsToggleRow(
    title: String,
    detail: String,
    checked: Boolean,
    onCheckedChange: (Boolean) -> Unit,
    rowTag: String,
    switchTag: String,
) {
    val colors = AmuxTheme.colors
    AmuxSwitchRow(
        checked = checked,
        onCheckedChange = onCheckedChange,
        modifier = Modifier
            .fillMaxWidth()
            .heightIn(min = ThumbTarget.MinSize)
            .testTag(rowTag),
        switchModifier = Modifier.testTag(switchTag),
    ) {
        Column(
            modifier = Modifier.weight(1f),
            verticalArrangement = Arrangement.spacedBy(Space.xxs),
        ) {
            Text(
                text = title,
                style = AmuxTheme.typography.body,
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

@Composable
private fun ExportDiagnosticsRow(
    exportState: DiagnosticsExportState,
    onExport: () -> Unit,
) {
    val colors = AmuxTheme.colors
    val busy = exportState is DiagnosticsExportState.Exporting
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .heightIn(min = ThumbTarget.MinSize)
            .clickable(enabled = !busy, role = Role.Button, onClick = onExport)
            .padding(horizontal = Space.l, vertical = Space.m)
            .testTag(SettingsTags.EXPORT_DIAGNOSTICS),
        verticalArrangement = Arrangement.spacedBy(Space.xs),
    ) {
        Row(
            modifier = Modifier.fillMaxWidth(),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(Space.m),
        ) {
            if (busy) {
                AmuxStateMark(state = RowState.Working, size = 12.dp)
            } else {
                Icon(
                    imageVector = Icons.Outlined.FileDownload,
                    contentDescription = null,
                    tint = colors.inkMuted,
                    modifier = Modifier.size(IconSize.Row),
                )
            }
            Text(
                text = stringResource(
                    if (busy) {
                        R.string.settings_exporting_diagnostics
                    } else {
                        R.string.settings_export_diagnostics
                    },
                ),
                style = AmuxTheme.typography.body,
                color = if (busy) colors.inkMuted else colors.ink,
                modifier = Modifier.weight(1f),
            )
        }
        when (exportState) {
            DiagnosticsExportState.Idle,
            DiagnosticsExportState.Exporting,
            -> Unit
            is DiagnosticsExportState.Exported -> {
                Text(
                    text = exportState.summary ?: stringResource(R.string.settings_diagnostics_exported),
                    style = AmuxTheme.typography.caption,
                    color = colors.inkMuted,
                    modifier = Modifier.testTag(SettingsTags.EXPORT_DIAGNOSTICS_STATUS),
                )
            }
            is DiagnosticsExportState.Failed -> {
                Text(
                    text = exportState.message,
                    style = AmuxTheme.typography.caption,
                    color = colors.removed,
                    modifier = Modifier.testTag(SettingsTags.EXPORT_DIAGNOSTICS_STATUS),
                )
            }
        }
    }
}

@Composable
private fun AdaptiveLabelValue(
    label: String,
    value: String,
    largeText: Boolean,
    monoValue: Boolean = false,
    valueTag: String? = null,
) {
    val colors = AmuxTheme.colors
    val valueStyle = if (monoValue) AmuxTheme.typography.monoSmall else AmuxTheme.typography.body
    val valueModifier = if (valueTag != null) Modifier.testTag(valueTag) else Modifier

    if (largeText) {
        Column(
            modifier = Modifier.fillMaxWidth(),
            verticalArrangement = Arrangement.spacedBy(Space.xs),
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
                modifier = valueModifier,
            )
        }
    } else {
        Row(
            modifier = Modifier.fillMaxWidth(),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(Space.m),
        ) {
            Text(
                text = label,
                style = AmuxTheme.typography.body,
                color = colors.ink,
                modifier = Modifier.weight(1f),
            )
            Text(
                text = value,
                style = valueStyle,
                color = colors.inkMuted,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                modifier = valueModifier,
            )
        }
    }
}

@Composable
private fun SettingsNavigationRow(
    label: String,
    value: String,
    largeText: Boolean,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
    valueTag: String? = null,
) {
    Row(
        modifier = modifier
            .fillMaxWidth()
            .heightIn(min = ThumbTarget.MinSize)
            .clickable(role = Role.Button, onClick = onClick)
            .padding(horizontal = Space.l, vertical = Space.m),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(Space.s),
    ) {
        Box(modifier = Modifier.weight(1f)) {
            AdaptiveLabelValue(
                label = label,
                value = value,
                largeText = largeText,
                valueTag = valueTag,
            )
        }
        AmuxDisclosureChevron()
    }
}

@Composable
private fun SettingsActionRow(
    label: String,
    color: Color,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Row(
        modifier = modifier
            .fillMaxWidth()
            .heightIn(min = ThumbTarget.MinSize)
            .clickable(role = Role.Button, onClick = onClick)
            .padding(horizontal = Space.l, vertical = Space.m),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(
            text = label,
            style = AmuxTheme.typography.body,
            color = color,
        )
    }
}

@Composable
private fun SettingsIconRow(
    label: String,
    icon: ImageVector,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val colors = AmuxTheme.colors
    Row(
        modifier = modifier
            .fillMaxWidth()
            .heightIn(min = ThumbTarget.MinSize)
            .clickable(role = Role.Button, onClick = onClick)
            .padding(horizontal = Space.l, vertical = Space.m),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(Space.m),
    ) {
        Icon(
            imageVector = icon,
            contentDescription = null,
            tint = colors.inkMuted,
            modifier = Modifier.size(IconSize.Row),
        )
        Text(
            text = label,
            style = AmuxTheme.typography.body,
            color = colors.ink,
            modifier = Modifier.weight(1f),
        )
        AmuxDisclosureChevron()
    }
}

@Composable
private fun WorkingInlineRow(
    label: String,
    modifier: Modifier = Modifier,
) {
    val colors = AmuxTheme.colors
    Row(
        modifier = modifier.thumbTarget(),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(Space.s),
    ) {
        AmuxStateMark(state = RowState.Working, size = 10.dp)
        Text(
            text = label,
            style = AmuxTheme.typography.bodyEmphasis,
            color = colors.inkMuted,
        )
    }
}
