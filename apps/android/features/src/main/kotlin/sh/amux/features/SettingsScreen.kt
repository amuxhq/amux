package sh.amux.features

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
import androidx.compose.material.icons.automirrored.outlined.ArrowBack
import androidx.compose.material.icons.automirrored.outlined.KeyboardArrowRight
import androidx.compose.material.icons.outlined.Add
import androidx.compose.material.icons.outlined.BugReport
import androidx.compose.material.icons.outlined.Check
import androidx.compose.material.icons.outlined.ContentCopy
import androidx.compose.material.icons.outlined.Email
import androidx.compose.material.icons.outlined.FileDownload
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Surface
import androidx.compose.material3.Switch
import androidx.compose.material3.SwitchDefaults
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.TopAppBarDefaults
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
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import sh.amux.design.AmuxGround
import sh.amux.design.AmuxHairline
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
    fun accountBadge(email: String): String = "settings.account.badge.$email"
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
 * Full-screen Settings route covering accounts, appearance, notifications,
 * privacy & diagnostics, and this phone's identity.
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

    AmuxGround(modifier = modifier.testTag(SettingsTags.SCREEN)) {
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
                            IconButton(
                                onClick = { onAction(SettingsAction.Back) },
                                modifier = Modifier
                                    .thumbTarget()
                                    .testTag(SettingsTags.BACK),
                            ) {
                                Icon(
                                    imageVector = Icons.AutoMirrored.Outlined.ArrowBack,
                                    contentDescription = backLabel,
                                )
                            }
                        },
                        title = {
                            Text(
                                text = stringResource(R.string.settings_title),
                                style = AmuxTheme.typography.identifier,
                                color = colors.ink,
                                maxLines = 1,
                                overflow = TextOverflow.Ellipsis,
                            )
                        },
                    )
                    AmuxHairline(color = colors.hairline.copy(alpha = 0.6f))
                }
            },
        ) { innerPadding ->
            Column(
                modifier = Modifier
                    .fillMaxSize()
                    .padding(innerPadding)
                    .verticalScroll(rememberScrollState())
                    .windowInsetsPadding(WindowInsets.navigationBars)
                    .padding(bottom = Space.xxl)
                    .testTag(SettingsTags.SCROLL),
                verticalArrangement = Arrangement.spacedBy(Space.m),
            ) {
                Spacer(modifier = Modifier.height(Space.xs))

                AccountsSection(
                    state = state,
                    largeText = largeText,
                    onAction = onAction,
                )

                AppearanceSection(
                    selected = state.appearance,
                    largeText = largeText,
                    onSelect = { onAction(SettingsAction.SetAppearance(it)) },
                )

                NotificationsSection(
                    status = state.notificationStatus,
                    pushDeliveryComingSoon = state.pushDeliveryComingSoon,
                    largeText = largeText,
                    onAction = onAction,
                )

                PrivacyAndDiagnosticsSection(
                    state = state,
                    onAction = onAction,
                )

                ThisPhoneSection(
                    state = state,
                    largeText = largeText,
                    onAction = onAction,
                )
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
        if (!state.hasAccounts) {
            AmuxPanel(
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(horizontal = Space.margin)
                    .testTag(SettingsTags.SIGNED_OUT_CARD),
            ) {
                Column(
                    modifier = Modifier
                        .fillMaxWidth()
                        .padding(Space.l),
                    verticalArrangement = Arrangement.spacedBy(Space.m),
                ) {
                    Column(verticalArrangement = Arrangement.spacedBy(Space.xs)) {
                        Text(
                            text = stringResource(R.string.settings_signed_out_title),
                            style = AmuxTheme.typography.bodyEmphasis,
                            color = colors.ink,
                        )
                        Text(
                            text = stringResource(R.string.settings_signed_out_detail),
                            style = AmuxTheme.typography.detail,
                            color = colors.inkMuted,
                        )
                    }
                    if (state.addingAccount) {
                        WorkingInlineRow(
                            label = stringResource(R.string.settings_signing_in),
                            modifier = Modifier.testTag(SettingsTags.SIGN_IN_BUTTON),
                        )
                    } else {
                        AmuxActionButton(
                            label = stringResource(R.string.settings_sign_in),
                            kind = ActionButtonKind.Primary,
                            fillWidth = true,
                            onClick = { onAction(SettingsAction.SignIn) },
                            modifier = Modifier.testTag(SettingsTags.SIGN_IN_BUTTON),
                        )
                    }
                }
            }
            return
        }

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
                )
            }
            AmuxInsetHairline(leadingInset = Space.l)
            if (state.addingAccount) {
                WorkingInlineRow(
                    label = stringResource(R.string.settings_signing_in),
                    modifier = Modifier
                        .fillMaxWidth()
                        .padding(horizontal = Space.l, vertical = Space.m)
                        .testTag(SettingsTags.ADD_ACCOUNT),
                )
            } else {
                Row(
                    modifier = Modifier
                        .fillMaxWidth()
                        .thumbTarget()
                        .clickable(role = Role.Button) { onAction(SettingsAction.AddAccount) }
                        .padding(horizontal = Space.l, vertical = Space.m)
                        .testTag(SettingsTags.ADD_ACCOUNT),
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(Space.m),
                ) {
                    Icon(
                        imageVector = Icons.Outlined.Add,
                        contentDescription = null,
                        tint = colors.accent,
                        modifier = Modifier.size(20.dp),
                    )
                    Text(
                        text = stringResource(R.string.settings_add_account),
                        style = AmuxTheme.typography.bodyEmphasis,
                        color = colors.accent,
                    )
                }
            }
        }

        val active = state.selectedAccount ?: return
        Spacer(modifier = Modifier.height(Space.m))
        AmuxSectionHeader(
            title = active.name.ifBlank { stringResource(R.string.settings_section_account_actions) },
        )
        AmuxRowCard(modifier = Modifier.padding(horizontal = Space.margin)) {
            val subscriptionTitle = stringResource(
                if (active.hasProAccess) {
                    R.string.settings_manage_subscription
                } else {
                    R.string.settings_upgrade
                },
            )
            val subscriptionValue = active.entitlementSummary ?: badgeLabel(active.entitlementBadge)
            SettingsNavigationRow(
                label = subscriptionTitle,
                value = subscriptionValue,
                largeText = largeText,
                onClick = { onAction(SettingsAction.ManageSubscription(active.id)) },
                modifier = Modifier.testTag(SettingsTags.MANAGE_SUBSCRIPTION),
            )
            if (active.signedIn && !active.expired) {
                AmuxInsetHairline(leadingInset = Space.l)
                SettingsActionRow(
                    label = stringResource(R.string.settings_sign_out),
                    color = colors.ink,
                    onClick = { onAction(SettingsAction.SignOut(active.id)) },
                    modifier = Modifier.testTag(SettingsTags.SIGN_OUT),
                )
            }
            AmuxInsetHairline(leadingInset = Space.l)
            SettingsActionRow(
                label = stringResource(R.string.settings_remove_from_phone),
                color = colors.ink,
                onClick = { onAction(SettingsAction.RemoveAccount(active.id)) },
                modifier = Modifier.testTag(SettingsTags.REMOVE_ACCOUNT),
            )
            AmuxInsetHairline(leadingInset = Space.l)
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
) {
    val colors = AmuxTheme.colors
    val badgeText = badgeLabel(account.entitlementBadge)
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

    Column(
        modifier = Modifier
            .fillMaxWidth()
            .thumbTarget()
            .clickable(enabled = !account.signingIn, role = Role.Button, onClick = rowClick)
            .semantics { selected = account.selected }
            .padding(horizontal = Space.l, vertical = Space.m)
            .testTag(SettingsTags.accountRow(account.email)),
        verticalArrangement = Arrangement.spacedBy(Space.s),
    ) {
        Row(
            modifier = Modifier.fillMaxWidth(),
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
                    text = account.name.ifBlank { account.email },
                    style = AmuxTheme.typography.bodyEmphasis,
                    color = if (account.needsSignInAgain) colors.inkMuted else colors.ink,
                    maxLines = if (largeText) 2 else 1,
                    overflow = TextOverflow.Ellipsis,
                )
                Text(
                    text = subtitle,
                    style = AmuxTheme.typography.caption,
                    color = if (account.expired) colors.accent else colors.inkMuted,
                    maxLines = if (largeText) 2 else 1,
                    overflow = TextOverflow.Ellipsis,
                )
            }
            if (!largeText) {
                AccountTrailing(
                    account = account,
                    badgeText = badgeText,
                    onSignInAgain = onSignInAgain,
                )
            }
        }
        if (largeText) {
            AccountTrailing(
                account = account,
                badgeText = badgeText,
                onSignInAgain = onSignInAgain,
            )
        }
    }
}

@Composable
private fun AccountTrailing(
    account: SettingsAccountItem,
    badgeText: String,
    onSignInAgain: () -> Unit,
) {
    val colors = AmuxTheme.colors
    Row(
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(Space.s),
    ) {
        AmuxStatusChip(
            text = badgeText,
            accent = false,
            modifier = Modifier.testTag(SettingsTags.accountBadge(account.email)),
        )
        when {
            account.signingIn -> {
                WorkingInlineRow(label = stringResource(R.string.settings_signing_in))
            }
            account.needsSignInAgain -> {
                Surface(
                    onClick = onSignInAgain,
                    shape = RoundedCornerShape(AmuxTheme.metrics.controlRadius),
                    color = colors.sunken,
                    border = BorderStroke(AmuxTheme.metrics.hairline, colors.hairline),
                    modifier = Modifier
                        .thumbTarget()
                        .testTag(SettingsTags.accountSignInAgain(account.email)),
                ) {
                    Box(
                        modifier = Modifier.padding(horizontal = Space.m, vertical = Space.s),
                        contentAlignment = Alignment.Center,
                    ) {
                        Text(
                            text = stringResource(R.string.settings_sign_in_again),
                            style = AmuxTheme.typography.caption,
                            color = colors.accent,
                            maxLines = 1,
                        )
                    }
                }
            }
            account.selected -> {
                Icon(
                    imageVector = Icons.Outlined.Check,
                    contentDescription = stringResource(R.string.settings_account_active),
                    tint = colors.ink,
                    modifier = Modifier.size(18.dp),
                )
            }
        }
    }
}

@Composable
private fun AccountMonogram(
    initials: String,
    selected: Boolean,
    signedIn: Boolean,
) {
    val colors = AmuxTheme.colors
    val fill = if (selected && signedIn) colors.ink else colors.sunken
    val foreground = if (selected && signedIn) colors.ground else colors.inkMuted
    Box(
        modifier = Modifier
            .size(34.dp)
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
            style = AmuxTheme.typography.badge,
            color = foreground,
            maxLines = 1,
        )
    }
}

@Composable
private fun badgeLabel(badge: SettingsEntitlementBadge): String = when (badge) {
    SettingsEntitlementBadge.Pro -> stringResource(R.string.settings_badge_pro)
    SettingsEntitlementBadge.Trial -> stringResource(R.string.settings_badge_trial)
    SettingsEntitlementBadge.Included -> stringResource(R.string.settings_badge_included)
    SettingsEntitlementBadge.Free -> stringResource(R.string.settings_badge_free)
}

@Composable
private fun AppearanceSection(
    selected: SettingsAppearance,
    largeText: Boolean,
    onSelect: (SettingsAppearance) -> Unit,
) {
    val colors = AmuxTheme.colors
    Column(modifier = Modifier.fillMaxWidth()) {
        AmuxSectionHeader(title = stringResource(R.string.settings_section_appearance))
        AmuxPanel(
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = Space.margin)
                .testTag(SettingsTags.APPEARANCE_GROUP),
        ) {
            Column(
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(Space.l),
                verticalArrangement = Arrangement.spacedBy(Space.m),
            ) {
                Text(
                    text = stringResource(R.string.settings_appearance_label),
                    style = AmuxTheme.typography.body,
                    color = colors.ink,
                )
                val options = listOf(
                    Triple(
                        SettingsAppearance.Light,
                        stringResource(R.string.settings_appearance_light),
                        SettingsTags.APPEARANCE_LIGHT,
                    ),
                    Triple(
                        SettingsAppearance.Dark,
                        stringResource(R.string.settings_appearance_dark),
                        SettingsTags.APPEARANCE_DARK,
                    ),
                    Triple(
                        SettingsAppearance.System,
                        stringResource(R.string.settings_appearance_system),
                        SettingsTags.APPEARANCE_SYSTEM,
                    ),
                )
                if (largeText) {
                    Column(
                        modifier = Modifier
                            .fillMaxWidth()
                            .clip(RoundedCornerShape(AmuxTheme.metrics.controlRadius))
                            .background(colors.sunken)
                            .border(
                                AmuxTheme.metrics.hairline,
                                colors.hairline,
                                RoundedCornerShape(AmuxTheme.metrics.controlRadius),
                            )
                            .padding(Space.xs),
                        verticalArrangement = Arrangement.spacedBy(Space.xs),
                    ) {
                        options.forEach { (option, label, tag) ->
                            AppearanceSegmentButton(
                                label = label,
                                chosen = selected == option,
                                onClick = { onSelect(option) },
                                modifier = Modifier
                                    .fillMaxWidth()
                                    .testTag(tag),
                            )
                        }
                    }
                } else {
                    Row(
                        modifier = Modifier
                            .fillMaxWidth()
                            .clip(RoundedCornerShape(AmuxTheme.metrics.controlRadius))
                            .background(colors.sunken)
                            .border(
                                AmuxTheme.metrics.hairline,
                                colors.hairline,
                                RoundedCornerShape(AmuxTheme.metrics.controlRadius),
                            )
                            .padding(Space.xs),
                        horizontalArrangement = Arrangement.spacedBy(Space.xs),
                    ) {
                        options.forEach { (option, label, tag) ->
                            AppearanceSegmentButton(
                                label = label,
                                chosen = selected == option,
                                onClick = { onSelect(option) },
                                modifier = Modifier
                                    .weight(1f)
                                    .testTag(tag),
                            )
                        }
                    }
                }
            }
        }
    }
}

@Composable
private fun AppearanceSegmentButton(
    label: String,
    chosen: Boolean,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val colors = AmuxTheme.colors
    val shape = RoundedCornerShape(AmuxTheme.metrics.controlRadius - 2.dp)
    Box(
        modifier = modifier
            .thumbTarget()
            .clip(shape)
            .background(if (chosen) colors.ink else Color.Transparent)
            .clickable(role = Role.RadioButton, onClick = onClick)
            .semantics { selected = chosen }
            .padding(horizontal = Space.m, vertical = Space.s),
        contentAlignment = Alignment.Center,
    ) {
        Text(
            text = label,
            style = if (chosen) AmuxTheme.typography.bodyEmphasis else AmuxTheme.typography.body,
            color = if (chosen) colors.ground else colors.inkMuted,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
        )
    }
}

@Composable
private fun NotificationsSection(
    status: SettingsNotificationStatus,
    pushDeliveryComingSoon: Boolean,
    largeText: Boolean,
    onAction: (SettingsAction) -> Unit,
) {
    val colors = AmuxTheme.colors
    val statusText = when (status) {
        SettingsNotificationStatus.Enabled -> stringResource(R.string.settings_notifications_enabled)
        SettingsNotificationStatus.Off -> stringResource(R.string.settings_notifications_off)
        SettingsNotificationStatus.NotAsked -> stringResource(R.string.settings_notifications_not_asked)
    }
    val actionLabel = when (status) {
        SettingsNotificationStatus.NotAsked -> stringResource(R.string.settings_notifications_enable_button)
        SettingsNotificationStatus.Enabled,
        SettingsNotificationStatus.Off,
        -> stringResource(R.string.settings_notifications_settings_button)
    }
    val actionEvent = when (status) {
        SettingsNotificationStatus.NotAsked -> SettingsAction.EnableNotifications
        SettingsNotificationStatus.Enabled,
        SettingsNotificationStatus.Off,
        -> SettingsAction.OpenNotificationSettings
    }

    Column(modifier = Modifier.fillMaxWidth()) {
        AmuxSectionHeader(title = stringResource(R.string.settings_section_notifications))
        AmuxPanel(
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = Space.margin)
                .testTag(SettingsTags.NOTIFICATIONS_CARD),
        ) {
            Column(
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(Space.l),
                verticalArrangement = Arrangement.spacedBy(Space.m),
            ) {
                AdaptiveLabelValue(
                    label = stringResource(R.string.settings_notifications_label),
                    value = statusText,
                    largeText = largeText,
                    valueTag = SettingsTags.NOTIFICATIONS_STATUS,
                )
                if (pushDeliveryComingSoon) {
                    Text(
                        text = stringResource(R.string.settings_notifications_push_coming_soon),
                        style = AmuxTheme.typography.detail,
                        color = colors.inkMuted,
                    )
                }
                AmuxActionButton(
                    label = actionLabel,
                    kind = if (status == SettingsNotificationStatus.NotAsked) {
                        ActionButtonKind.Primary
                    } else {
                        ActionButtonKind.Outline
                    },
                    fillWidth = true,
                    onClick = { onAction(actionEvent) },
                    modifier = Modifier.testTag(SettingsTags.NOTIFICATIONS_ACTION),
                )
            }
        }
    }
}

@Composable
private fun PrivacyAndDiagnosticsSection(
    state: SettingsScreenState,
    onAction: (SettingsAction) -> Unit,
) {
    val colors = AmuxTheme.colors
    Column(modifier = Modifier.fillMaxWidth()) {
        AmuxSectionHeader(title = stringResource(R.string.settings_section_privacy_diagnostics))
        AmuxRowCard(modifier = Modifier.padding(horizontal = Space.margin)) {
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
            AmuxInsetHairline(leadingInset = Space.l)
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
            if (state.showContactSupport) {
                AmuxInsetHairline(leadingInset = Space.l)
                SettingsIconRow(
                    label = stringResource(R.string.settings_contact_support),
                    icon = Icons.Outlined.Email,
                    onClick = { onAction(SettingsAction.ContactSupport) },
                    modifier = Modifier.testTag(SettingsTags.CONTACT_SUPPORT),
                )
            }
        }
        Text(
            text = stringResource(R.string.settings_diagnostics_caption),
            style = AmuxTheme.typography.caption,
            color = colors.inkFaint,
            modifier = Modifier.padding(
                start = Space.margin + Space.xs,
                end = Space.margin + Space.xs,
                top = Space.s,
            ),
        )
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
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .thumbTarget()
            .clickable(role = Role.Switch) { onCheckedChange(!checked) }
            .padding(horizontal = Space.l, vertical = Space.m)
            .testTag(rowTag),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(Space.m),
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
        Switch(
            checked = checked,
            onCheckedChange = onCheckedChange,
            colors = SwitchDefaults.colors(
                checkedThumbColor = colors.ground,
                checkedTrackColor = colors.ink,
                checkedBorderColor = colors.ink,
                uncheckedThumbColor = colors.inkMuted,
                uncheckedTrackColor = colors.sunken,
                uncheckedBorderColor = colors.hairline,
            ),
            modifier = Modifier
                .thumbTarget()
                .testTag(switchTag),
        )
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
            .thumbTarget()
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
                    modifier = Modifier.size(20.dp),
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
            if (state.deviceName.isNotEmpty()) {
                Column(
                    modifier = Modifier
                        .fillMaxWidth()
                        .thumbTarget()
                        .padding(horizontal = Space.l, vertical = Space.m)
                        .testTag(SettingsTags.DEVICE_NAME_ROW),
                    verticalArrangement = Arrangement.spacedBy(Space.xs),
                ) {
                    AdaptiveLabelValue(
                        label = stringResource(R.string.settings_device_name_label),
                        value = state.deviceName,
                        largeText = largeText,
                    )
                }
            }

            if (groupedFingerprint.isNotEmpty()) {
                if (state.deviceName.isNotEmpty()) {
                    AmuxInsetHairline(leadingInset = Space.l)
                }
                Column(
                    modifier = Modifier
                        .fillMaxWidth()
                        .thumbTarget()
                        .clickable(role = Role.Button) {
                            onAction(SettingsAction.CopyFingerprint(state.deviceFingerprint))
                        }
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
                                imageVector = if (state.fingerprintCopied) {
                                    Icons.Outlined.Check
                                } else {
                                    Icons.Outlined.ContentCopy
                                },
                                contentDescription = null,
                                tint = colors.inkMuted,
                                modifier = Modifier.size(16.dp),
                            )
                            Text(
                                text = stringResource(
                                    if (state.fingerprintCopied) {
                                        R.string.settings_device_fingerprint_copied
                                    } else {
                                        R.string.settings_device_fingerprint_tap
                                    },
                                ),
                                style = AmuxTheme.typography.caption,
                                color = colors.inkMuted,
                            )
                        }
                    }
                    Text(
                        text = groupedFingerprint,
                        style = AmuxTheme.typography.monoSmall,
                        color = colors.inkMuted,
                    )
                }
            }

            if (state.appVersion.isNotEmpty()) {
                if (state.deviceName.isNotEmpty() || groupedFingerprint.isNotEmpty()) {
                    AmuxInsetHairline(leadingInset = Space.l)
                }
                Box(
                    modifier = Modifier
                        .fillMaxWidth()
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
) {
    val colors = AmuxTheme.colors
    Row(
        modifier = modifier
            .fillMaxWidth()
            .thumbTarget()
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
            )
        }
        Icon(
            imageVector = Icons.AutoMirrored.Outlined.KeyboardArrowRight,
            contentDescription = null,
            tint = colors.inkFaint,
            modifier = Modifier.size(20.dp),
        )
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
            .thumbTarget()
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
            .thumbTarget()
            .clickable(role = Role.Button, onClick = onClick)
            .padding(horizontal = Space.l, vertical = Space.m),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(Space.m),
    ) {
        Icon(
            imageVector = icon,
            contentDescription = null,
            tint = colors.inkMuted,
            modifier = Modifier.size(20.dp),
        )
        Text(
            text = label,
            style = AmuxTheme.typography.body,
            color = colors.ink,
            modifier = Modifier.weight(1f),
        )
        Icon(
            imageVector = Icons.AutoMirrored.Outlined.KeyboardArrowRight,
            contentDescription = null,
            tint = colors.inkFaint,
            modifier = Modifier.size(20.dp),
        )
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
