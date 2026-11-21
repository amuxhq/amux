package sh.amux.android

import android.content.Intent
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import sh.amux.design.AmuxTheme
import sh.amux.features.StoreFailureScreen
import sh.amux.shell.Shell

class MainActivity : ComponentActivity() {
    private val composition: Composition
        get() = (application as AmuxApplication).composition

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        DebugHooks.onActivityCreated(this, composition)
        val appComposition = composition
        handleIncomingIntent(intent)

        setContent {
            val appearanceMode by appComposition.appearanceMode.collectAsState()
            val coordinatorState by appComposition.runtime.state.collectAsState()
            val activeStores by appComposition.runtime.storesFlow.collectAsState()
            val signInState by appComposition.signIn.state.collectAsState()

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
                            signingIn = signInState.working,
                            onAction = { action ->
                                appComposition.handleShellAction(action, this@MainActivity)
                            },
                        )
                    }
                }
            }
        }
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
        val data = intent?.dataString ?: return
        composition.handleDeepLink(data)
    }
}
