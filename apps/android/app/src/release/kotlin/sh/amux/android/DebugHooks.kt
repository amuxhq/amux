package sh.amux.android

import android.app.Activity

internal object DebugHooks {
    val enabled: Boolean = false

    val compositionOverrides: CompositionOverrides? = null

    fun onApplicationCreated(application: AmuxApplication, composition: Composition) {
        // Release builds ship with no debug hooks.
    }

    fun onActivityCreated(activity: Activity, composition: Composition) {
        // Release builds ship with no debug hooks.
    }
}
