package sh.amux.core.runtime

import androidx.lifecycle.DefaultLifecycleObserver
import androidx.lifecycle.Lifecycle as AndroidLifecycle
import androidx.lifecycle.LifecycleOwner
import androidx.lifecycle.ProcessLifecycleOwner
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.launch

/**
 * Bridges Android's [ProcessLifecycleOwner] into [RuntimeCoordinator]'s ordered
 * `Foreground`, `Background`, and `Suspending` lifecycle inputs.
 */
class AppLifecycleObserver(
    private val scope: CoroutineScope,
    private val onForeground: () -> Unit,
    private val onBackground: () -> Unit,
    private val onSuspending: suspend () -> Unit,
) : DefaultLifecycleObserver {
    constructor(
        coordinator: RuntimeCoordinator,
        scope: CoroutineScope,
    ) : this(
        scope = scope,
        onForeground = { coordinator.setActive(true) },
        onBackground = { coordinator.setActive(false) },
        onSuspending = { coordinator.suspending() },
    )

    override fun onStart(owner: LifecycleOwner) {
        onForeground()
    }

    override fun onStop(owner: LifecycleOwner) {
        onBackground()
        scope.launch {
            onSuspending()
        }
    }

    fun attach(lifecycle: AndroidLifecycle = ProcessLifecycleOwner.get().lifecycle) {
        lifecycle.addObserver(this)
    }

    fun detach(lifecycle: AndroidLifecycle = ProcessLifecycleOwner.get().lifecycle) {
        lifecycle.removeObserver(this)
    }
}
