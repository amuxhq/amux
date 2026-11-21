package sh.amux.android

import android.app.Activity
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
        @Suppress("UNUSED_VARIABLE")
        val ignoredComposition = composition
        TestSupport.attach(activity)
    }
}
