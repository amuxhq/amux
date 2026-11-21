package sh.amux.core.runtime

import java.util.concurrent.CompletableFuture
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

        val stoppedBecause = CompletableFuture<String>()
        val loop = CoreLoop(
            onError = { err -> reportedError.compareAndSet(null, err) },
            onStopped = { reason -> stoppedBecause.complete(reason) },
            bridge = bridge,
        )
        assertEquals(77L, loop.start(dummyConfig))

        assertTrue("fatal step error must release native runtime", stoppedLatch.await(5, TimeUnit.SECONDS))
        assertEquals("native runtime panicked", stoppedBecause.get(5, TimeUnit.SECONDS))
        loop.awaitWatcherExit(5_000L)
        assertTrue(reportedError.get() is BridgeException)
        assertEquals(0L, loop.handle)
        assertFalse(loop.isRunning())
        assertTrue(loop.isExecutorShutdown)
        assertEquals(1, stopCalls.get())

        loop.stop()
        assertEquals(1, stopCalls.get())
    }

    @Test
    fun aStepWhoseDeltasCannotBeReadStillAnswersItsTicketsAndTheLoopGoesOn() {
        val firstWake = CountDownLatch(1)
        val secondWake = CountDownLatch(1)
        val stopRang = CountDownLatch(1)
        val waits = AtomicInteger(0)
        val steps = AtomicInteger(0)
        val errors = mutableListOf<Throwable>()
        val stopped = AtomicReference<String?>(null)

        val bridge = object : CoreLoopBridge {
            override fun runtimeStart(configJson: String): Long = 5L
            override fun runtimeStop(handle: Long) = Unit

            override fun runtimeWaitWake(handle: Long, timeoutMs: Int): Int {
                val gate = when (waits.incrementAndGet()) {
                    1 -> firstWake
                    2 -> secondWake
                    else -> stopRang
                }
                assertTrue(gate.await(5, TimeUnit.SECONDS))
                return 1
            }

            override fun runtimeInterruptWake(handle: Long) {
                stopRang.countDown()
            }

            override fun runtimeStep(handle: Long): String? = when (steps.incrementAndGet()) {
                // The step start() takes.
                1 -> null
                // A fleet delta this build cannot read, beside an answer it can.
                2 -> """{"fleets":[{"fleet":1,"delta":{"hosts":"not a list"}}],""" +
                    """"answers":[{"ticket":99,"json":"{\"Ok\":null}"}]}"""
                else -> """{"answers":[{"ticket":100,"json":"{\"Ok\":1}"}]}"""
            }
        }

        val loop = CoreLoop(
            onError = { synchronized(errors) { errors.add(it) } },
            onStopped = { stopped.set(it) },
            bridge = bridge,
        )
        loop.start(dummyConfig)

        val first = loop.submitAct { 99L }
        firstWake.countDown()
        assertEquals("""{"Ok":null}""", first.get(5, TimeUnit.SECONDS))
        assertTrue(loop.isRunning())
        assertTrue(synchronized(errors) { errors.single() } is CoreLoop.UnreadableStep)

        // The loop steps again after the step it skipped.
        val second = loop.submitAct { 100L }
        secondWake.countDown()
        assertEquals("""{"Ok":1}""", second.get(5, TimeUnit.SECONDS))

        loop.stop()
        assertFalse(loop.isRunning())
        assertEquals("a stop asked for is not a stop by itself", null, stopped.get())
    }

    @Test
    fun aStepThatCannotBeReadAtAllAnswersEveryWaitingAct() {
        val wake = CountDownLatch(1)
        val stopRang = CountDownLatch(1)
        val waits = AtomicInteger(0)
        val steps = AtomicInteger(0)

        val bridge = object : CoreLoopBridge {
            override fun runtimeStart(configJson: String): Long = 6L
            override fun runtimeStop(handle: Long) = Unit

            override fun runtimeWaitWake(handle: Long, timeoutMs: Int): Int {
                val gate = if (waits.incrementAndGet() == 1) wake else stopRang
                assertTrue(gate.await(5, TimeUnit.SECONDS))
                return 1
            }

            override fun runtimeInterruptWake(handle: Long) {
                stopRang.countDown()
            }

            override fun runtimeStep(handle: Long): String? = when (steps.incrementAndGet()) {
                1 -> null
                2 -> """{"answers":[{"ticket":7,"json":"""
                else -> null
            }
        }

        val loop = CoreLoop(onError = {}, bridge = bridge)
        loop.start(dummyConfig)

        val waiting = listOf(loop.submitAct { 7L }, loop.submitAct { 8L })
        // Both tickets are taken on the core thread before the step runs.
        loop.submitAct { 0L }.get(5, TimeUnit.SECONDS)
        wake.countDown()
        for (act in waiting) {
            assertEquals(CoreLoop.UNREADABLE_ANSWER, act.get(5, TimeUnit.SECONDS))
        }
        assertTrue(loop.isRunning())
        loop.stop()
    }
}
