package sh.amux.shell

import android.content.Intent
import android.net.Uri
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.async
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class OAuthBrowserTest {
    private class ManualScheduler {
        data class Entry(
            val delayMs: Long,
            val action: () -> Unit,
            var cancelled: Boolean = false,
        )

        val entries = mutableListOf<Entry>()

        fun schedule(delayMs: Long, action: () -> Unit): () -> Unit {
            val entry = Entry(delayMs, action)
            entries += entry
            return { entry.cancelled = true }
        }

        fun fireActive() {
            val snapshot = entries.toList()
            for (entry in snapshot) {
                if (!entry.cancelled) {
                    entry.cancelled = true
                    entry.action()
                }
            }
        }
    }

    @Test
    fun deliversMatchingCallbackUriToPendingSession() = runTest {
        val launched = mutableListOf<Uri>()
        val scheduler = ManualScheduler()
        val browser = OAuthBrowser(
            callbackHost = "callback",
            resumeGraceMs = 350L,
            launchUrl = { launched += it },
            scheduleGraceCheck = scheduler::schedule,
        )

        val deferred = async(start = CoroutineStart.UNDISPATCHED) {
            browser.present(
                url = "https://amux.sh/connect/authorize?client_id=mobile&state=abc",
                callbackScheme = "amux",
            )
        }

        assertEquals(1, launched.size)
        assertEquals("https://amux.sh/connect/authorize?client_id=mobile&state=abc", launched.single().toString())
        assertTrue(browser.hasPendingSession)

        val callback = Uri.parse("amux://callback?code=auth-123&state=abc")
        assertTrue(browser.handleCallbackUri(callback))
        assertEquals(callback.toString(), deferred.await())
        assertFalse(browser.hasPendingSession)
    }

    @Test
    fun filtersWrongSchemeWrongHostAndIdleCallbacks() = runTest {
        val scheduler = ManualScheduler()
        val browser = OAuthBrowser(
            callbackHost = "callback",
            resumeGraceMs = 350L,
            launchUrl = {},
            scheduleGraceCheck = scheduler::schedule,
        )

        assertFalse(browser.handleCallbackUri(Uri.parse("amux://callback?code=none")))

        val deferred = async(start = CoroutineStart.UNDISPATCHED) {
            browser.present(
                url = "https://amux.sh/connect/authorize?state=xyz",
                callbackScheme = "amux",
            )
        }

        assertFalse(browser.handleCallbackUri(Uri.parse("https://amux.sh/p#invite")))
        assertFalse(browser.handleCallbackUri(Uri.parse("amux://other?code=1")))
        assertFalse(browser.handleCallbackUri(Uri.parse("other://callback?code=1")))
        assertTrue(browser.hasPendingSession)

        assertTrue(browser.handleCallbackUri(Uri.parse("AMUX://CALLBACK?code=ok&state=xyz")))
        assertEquals("AMUX://CALLBACK?code=ok&state=xyz", deferred.await())
    }

    @Test
    fun newSignInReplacesAndCancelsPriorPendingSession() = runTest {
        val launched = mutableListOf<Uri>()
        val scheduler = ManualScheduler()
        val browser = OAuthBrowser(
            callbackHost = "callback",
            resumeGraceMs = 350L,
            launchUrl = { launched += it },
            scheduleGraceCheck = scheduler::schedule,
        )

        val first = async(start = CoroutineStart.UNDISPATCHED) {
            browser.present("https://amux.sh/connect/authorize?state=first", "amux")
        }
        val second = async(start = CoroutineStart.UNDISPATCHED) {
            browser.present("https://amux.sh/connect/authorize?state=second", "amux")
        }

        try {
            first.await()
            fail("Expected first sign-in session to be cancelled when superseded")
        } catch (_: CancellationException) {
            // expected
        }

        assertEquals(2, launched.size)
        assertTrue(browser.handleCallbackUri(Uri.parse("amux://callback?code=second-code&state=second")))
        assertEquals("amux://callback?code=second-code&state=second", second.await())
    }

    @Test
    fun cancelsOnResumeWhenGraceWindowExpiresWithoutCallback() = runTest {
        val scheduler = ManualScheduler()
        val browser = OAuthBrowser(
            callbackHost = "callback",
            resumeGraceMs = 350L,
            launchUrl = {},
            scheduleGraceCheck = scheduler::schedule,
        )

        val deferred = async(start = CoroutineStart.UNDISPATCHED) {
            browser.present("https://amux.sh/connect/authorize?state=cancel-me", "amux")
        }

        browser.onActivityResumed()
        assertEquals(1, scheduler.entries.size)
        assertEquals(350L, scheduler.entries.single().delayMs)
        assertTrue(browser.hasPendingSession)

        scheduler.fireActive()
        assertFalse(browser.hasPendingSession)

        try {
            deferred.await()
            fail("Expected CancellationException when Activity resumes without callback")
        } catch (_: CancellationException) {
            // expected
        }
    }

    @Test
    fun callbackArrivingDuringResumeGraceWindowCompletesSessionWithoutCancelling() = runTest {
        val scheduler = ManualScheduler()
        val browser = OAuthBrowser(
            callbackHost = "callback",
            resumeGraceMs = 350L,
            launchUrl = {},
            scheduleGraceCheck = scheduler::schedule,
        )

        val deferred = async(start = CoroutineStart.UNDISPATCHED) {
            browser.present("https://amux.sh/connect/authorize?state=in-time", "amux")
        }

        browser.onActivityResumed()
        assertTrue(browser.handleCallbackUri(Uri.parse("amux://callback?code=good&state=in-time")))
        assertTrue(scheduler.entries.single().cancelled)

        scheduler.fireActive()
        assertEquals("amux://callback?code=good&state=in-time", deferred.await())
    }

    @Test
    fun defaultLauncherFallsBackToActionViewWhenNoCustomTabsProviderInstalled() {
        val app = RuntimeEnvironment.getApplication()
        val uri = Uri.parse("https://amux.sh/connect/authorize?client_id=mobile")

        OAuthBrowser.openInCustomTabsOrBrowser(app, uri)

        val started = shadowOf(app).nextStartedActivity
        assertNotNull(started)
        assertEquals(Intent.ACTION_VIEW, started.action)
        assertEquals(uri, started.data)
        assertTrue((started.flags and Intent.FLAG_ACTIVITY_NEW_TASK) != 0)
    }
}
