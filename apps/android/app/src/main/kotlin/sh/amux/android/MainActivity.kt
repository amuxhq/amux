package sh.amux.android

import android.Manifest
import android.app.Activity
import android.content.Intent
import android.graphics.Color
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.SystemBarStyle
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import sh.amux.design.AmuxTheme
import sh.amux.design.AppearanceMode
import sh.amux.features.SettingsNotificationStatus
import sh.amux.features.StoreFailureScreen
import sh.amux.shell.Shell

class MainActivity : ComponentActivity() {
    private val composition: Composition
        get() = (application as AmuxApplication).composition

    private val notificationPermissionLauncher = registerForActivityResult(
        ActivityResultContracts.RequestPermission(),
    ) { _ ->
        composition.onNotificationPermissionResult(this)
    }

    internal val reportFreezer: ReportFreezer by lazy {
        ReportFreezer(
            activity = this,
            compositionProvider = { composition },
        )
    }

    internal val screenCaptureCallback = Activity.ScreenCaptureCallback {
        composition.report.offer(reportFreezer)
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        DebugHooks.onActivityCreated(this, composition)
        val appComposition = composition
        if (savedInstanceState == null) {
            handleIncomingIntent(intent)
        }

        setContent {
            val appearanceMode by appComposition.appearanceMode.collectAsState()
            val coordinatorState by appComposition.runtime.state.collectAsState()
            val activeStores by appComposition.runtime.storesFlow.collectAsState()
            val signInState by appComposition.signIn.state.collectAsState()
            val notificationStatus by appComposition.notificationStatus.collectAsState()
            val notificationsDismissed by appComposition.notificationsDismissed.collectAsState()
            val diagnosticsExport by appComposition.diagnosticsExport.collectAsState()

            val systemDark = isSystemInDarkTheme()
            val isDark = when (appearanceMode) {
                AppearanceMode.Dark -> true
                AppearanceMode.Light -> false
                AppearanceMode.System -> systemDark
            }
            LaunchedEffect(isDark) {
                val barStyle = if (isDark) {
                    SystemBarStyle.dark(Color.TRANSPARENT)
                } else {
                    SystemBarStyle.light(Color.TRANSPARENT, Color.TRANSPARENT)
                }
                enableEdgeToEdge(
                    statusBarStyle = barStyle,
                    navigationBarStyle = barStyle,
                )
            }

            DebugHooks.ContentWrapper {
                AmuxTheme(appearanceMode = appearanceMode) {
                    val storeFailure = coordinatorState.storeFailure
                    if (storeFailure != null) {
                        StoreFailureScreen(
                            message = storeFailure,
                            onRelaunch = { appComposition.relaunch() },
                        )
                    } else {
                        Shell(
                            router = appComposition.router,
                            accounts = appComposition.accounts,
                            stores = activeStores,
                            deviceName = appComposition.deviceName,
                            offerNotifications = !notificationsDismissed &&
                                notificationStatus != SettingsNotificationStatus.Enabled,
                            signingIn = signInState.working,
                            onAction = { action ->
                                appComposition.handleShellAction(
                                    action = action,
                                    activityContext = this@MainActivity,
                                    requestNotificationPermission = {
                                        notificationPermissionLauncher.launch(
                                            Manifest.permission.POST_NOTIFICATIONS,
                                        )
                                    },
                                    reportFreezer = reportFreezer,
                                )
                            },
                            appearanceMode = appearanceMode,
                            notificationStatus = notificationStatus,
                            shareUsage = coordinatorState.shareUsage,
                            autoReports = coordinatorState.autoReports,
                            appVersion = appComposition.appVersion,
                            diagnosticsExport = diagnosticsExport,
                            cloudEndpoint = appComposition.cloud.endpoint,
                            signIn = appComposition.signIn,
                            paywall = appComposition.paywall,
                            paywallStoreUnavailable = appComposition.paywallStoreUnavailable,
                            removal = appComposition.removal,
                            deletion = appComposition.deletion,
                            report = appComposition.report,
                            dictation = appComposition.dictation,
                        )
                    }
                }
            }
        }
    }

    override fun onStart() {
        super.onStart()
        composition.attachActivity(this)
        runCatching {
            registerScreenCaptureCallback(mainExecutor, screenCaptureCallback)
        }
    }

    override fun onResume() {
        super.onResume()
        composition.onActivityResumed(this)
    }

    override fun onStop() {
        runCatching {
            unregisterScreenCaptureCallback(screenCaptureCallback)
        }
        composition.onActivityStopped(this)
        super.onStop()
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        setIntent(intent)
        DebugHooks.onNewIntent(this, intent)
        handleIncomingIntent(intent)
    }

    override fun onDestroy() {
        DebugHooks.onActivityDestroyed(this)
        super.onDestroy()
    }

    private fun handleIncomingIntent(intent: Intent?) {
        val uri = intent?.data ?: return
        composition.handleIncomingUri(uri)
    }
}
