package sh.amux.android

import android.app.Activity

internal object DebugHooks {
    val enabled: Boolean = false

    fun onActivityCreated(activity: Activity) {
        // Release builds ship with no debug hooks.
    }
}
