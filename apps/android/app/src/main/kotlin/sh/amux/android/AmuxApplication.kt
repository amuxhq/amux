package sh.amux.android

import android.app.Application
import android.os.Build
import sh.amux.core.runtime.NoOpNetworkWatcher

/**
 * Process-wide application owning the single [Composition] instance.
 */
class AmuxApplication : Application() {
    lateinit var composition: Composition
        private set

    override fun onCreate() {
        super.onCreate()
        val overrides = DebugHooks.compositionOverrides ?: defaultOverrides()
        composition = Composition(context = this, overrides = overrides)
        DebugHooks.onApplicationCreated(this, composition)
    }

    internal fun replaceCompositionForTest(newComposition: Composition) {
        composition = newComposition
    }

    private fun defaultOverrides(): CompositionOverrides {
        // Avoid spawning live NsdManager / ConnectivityManager / JNI runtime
        // threads during pure Robolectric JVM UI and storybook tests.
        val isRobolectric = Build.FINGERPRINT.equals("robolectric", ignoreCase = true)
        return if (isRobolectric) {
            CompositionOverrides(
                useSystemDiscovery = false,
                networkWatch = NoOpNetworkWatcher,
                autoStart = false,
                attachProcessLifecycle = false,
            )
        } else {
            CompositionOverrides()
        }
    }
}
