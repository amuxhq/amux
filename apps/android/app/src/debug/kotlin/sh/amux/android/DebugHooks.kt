package sh.amux.android

import android.app.Activity
import android.content.Intent
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.unit.Density
import sh.amux.testsupport.TestSupport

internal object DebugHooks {
    val enabled: Boolean = TestSupport.enabled

    @Volatile
    var compositionOverrides: CompositionOverrides? = null

    fun onApplicationCreated(application: AmuxApplication, composition: Composition) {
        @Suppress("UNUSED_VARIABLE")
        val ignoredApp = application
        @Suppress("UNUSED_VARIABLE")
        val ignoredComposition = composition
    }

    fun onActivityCreated(activity: Activity, composition: Composition) {
        TestSupport.attach(activity)
        DoorHost.attach(activity, composition)
    }

    fun onNewIntent(activity: Activity, intent: Intent?) {
        DoorHost.onNewIntent(activity, intent)
    }

    fun onActivityDestroyed(activity: Activity) {
        DoorHost.detach(activity)
    }

    @Composable
    fun ContentWrapper(content: @Composable () -> Unit) {
        val fontScale by DoorHost.fontScaleOverride.collectAsState()
        val overrideScale = fontScale
        if (overrideScale != null) {
            val baseDensity = LocalDensity.current
            CompositionLocalProvider(
                LocalDensity provides Density(
                    density = baseDensity.density,
                    fontScale = overrideScale,
                ),
            ) {
                content()
            }
        } else {
            content()
        }
    }
}
