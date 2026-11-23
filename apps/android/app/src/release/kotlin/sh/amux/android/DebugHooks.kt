package sh.amux.android

import android.app.Activity
import android.content.Intent
import androidx.compose.runtime.Composable

internal object DebugHooks {
    val enabled: Boolean = false

    val compositionOverrides: CompositionOverrides? = null

    fun onApplicationCreated(application: AmuxApplication, composition: Composition) {
        // Release builds ship with no debug hooks.
    }

    fun onActivityCreated(activity: Activity, composition: Composition) {
        // Release builds ship with no debug hooks.
    }

    fun onNewIntent(activity: Activity, intent: Intent?) {
        // Release builds ship with no debug hooks.
    }

    fun onActivityDestroyed(activity: Activity) {
        // Release builds ship with no debug hooks.
    }

    @Composable
    fun ContentWrapper(content: @Composable () -> Unit) {
        content()
    }
}
