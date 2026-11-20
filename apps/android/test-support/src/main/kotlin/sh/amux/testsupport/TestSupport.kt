package sh.amux.testsupport

import android.app.Activity

/**
 * Entry point for debug-only test harnesses and driving hooks.
 * Never linked into release builds.
 */
object TestSupport {
    const val enabled: Boolean = true

    fun attach(activity: Activity) {
        // Driving door and debug hooks attach here in debug builds.
        @Suppress("UNUSED_VARIABLE")
        val ignored = activity
    }
}
