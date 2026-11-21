package sh.amux.shell

import java.time.Instant
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import java.time.format.FormatStyle
import sh.amux.core.runtime.AccountRegistryState
import sh.amux.core.runtime.CloudEndpoint
import sh.amux.core.runtime.Entitlement
import sh.amux.core.runtime.EntitlementSource
import sh.amux.core.runtime.Grant
import sh.amux.core.runtime.HostsState
import sh.amux.core.runtime.PaywallState
import sh.amux.core.runtime.PaywallStore
import sh.amux.core.runtime.Plan
import sh.amux.core.runtime.SignInIntent
import sh.amux.core.runtime.SignInState
import sh.amux.design.AppearanceMode
import sh.amux.features.DiagnosticsExportState
import sh.amux.features.PaywallGrantSource
import sh.amux.features.PaywallPhaseUi
import sh.amux.features.PaywallPlanPeriod
import sh.amux.features.PaywallPlanUi
import sh.amux.features.PaywallScreenState
import sh.amux.features.PaywallUnconfirmedReason
import sh.amux.features.SettingsAccountItem
import sh.amux.features.SettingsAppearance
import sh.amux.features.SettingsEntitlementBadge
import sh.amux.features.SettingsNotificationStatus
import sh.amux.features.SettingsScreenState
import sh.amux.values.RelayLink
import sh.amux.words.name

/** Pure mappers from accounts, preferences and the paywall store to Settings and the paywall. */
object SettingsMappers {
    fun toSettingsAppearance(mode: AppearanceMode): SettingsAppearance = when (mode) {
        AppearanceMode.Light -> SettingsAppearance.Light
        AppearanceMode.Dark -> SettingsAppearance.Dark
        AppearanceMode.System -> SettingsAppearance.System
    }

    fun toAppearanceMode(appearance: SettingsAppearance): AppearanceMode = when (appearance) {
        SettingsAppearance.Light -> AppearanceMode.Light
        SettingsAppearance.Dark -> AppearanceMode.Dark
        SettingsAppearance.System -> AppearanceMode.System
    }

    fun toSettingsScreenState(
        registry: AccountRegistryState,
        hosts: HostsState,
        relay: RelayLink,
        signIn: SignInState,
        appearance: AppearanceMode,
        notificationStatus: SettingsNotificationStatus,
        pushDeliveryComingSoon: Boolean = true,
        shareUsage: Boolean,
        autoReports: Boolean,
        diagnosticsExport: DiagnosticsExportState,
        deviceName: String,
        fingerprintCopied: Boolean,
        appVersion: String,
    ): SettingsScreenState {
        val returningId = (signIn.pending as? SignInIntent.Returning)?.account?.id
        val items = registry.accounts.map { entry ->
            val selected = entry.id == registry.selected
            val expired = selected && relay == RelayLink.SignInAgain
            val badge = when (val ent = entry.entitlement) {
                is Entitlement.Active -> when (ent.grant) {
                    Grant.Granted -> SettingsEntitlementBadge.Included
                    is Grant.Purchased -> SettingsEntitlementBadge.Pro
                }
                Entitlement.None, is Entitlement.Lapsed -> {
                    if (selected && hosts.account?.pro == true) {
                        SettingsEntitlementBadge.Pro
                    } else {
                        SettingsEntitlementBadge.Free
                    }
                }
            }
            val summary = when (val ent = entry.entitlement) {
                is Entitlement.Active -> ent.grant.named
                Entitlement.None, is Entitlement.Lapsed -> null
            }
            SettingsAccountItem(
                id = entry.id.value,
                name = entry.name,
                email = entry.account.email,
                line = entry.line,
                signedIn = entry.signedIn,
                selected = selected,
                expired = expired,
                entitlementBadge = badge,
                entitlementSummary = summary,
                signingIn = signIn.working && returningId == entry.id,
                attention = entry.attention,
            )
        }
        return SettingsScreenState(
            accounts = items,
            addingAccount = signIn.working && signIn.pending is SignInIntent.Adding,
            appearance = toSettingsAppearance(appearance),
            notificationStatus = notificationStatus,
            pushDeliveryComingSoon = pushDeliveryComingSoon,
            shareUsage = shareUsage,
            autoReports = autoReports,
            diagnosticsExport = diagnosticsExport,
            deviceName = deviceName,
            deviceFingerprint = hosts.roster?.identity?.fingerprint.orEmpty(),
            fingerprintCopied = fingerprintCopied,
            appVersion = appVersion,
            showContactSupport = true,
        )
    }

    fun toPaywallScreenState(
        paywall: PaywallState,
        accountEmail: String?,
        storeUnavailable: Boolean = false,
        privacyUrl: String = CloudEndpoint.Production.privacyUrl,
        formatRenews: (String) -> String = { "Renews $it" },
    ): PaywallScreenState {
        val plans = paywall.plans.map { plan ->
            PaywallPlanUi(
                id = plan.id,
                period = when (plan.period) {
                    Plan.Period.Monthly -> PaywallPlanPeriod.Monthly
                    Plan.Period.Yearly -> PaywallPlanPeriod.Yearly
                },
                price = plan.price,
                saving = plan.saving,
            )
        }
        val chosen = when (paywall.chosen) {
            Plan.Period.Monthly -> PaywallPlanPeriod.Monthly
            Plan.Period.Yearly -> PaywallPlanPeriod.Yearly
        }
        val phase: PaywallPhaseUi = when (val raw = paywall.phase) {
            is PaywallStore.Phase.Entitled -> {
                val source = when (val grant = raw.grant) {
                    Grant.Granted -> PaywallGrantSource.Granted
                    is Grant.Purchased -> when (grant.source) {
                        EntitlementSource.PlayStore -> PaywallGrantSource.PlayStore
                        EntitlementSource.AppStore -> PaywallGrantSource.AppStore
                        EntitlementSource.Web -> PaywallGrantSource.Web
                    }
                }
                val renewalText = (paywall.entitlement as? Entitlement.Active)?.renewsAtMs?.let { ms ->
                    formatRenews(formatLocalizedDate(ms))
                }
                PaywallPhaseUi.Entitled(source = source, renewalOrExpiryText = renewalText)
            }
            PaywallStore.Phase.Buying -> PaywallPhaseUi.Buying
            PaywallStore.Phase.AwaitingApproval -> PaywallPhaseUi.AwaitingApproval
            PaywallStore.Phase.Confirming -> PaywallPhaseUi.Confirming
            is PaywallStore.Phase.Unconfirmed -> PaywallPhaseUi.Unconfirmed(
                when (val why = raw.why) {
                    PaywallStore.Unconfirmed.Unreachable -> PaywallUnconfirmedReason.Unreachable
                    is PaywallStore.Unconfirmed.Refused -> PaywallUnconfirmedReason.Refused(why.reason)
                    PaywallStore.Unconfirmed.SwitchingOn -> PaywallUnconfirmedReason.SwitchingOn
                },
            )
            is PaywallStore.Phase.Failed -> {
                if (storeUnavailable && plans.isEmpty()) {
                    PaywallPhaseUi.StoreUnavailable(raw.reason)
                } else {
                    PaywallPhaseUi.Failed(raw.reason)
                }
            }
            PaywallStore.Phase.Ready -> {
                if (storeUnavailable && plans.isEmpty()) {
                    PaywallPhaseUi.StoreUnavailable(null)
                } else {
                    PaywallPhaseUi.Ready
                }
            }
        }
        return PaywallScreenState(
            plans = plans,
            chosen = chosen,
            phase = phase,
            manageUrl = paywall.manageUrl,
            privacyUrl = privacyUrl,
            accountEmail = accountEmail,
        )
    }

    fun toPlanPeriod(period: PaywallPlanPeriod): Plan.Period = when (period) {
        PaywallPlanPeriod.Monthly -> Plan.Period.Monthly
        PaywallPlanPeriod.Yearly -> Plan.Period.Yearly
    }

    /** A date as this phone writes dates, for renewals and billing warnings. */
    internal fun formatLocalizedDate(epochMs: Long): String =
        Instant.ofEpochMilli(epochMs)
            .atZone(ZoneId.systemDefault())
            .toLocalDate()
            .format(DateTimeFormatter.ofLocalizedDate(FormatStyle.MEDIUM))

}
