package sh.amux.android

import android.app.Activity
import java.io.File
import java.nio.file.Files
import kotlinx.coroutines.CompletableDeferred
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.Robolectric
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config
import sh.amux.core.runtime.AccountId
import sh.amux.core.runtime.Entitlement
import sh.amux.core.runtime.EntitlementSource
import sh.amux.core.runtime.FrozenFrame
import sh.amux.core.runtime.Grant
import sh.amux.core.runtime.PaywallStore
import sh.amux.core.runtime.ReportCapture
import sh.amux.core.runtime.SignInIntent
import sh.amux.core.runtime.StubBillingStoreFront
import sh.amux.features.DiagnosticsExportState
import sh.amux.shell.DictationController
import sh.amux.shell.ShellAction
import sh.amux.testsupport.CloudCall
import sh.amux.testsupport.ScriptedCloudService
import sh.amux.testsupport.ScriptedCloudState
import sh.amux.testsupport.ScriptedWebAuth
import sh.amux.values.AccountBinding
import sh.amux.values.AccountView
import sh.amux.values.ProfileView
import sh.amux.values.RelayLink

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class CompositionTest {
    private class FakeDictation : DictationController {
        override val isAvailable: Boolean = true
        var stopCount: Int = 0
        var cancelCount: Int = 0
        var destroyCount: Int = 0

        override fun start(onPartial: (String) -> Unit, onFinal: (String) -> Unit, onError: () -> Unit) = Unit
        override fun stop() {
            stopCount += 1
        }
        override fun cancel() {
            cancelCount += 1
        }
        override fun destroy() {
            destroyCount += 1
        }
    }

    private fun testComposition(
        cloud: ScriptedCloudService = ScriptedCloudService(),
        dictation: DictationController = FakeDictation(),
        shareFile: ((android.content.Context, File) -> Result<Unit>)? = null,
    ): Pair<Composition, File> {
        val tempRoot = Files.createTempDirectory("amux-comp-test").toFile()
        val app = RuntimeEnvironment.getApplication()
        val comp = Composition(
            context = app,
            overrides = CompositionOverrides(
                cloud = cloud,
                billing = StubBillingStoreFront(),
                authPresenter = ScriptedWebAuth(),
                dictation = dictation,
                useSystemDiscovery = false,
                useSystemPush = false,
                autoStart = false,
                attachProcessLifecycle = false,
                installationDir = File(tempRoot, "install"),
                accountsFile = File(tempRoot, "accounts.json"),
                prefsName = "amux_test_${tempRoot.name}",
                shareFile = shareFile,
            ),
        )
        return comp to tempRoot
    }

    private fun seedTwoAccounts(comp: Composition) {
        comp.accounts.show(
            listOf(
                ProfileView(
                    id = "prof-a",
                    label = "Ada",
                    subject = "sub-a",
                    account = AccountView(
                        binding = AccountBinding.SignedIn,
                        email = "ada@example.com",
                        name = "Ada",
                        picture = "",
                        pro = true,
                        relay = RelayLink.Connected,
                    ),
                ),
                ProfileView(
                    id = "prof-b",
                    label = "Grace",
                    subject = "sub-b",
                    account = AccountView(
                        binding = AccountBinding.SignedIn,
                        email = "grace@example.com",
                        name = "Grace",
                        picture = "",
                        pro = false,
                        relay = RelayLink.Connected,
                    ),
                ),
            ),
        )
    }

    @Test
    fun signOutAndConfirmRemoveAccountCallCloudForgetSession() {
        val cloud = ScriptedCloudService()
        val (comp, dir) = testComposition(cloud = cloud)
        val app = RuntimeEnvironment.getApplication()
        try {
            seedTwoAccounts(comp)

            comp.handleShellAction(ShellAction.SignOut(AccountId("sub-a")), app)
            shadowOf(app.mainLooper).idle()
            assertTrue(CloudCall.ForgetSession(AccountId("sub-a")) in cloud.calls)

            comp.handleShellAction(ShellAction.ConfirmRemoveAccount(AccountId("sub-b")), app)
            shadowOf(app.mainLooper).idle()
            assertTrue(CloudCall.ForgetSession(AccountId("sub-b")) in cloud.calls)
        } finally {
            comp.shutdown()
            dir.deleteRecursively()
        }
    }

    @Test
    fun paywallPreservesEntitledUnderStubBillingAndResetsForFreeAccount() {
        val proEntitlement = Entitlement.Active(
            grant = Grant.Purchased(EntitlementSource.Web),
            renewsAtMs = 1_800_000_000_000L,
        )
        val cloud = ScriptedCloudService(ScriptedCloudState(entitlement = proEntitlement))
        val (comp, dir) = testComposition(cloud = cloud)
        val app = RuntimeEnvironment.getApplication()
        try {
            seedTwoAccounts(comp)
            comp.accounts.select(AccountId("sub-a"))
            comp.accounts.entitlement(proEntitlement, forAccount = AccountId("sub-a"))

            comp.handleShellAction(ShellAction.Subscribe, app)
            shadowOf(app.mainLooper).idle()
            assertEquals(
                PaywallStore.Phase.Entitled(Grant.Purchased(EntitlementSource.Web)),
                comp.paywall.snapshot.phase,
            )

            // Switching to free account sub-b and opening Paywall must clear stale Entitled phase.
            cloud.scripted = ScriptedCloudState(entitlement = Entitlement.None)
            comp.handleShellAction(ShellAction.SelectAccount(AccountId("sub-b")), app)
            comp.handleShellAction(ShellAction.Subscribe, app)
            shadowOf(app.mainLooper).idle()
            assertFalse(comp.paywall.snapshot.phase is PaywallStore.Phase.Entitled)
        } finally {
            comp.shutdown()
            dir.deleteRecursively()
        }
    }

    @Test
    fun homeSignInRoutesExpiredAccountThroughReturningIntent() {
        val cloud = ScriptedCloudService(
            ScriptedCloudState(signIn = ScriptedCloudState.SignInOutcome.Cancelled),
        )
        val (comp, dir) = testComposition(cloud = cloud)
        val app = RuntimeEnvironment.getApplication()
        try {
            seedTwoAccounts(comp)
            comp.accounts.select(AccountId("sub-a"))
            comp.runtime.stores.fleet.relay(RelayLink.SignInAgain)

            comp.handleShellAction(ShellAction.SignIn, app)
            shadowOf(app.mainLooper).idle()

            val signInCall = cloud.calls.filterIsInstance<CloudCall.SignIn>().single()
            assertTrue("expected Returning intent for expired account", signInCall.intent is SignInIntent.Returning)
            assertEquals("ada@example.com", (signInCall.intent as SignInIntent.Returning).account.email)

            // AddAccount always uses SignInIntent.Adding.
            cloud.reset()
            comp.handleShellAction(ShellAction.AddAccount, app)
            shadowOf(app.mainLooper).idle()
            val addCall = cloud.calls.filterIsInstance<CloudCall.SignIn>().single()
            assertEquals(SignInIntent.Adding, addCall.intent)
        } finally {
            comp.shutdown()
            dir.deleteRecursively()
        }
    }

    @Test
    fun dismissNotificationsPersistsAndAccountSwitchClearsPendingReportOffer() {
        val (comp, dir) = testComposition()
        val app = RuntimeEnvironment.getApplication()
        try {
            seedTwoAccounts(comp)
            comp.accounts.select(AccountId("sub-a"))

            assertFalse(comp.notificationsDismissed.value)
            comp.handleShellAction(ShellAction.DismissNotifications, app)
            assertTrue(comp.notificationsDismissed.value)

            // Offer captured under sub-a must be dismissed when switching to sub-b.
            val capture = ReportCapture(
                frame = FrozenFrame(byteArrayOf(1, 2, 3), 400.0, 800.0, 2.0),
                dump = CompletableDeferred(Result.failure(RuntimeException("no dump"))),
                route = "agents",
                account = AccountId("sub-a"),
            )
            assertTrue(comp.report.offer { capture })
            assertTrue(comp.report.state.value.offering)

            comp.handleShellAction(ShellAction.SelectAccount(AccountId("sub-b")), app)
            assertFalse(comp.report.state.value.offering)
            assertNull(comp.report.state.value.capture)
        } finally {
            comp.shutdown()
            dir.deleteRecursively()
        }
    }

    @Test
    fun exportDiagnosticsPropagatesShareFailureAndClearsOnExit() {
        val (comp, dir) = testComposition(
            shareFile = { _, _ -> Result.failure(IllegalStateException("chooser unavailable")) },
        )
        val app = RuntimeEnvironment.getApplication()
        try {
            comp.handleShellAction(ShellAction.ExportDiagnostics, app)
            shadowOf(app.mainLooper).idle()

            // Dump fails because runtime was not started (or share fails) -> must be Failed, never Exported.
            assertTrue(
                "expected Failed state when dump/share fails, got ${comp.diagnosticsExport.value}",
                comp.diagnosticsExport.value is DiagnosticsExportState.Failed,
            )

            comp.handleShellAction(ShellAction.ClearDiagnosticsExport, app)
            assertEquals(DiagnosticsExportState.Idle, comp.diagnosticsExport.value)
        } finally {
            comp.shutdown()
            dir.deleteRecursively()
        }
    }

    @Test
    fun onActivityStoppedCancelsActiveDictation() {
        val dictation = FakeDictation()
        val (comp, dir) = testComposition(dictation = dictation)
        val activity = Robolectric.buildActivity(Activity::class.java).create().get()
        try {
            comp.attachActivity(activity)
            comp.onActivityStopped(activity)
            assertEquals(1, dictation.cancelCount)
            assertEquals(0, dictation.stopCount)
        } finally {
            comp.shutdown()
            dir.deleteRecursively()
        }
    }

    @Test
    fun pushDeliveryEnabledReflectsWhetherPushRegistrarIsNoOp() {
        val (defaultComp, defaultDir) = testComposition()
        try {
            assertFalse(
                "default NoOpPushRegistrar must report pushDeliveryEnabled = false",
                defaultComp.pushDeliveryEnabled,
            )
        } finally {
            defaultComp.shutdown()
            defaultDir.deleteRecursively()
        }

        val tempRoot = Files.createTempDirectory("amux-comp-push-test").toFile()
        val app = RuntimeEnvironment.getApplication()
        val customPush = object : sh.amux.core.runtime.PushRegistrar {
            override fun updateAccounts(accounts: List<sh.amux.core.runtime.PushAccount>) = Unit
            override fun updatePermitted(permitted: Boolean) = Unit
            override fun updateToken(token: ByteArray) = Unit
            override suspend fun withdraw(account: AccountId) = Unit
            override suspend fun settled() = Unit
        }
        val customComp = Composition(
            context = app,
            overrides = CompositionOverrides(
                cloud = ScriptedCloudService(),
                billing = StubBillingStoreFront(),
                pushRegistrar = customPush,
                authPresenter = ScriptedWebAuth(),
                dictation = FakeDictation(),
                useSystemDiscovery = false,
                autoStart = false,
                attachProcessLifecycle = false,
                installationDir = File(tempRoot, "install"),
                accountsFile = File(tempRoot, "accounts.json"),
                prefsName = "amux_test_${tempRoot.name}",
            ),
        )
        try {
            assertTrue(
                "custom PushRegistrar should enable push delivery UI",
                customComp.pushDeliveryEnabled,
            )
        } finally {
            customComp.shutdown()
            tempRoot.deleteRecursively()
        }
    }
}
