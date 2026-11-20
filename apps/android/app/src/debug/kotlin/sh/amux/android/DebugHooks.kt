package sh.amux.android

import android.app.Activity
import sh.amux.testsupport.TestSupport

internal object DebugHooks {
    val enabled: Boolean = TestSupport.enabled

    fun onActivityCreated(activity: Activity) {
        TestSupport.attach(activity)
    }
}
