package sh.amux.core.runtime

import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger
import java.util.concurrent.atomic.AtomicReference
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import sh.amux.values.StartConfig

class CoreLoopTest {
    private val dummyConfig = StartConfig(
        dataDir = "/tmp/amux-coreloop-test",
        deviceName = "CoreLoop Test",
        lan = false,
        telemetry = false,
        autoReports = false,
    )

    @Test
    fun fatalWatcherWaitErrorReleasesRuntimeAndShutsDownExecutorIdempotently() {
        val stopCalls = AtomicInteger(0)
        val stoppedLatch = CountDownLatch(1)
        val waitRelease = CountDownLatch(1)
        val reportedError = AtomicReference<Throwable?>(null)

        val bridge = object : CoreLoopBridge {
            override fun runtimeStart(configJson: String): Long = 42L

            override fun runtimeStop(handle: Long) {
                assertEquals(42L, handle)
                stopCalls.incrementAndGet()
                stoppedLatch.countDown()
            }

            override fun runtimeWaitWake(handle: Long, timeoutMs: Int): Int {
                waitRelease.await(5, TimeUnit.SECONDS)
                throw BridgeException("wake fd failed")
            }

            override fun runtimeInterruptWake(handle: Long) {
                waitRelease.countDown()
            }

            override fun runtimeStep(handle: Long): String? = null
        }

        val loop = CoreLoop(
            onError = { err -> reportedError.compareAndSet(null, err) },
            bridge = bridge,
        )
        assertEquals(42L, loop.start(dummyConfig))
        assertTrue(loop.isRunning())

        val pendingAct = loop.submitAct { 99L }

        waitRelease.countDown()
        assertTrue("fatal watcher exit must release native runtime", stoppedLatch.await(5, TimeUnit.SECONDS))
        loop.awaitWatcherExit(5_000L)

        val answer = pendingAct.get(5, TimeUnit.SECONDS)
        assertEquals(CoreLoop.STOPPED_ANSWER, answer)
        assertTrue(reportedError.get() is BridgeException)
        assertEquals(0L, loop.handle)
        assertFalse(loop.isRunning())
        assertTrue("executor must shut down on fatal watcher exit", loop.isExecutorShutdown)
        assertEquals(1, stopCalls.get())

        // Subsequent stop() must be a clean idempotent no-op without double-stopping.
        loop.stop()
        assertEquals(1, stopCalls.get())
        assertEquals(0L, loop.handle)
        assertFalse(loop.isRunning())
    }

    @Test
    fun fatalStepBridgeExceptionReleasesRuntimeAndShutsDownExecutor() {
        val stopCalls = AtomicInteger(0)
        val stoppedLatch = CountDownLatch(1)
        val stepCalls = AtomicInteger(0)
        val wakeCount = AtomicInteger(0)
        val reportedError = AtomicReference<Throwable?>(null)

        val bridge = object : CoreLoopBridge {
            override fun runtimeStart(configJson: String): Long = 77L

            override fun runtimeStop(handle: Long) {
                assertEquals(77L, handle)
                stopCalls.incrementAndGet()
                stoppedLatch.countDown()
            }

            override fun runtimeWaitWake(handle: Long, timeoutMs: Int): Int {
                return if (wakeCount.incrementAndGet() == 1) 1 else -1
            }

            override fun runtimeInterruptWake(handle: Long) = Unit

            override fun runtimeStep(handle: Long): String? {
                // Initial step in start() succeeds; watcher-triggered step fails fatally.
                if (stepCalls.incrementAndGet() == 1) {
                    return null
                }
                throw BridgeException("native runtime panicked")
            }
        }

        val loop = CoreLoop(
            onError = { err -> reportedError.compareAndSet(null, err) },
            bridge = bridge,
        )
        assertEquals(77L, loop.start(dummyConfig))

        assertTrue("fatal step error must release native runtime", stoppedLatch.await(5, TimeUnit.SECONDS))
        loop.awaitWatcherExit(5_000L)
        assertTrue(reportedError.get() is BridgeException)
        assertEquals(0L, loop.handle)
        assertFalse(loop.isRunning())
        assertTrue(loop.isExecutorShutdown)
        assertEquals(1, stopCalls.get())

        loop.stop()
        assertEquals(1, stopCalls.get())
    }
}
