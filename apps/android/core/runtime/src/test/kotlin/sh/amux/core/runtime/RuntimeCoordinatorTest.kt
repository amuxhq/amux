package sh.amux.core.runtime

import java.nio.file.Files
import java.util.UUID
import java.util.concurrent.Executors
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.asCoroutineDispatcher
import kotlinx.coroutines.cancel
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Assume.assumeFalse
import org.junit.Test
import sh.amux.values.Found

class RuntimeCoordinatorTest {
    private val isWindows: Boolean =
        System.getProperty("os.name").orEmpty().lowercase().contains("win")

    private class FakeDiscovery : DiscoverySource {
        var starts = 0
        var stops = 0
        override fun start() { starts += 1 }
        override fun stop() { stops += 1 }
    }

    private class FakeNetworkWatch : NetworkWatcher {
        private var callback: (() -> Unit)? = null
        override fun start(onChange: () -> Unit) { callback = onChange }
        override fun stop() { callback = null }
        fun trigger() { callback?.invoke() }
    }

    @Test
    fun realEmbeddedRuntimeLifecycleDiscoveryProfilesAndCleanStop() {
        assumeFalse("wake_fd watcher requires Unix", isWindows)
        val tempDir = Files.createTempDirectory("amux-coordinator-e2e").toFile()
        val mainExecutor = Executors.newSingleThreadExecutor { r ->
            Thread(r, "test-main").apply { isDaemon = true }
        }
        val mainDispatcher = mainExecutor.asCoroutineDispatcher()
        val scope = CoroutineScope(SupervisorJob() + mainDispatcher)
        val dispatch: (() -> Unit) -> Unit = { block -> mainExecutor.execute(block) }

        val discovery = FakeDiscovery()
        val networkWatch = FakeNetworkWatch()
        val registry = AccountRegistry(
            makeStores = { id -> StoreBundle(account = id, scope = scope) },
        )
        val signedOut = StoreBundle(account = AccountId("signed-out"), scope = scope)
        val coordinator = RuntimeCoordinator(
            registry = registry,
            installationDir = tempDir,
            deviceName = "Android Coordinator Test",
            signedOutStores = signedOut,
            scope = scope,
            ioDispatcher = Dispatchers.IO,
            dispatch = dispatch,
            options = RuntimeCoordinator.Options(lan = false),
            networkWatch = networkWatch,
            discovery = discovery,
        )

        try {
            runBlocking(mainDispatcher) {
                coordinator.start()
                val runtime = withTimeout(10_000L) { coordinator.awaitStarted() }
                assertNotNull("embedded runtime should start cleanly", runtime)
                assertNull(coordinator.snapshot.storeFailure)

                // Wait for initial profile opening & fleet load via StateFlow.
                withTimeout(10_000L) {
                    signedOut.fleet.state.first { it.loaded }
                }

                assertEquals(1, coordinator.snapshot.profiles.size)
                val defaultProfileId = coordinator.snapshot.profiles.single().id
                assertEquals(defaultProfileId, coordinator.profile?.id)
                assertEquals("Android Coordinator Test", signedOut.hosts.snapshot.local?.name)
                assertTrue(runtime!!.listsSources(defaultProfileId))
                assertTrue(discovery.starts >= 1)

                // Feed a discovered LAN peer and verify it lands in HostsStore.discovered.
                val peerHostId = HostId(UUID.fromString("42424242-4242-4242-4242-424242424242"))
                coordinator.discovered(
                    listOf(
                        Found(
                            hostId = peerHostId.bytes,
                            name = "Studio Mac",
                            addrs = listOf("127.0.0.1:41990"),
                            scope = "",
                            pairingOpen = true,
                        ),
                    ),
                )
                withTimeout(10_000L) {
                    signedOut.hosts.state.first { hs ->
                        hs.discovered.any { it.id == peerHostId && it.windowOpen }
                    }
                }
                assertEquals(peerHostId, signedOut.hosts.snapshot.windowToOffer?.id)

                // Create a second profile and confirm profile list updates.
                val second = withTimeout(10_000L) { runtime.createProfile() }
                assertTrue("createProfile should succeed: $second", second.isSuccess)
                val secondProfileId = second.getOrThrow().id
                withTimeout(10_000L) {
                    coordinator.state.first { it.profiles.size == 2 }
                }
                assertTrue(withTimeout(10_000L) { runtime.deleteProfile(secondProfileId) }.isSuccess)
                withTimeout(10_000L) {
                    coordinator.state.first { it.profiles.size == 1 }
                }

                // Analytics toggles apply live.
                coordinator.setShareUsage(false)
                coordinator.setAutoReports(true)
                assertFalse(coordinator.snapshot.shareUsage)
                assertTrue(coordinator.snapshot.autoReports)

                // Ordered lifecycle: Foreground -> Background -> Suspending -> Foreground -> NetworkChanged.
                coordinator.setActive(false)
                assertFalse(runtime.listsSources(defaultProfileId))
                assertTrue(discovery.stops >= 1)

                withTimeout(10_000L) { coordinator.suspending() }
                coordinator.setActive(true)
                assertTrue(runtime.listsSources(defaultProfileId))

                networkWatch.trigger()
                val flushed = CompletableDeferred<Unit>()
                dispatch { flushed.complete(Unit) }
                withTimeout(5_000L) { flushed.await() }

                assertEquals(
                    listOf(
                        Lifecycle.Foreground,
                        Lifecycle.Background,
                        Lifecycle.Suspending,
                        Lifecycle.Foreground,
                        Lifecycle.NetworkChanged,
                    ),
                    coordinator.fedLifecycles,
                )

                coordinator.stop().join()
                assertNull(coordinator.runtime)
                assertNull(coordinator.profile)
            }
        } finally {
            runBlocking { coordinator.stop().join() }
            scope.cancel()
            mainExecutor.shutdownNow()
            tempDir.deleteRecursively()
        }
    }

    @Test
    fun storeFailureSurfacesAndRelaunchesCleanly() {
        assumeFalse("wake_fd watcher requires Unix", isWindows)
        val tempDir = Files.createTempDirectory("amux-coordinator-relaunch").toFile()
        val mainExecutor = Executors.newSingleThreadExecutor { r ->
            Thread(r, "test-relaunch-main").apply { isDaemon = true }
        }
        val mainDispatcher = mainExecutor.asCoroutineDispatcher()
        val scope = CoroutineScope(SupervisorJob() + mainDispatcher)
        val dispatch: (() -> Unit) -> Unit = { block -> mainExecutor.execute(block) }

        var failFirst = true
        val registry = AccountRegistry(
            makeStores = { id -> StoreBundle(account = id, scope = scope) },
        )
        val signedOut = StoreBundle(account = AccountId("signed-out"), scope = scope)
        val coordinator = RuntimeCoordinator(
            registry = registry,
            installationDir = tempDir,
            deviceName = "Relaunch Test",
            signedOutStores = signedOut,
            scope = scope,
            ioDispatcher = Dispatchers.IO,
            dispatch = dispatch,
            options = RuntimeCoordinator.Options(lan = false),
            starter = RuntimeStarter { config, disp, onProfilesMoved, onStopped ->
                if (failFirst) {
                    failFirst = false
                    throw RuntimeFailure("corrupt store")
                }
                Runtime.start(config, dispatch = disp, onProfilesMoved = onProfilesMoved, onStopped = onStopped)
            },
        )

        try {
            runBlocking(mainDispatcher) {
                coordinator.start()
                assertNull(withTimeout(10_000L) { coordinator.awaitStarted() })
                assertEquals("corrupt store", coordinator.snapshot.storeFailure)

                coordinator.relaunch()
                val recovered = withTimeout(10_000L) { coordinator.awaitStarted() }
                assertNotNull("relaunch should start embedded runtime", recovered)
                assertNull(coordinator.snapshot.storeFailure)

                coordinator.stop().join()
            }
        } finally {
            runBlocking { coordinator.stop().join() }
            scope.cancel()
            mainExecutor.shutdownNow()
            tempDir.deleteRecursively()
        }
    }

    @Test
    fun stopDuringInFlightStartCleansUpStartedRuntimeAndReleasesWaiters() {
        assumeFalse("wake_fd watcher requires Unix", isWindows)
        val tempDir = Files.createTempDirectory("amux-coordinator-cancel-start").toFile()
        val mainExecutor = Executors.newSingleThreadExecutor { r ->
            Thread(r, "test-cancel-main").apply { isDaemon = true }
        }
        val mainDispatcher = mainExecutor.asCoroutineDispatcher()
        val scope = CoroutineScope(SupervisorJob() + mainDispatcher)
        val dispatch: (() -> Unit) -> Unit = { block -> mainExecutor.execute(block) }

        val enteredStart = CompletableDeferred<Unit>()
        val releaseStart = java.util.concurrent.CountDownLatch(1)
        val createdRef = java.util.concurrent.atomic.AtomicReference<Runtime?>(null)

        val registry = AccountRegistry(
            makeStores = { id -> StoreBundle(account = id, scope = scope) },
        )
        val signedOut = StoreBundle(account = AccountId("signed-out"), scope = scope)
        val coordinator = RuntimeCoordinator(
            registry = registry,
            installationDir = tempDir,
            deviceName = "Cancel Start Test",
            signedOutStores = signedOut,
            scope = scope,
            ioDispatcher = Dispatchers.IO,
            dispatch = dispatch,
            options = RuntimeCoordinator.Options(lan = false),
            starter = RuntimeStarter { config, disp, onProfilesMoved, onStopped ->
                enteredStart.complete(Unit)
                releaseStart.await(5, java.util.concurrent.TimeUnit.SECONDS)
                Runtime.start(config, dispatch = disp, onProfilesMoved = onProfilesMoved, onStopped = onStopped)
                    .also { createdRef.set(it) }
            },
        )

        try {
            runBlocking(mainDispatcher) {
                coordinator.start()
                withTimeout(5_000L) { enteredStart.await() }

                // Stop while starter.start() is still blocked on IO.
                coordinator.stop()
                assertNull(coordinator.runtime)

                // Let starter.start() return a real Runtime after cancellation.
                releaseStart.countDown()

                // Wait until the orphan runtime is created and stopped by finally.
                val stopped = withTimeout(5_000L) {
                    while (true) {
                        val created = createdRef.get()
                        if (created != null && created.createProfile().isFailure) {
                            return@withTimeout true
                        }
                        kotlinx.coroutines.yield()
                    }
                    @Suppress("UNREACHABLE_CODE")
                    false
                }
                assertNotNull("starter should have returned a runtime", createdRef.get())
                assertTrue("orphan runtime must be stopped immediately after cancelled startup", stopped)
                assertNull(coordinator.runtime)
            }
        } finally {
            releaseStart.countDown()
            runBlocking { coordinator.stop().join() }
            scope.cancel()
            mainExecutor.shutdownNow()
            tempDir.deleteRecursively()
        }
    }

    @Test
    fun aRuntimeThatStopsByItselfRaisesTheStoreFailureAndClearsTheRuntime() {
        val tempDir = Files.createTempDirectory("amux-coordinator-died").toFile()
        val mainExecutor = Executors.newSingleThreadExecutor { r ->
            Thread(r, "test-died-main").apply { isDaemon = true }
        }
        val mainDispatcher = mainExecutor.asCoroutineDispatcher()
        val scope = CoroutineScope(SupervisorJob() + mainDispatcher)
        val dispatch: (() -> Unit) -> Unit = { block -> mainExecutor.execute(block) }

        val die = java.util.concurrent.CountDownLatch(1)
        val stops = java.util.concurrent.atomic.AtomicInteger(0)
        val bridge = object : CoreLoopBridge {
            override fun runtimeStart(configJson: String): Long = 4242L
            override fun runtimeStop(handle: Long) {
                stops.incrementAndGet()
            }
            override fun runtimeWaitWake(handle: Long, timeoutMs: Int): Int {
                die.await(10, java.util.concurrent.TimeUnit.SECONDS)
                throw BridgeException("the runtime went away")
            }
            override fun runtimeInterruptWake(handle: Long) = die.countDown()
            override fun runtimeStep(handle: Long): String? = null
        }

        val registry = AccountRegistry(makeStores = { id -> StoreBundle(account = id, scope = scope) })
        val coordinator = RuntimeCoordinator(
            registry = registry,
            installationDir = tempDir,
            deviceName = "Died Test",
            signedOutStores = StoreBundle(account = AccountId("signed-out"), scope = scope),
            scope = scope,
            ioDispatcher = Dispatchers.IO,
            dispatch = dispatch,
            options = RuntimeCoordinator.Options(lan = false),
            starter = RuntimeStarter { config, disp, onProfilesMoved, onStopped ->
                Runtime.start(
                    config,
                    CoreLoop(
                        dispatch = disp,
                        onProfilesMoved = onProfilesMoved,
                        onStopped = onStopped,
                        bridge = bridge,
                    ),
                )
            },
        )

        try {
            runBlocking(mainDispatcher) {
                coordinator.start()
                assertNotNull(withTimeout(10_000L) { coordinator.awaitStarted() })
                assertNull(coordinator.snapshot.storeFailure)

                die.countDown()
                val failed = withTimeout(10_000L) {
                    coordinator.state.first { it.storeFailure != null }
                }
                assertEquals("the runtime went away", failed.storeFailure)
                assertNull("a stopped runtime is let go", coordinator.runtime)
                assertNull(coordinator.profile)
                assertEquals(1, stops.get())
            }
        } finally {
            runBlocking { coordinator.stop().join() }
            scope.cancel()
            mainExecutor.shutdownNow()
            tempDir.deleteRecursively()
        }
    }
}
