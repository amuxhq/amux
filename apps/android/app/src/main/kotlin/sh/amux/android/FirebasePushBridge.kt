package sh.amux.android

import android.content.Context
import com.google.firebase.FirebaseApp
import com.google.firebase.messaging.FirebaseMessaging

/**
 * Isolates FirebaseApp / FirebaseMessaging SDK calls so builds without
 * `google-services.json` (CI, Robolectric tests) degrade cleanly to
 * [sh.amux.core.runtime.NoOpPushRegistrar] instead of crashing at startup.
 */
internal object FirebasePushBridge {
    fun isAvailable(context: Context): Boolean = runCatching {
        val appContext = context.applicationContext
        FirebaseApp.getApps(appContext).isNotEmpty() || FirebaseApp.initializeApp(appContext) != null
    }.getOrDefault(false)

    @Suppress("DEPRECATION")
    fun fetchToken(onToken: (String) -> Unit) {
        runCatching {
            FirebaseMessaging.getInstance().token.addOnSuccessListener { token ->
                if (!token.isNullOrBlank()) {
                    onToken(token)
                }
            }
        }
    }
}
