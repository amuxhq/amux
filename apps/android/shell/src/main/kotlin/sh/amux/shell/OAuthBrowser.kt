package sh.amux.shell

import android.app.Activity
import android.content.ActivityNotFoundException
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.os.Handler
import android.os.Looper
import androidx.browser.customtabs.CustomTabsClient
import androidx.browser.customtabs.CustomTabsIntent
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CompletableDeferred
import sh.amux.core.runtime.CloudError
import sh.amux.core.runtime.CloudException
import sh.amux.core.runtime.WebAuthPresenter

/**
 * Browser authorization hand-off for [sh.amux.core.runtime.AmuxCloudService].
 *
 * Opens the PKCE authorization URL in Chrome Custom Tabs (falling back to
 * `ACTION_VIEW` if no Custom Tabs provider is installed) and awaits the
 * single `amux://callback` redirect delivered back via [handleCallbackUri].
 * If the Activity resumes without receiving a callback within the resume
 * grace window, the pending session cancels with [CancellationException].
 */
class OAuthBrowser internal constructor(
    private val callbackHost: String,
    private val resumeGraceMs: Long,
    private val launchUrl: (Uri) -> Unit,
    private val scheduleGraceCheck: (delayMs: Long, action: () -> Unit) -> () -> Unit,
) : WebAuthPresenter {

    constructor(
        contextProvider: () -> Context,
        callbackHost: String = DEFAULT_CALLBACK_HOST,
        resumeGraceMs: Long = DEFAULT_RESUME_GRACE_MS,
    ) : this(
        callbackHost = callbackHost,
        resumeGraceMs = resumeGraceMs,
        launchUrl = { uri -> openInCustomTabsOrBrowser(contextProvider(), uri) },
        scheduleGraceCheck = { delayMs, action ->
            val handler = Handler(Looper.getMainLooper())
            val runnable = Runnable(action)
            handler.postDelayed(runnable, delayMs)
            ({ handler.removeCallbacks(runnable) })
        },
    )

    constructor(
        context: Context,
        callbackHost: String = DEFAULT_CALLBACK_HOST,
        resumeGraceMs: Long = DEFAULT_RESUME_GRACE_MS,
    ) : this(
        contextProvider = { context },
        callbackHost = callbackHost,
        resumeGraceMs = resumeGraceMs,
    )

    private class PendingSession(
        val expectedScheme: String,
        val expectedHost: String,
        val deferred: CompletableDeferred<String>,
    )

    private val lock = Any()
    private var pending: PendingSession? = null
    private var cancelGraceTimer: (() -> Unit)? = null

    val hasPendingSession: Boolean
        get() = synchronized(lock) { pending?.deferred?.isActive == true }

    override suspend fun present(url: String, callbackScheme: String): String {
        val uri = try {
            Uri.parse(url)
        } catch (_: Exception) {
            null
        }
        val scheme = uri?.scheme?.lowercase()
        if (uri == null || (scheme != "https" && scheme != "http") || uri.host.isNullOrBlank()) {
            throw CloudException(CloudError.Refused("invalid sign-in address"))
        }

        val normalizedScheme = callbackScheme.trim().lowercase()
        val normalizedHost = callbackHost.trim().lowercase()
        val session = PendingSession(
            expectedScheme = normalizedScheme,
            expectedHost = normalizedHost,
            deferred = CompletableDeferred(),
        )

        val superseded: PendingSession?
        val oldTimer: (() -> Unit)?
        synchronized(lock) {
            superseded = pending
            oldTimer = cancelGraceTimer
            pending = session
            cancelGraceTimer = null
        }
        oldTimer?.invoke()
        superseded?.deferred?.cancel(CancellationException("Superseded by a new sign-in"))

        try {
            launchUrl(uri)
        } catch (err: Throwable) {
            synchronized(lock) {
                if (pending === session) {
                    pending = null
                    cancelGraceTimer?.invoke()
                    cancelGraceTimer = null
                }
            }
            val cloudError = CloudException(
                CloudError.Refused(err.message?.takeIf { it.isNotBlank() } ?: "could not open browser"),
            )
            session.deferred.completeExceptionally(cloudError)
            throw cloudError
        }

        return try {
            session.deferred.await()
        } finally {
            var timerToCancel: (() -> Unit)? = null
            synchronized(lock) {
                if (pending === session) {
                    pending = null
                    timerToCancel = cancelGraceTimer
                    cancelGraceTimer = null
                }
            }
            timerToCancel?.invoke()
        }
    }

    /**
     * Delivers an incoming redirect URI from `MainActivity.onCreate` or
     * `MainActivity.onNewIntent` to the active authorization session.
     *
     * Returns `true` only when a pending session matched the URI's scheme and
     * host and accepted the callback.
     */
    fun handleCallbackUri(uri: Uri): Boolean {
        val scheme = uri.scheme?.lowercase() ?: return false
        val host = uri.host?.lowercase() ?: return false

        val matched: PendingSession
        val timerToCancel: (() -> Unit)?
        synchronized(lock) {
            val current = pending ?: return false
            if (!current.deferred.isActive) {
                pending = null
                return false
            }
            if (scheme != current.expectedScheme || host != current.expectedHost) {
                return false
            }
            matched = current
            pending = null
            timerToCancel = cancelGraceTimer
            cancelGraceTimer = null
        }
        timerToCancel?.invoke()
        return matched.deferred.complete(uri.toString())
    }

    /** Convenience overload accepting a raw redirect URL string. */
    fun handleCallbackUrl(url: String): Boolean {
        val trimmed = url.trim()
        if (trimmed.isEmpty()) return false
        val uri = runCatching { Uri.parse(trimmed) }.getOrNull() ?: return false
        return handleCallbackUri(uri)
    }

    /**
     * Called when the hosting Activity resumes. Gives any in-flight redirect
     * intent a brief grace window to arrive via [handleCallbackUri] before
     * treating the return as a user cancellation.
     */
    fun onActivityResumed() {
        val session: PendingSession
        val previousTimer: (() -> Unit)?
        synchronized(lock) {
            val current = pending ?: return
            if (!current.deferred.isActive) {
                pending = null
                return
            }
            session = current
            previousTimer = cancelGraceTimer
            cancelGraceTimer = null
        }
        previousTimer?.invoke()

        val cancelHandle = scheduleGraceCheck(resumeGraceMs) {
            var toCancel: PendingSession? = null
            synchronized(lock) {
                if (pending === session && session.deferred.isActive) {
                    pending = null
                    cancelGraceTimer = null
                    toCancel = session
                }
            }
            toCancel?.deferred?.cancel(CancellationException("Browser sign-in cancelled"))
        }

        synchronized(lock) {
            if (pending === session && session.deferred.isActive) {
                cancelGraceTimer = cancelHandle
            } else {
                cancelHandle()
            }
        }
    }

    /** Cancels any pending browser sign-in immediately. */
    fun cancelPending(reason: String = "Browser sign-in cancelled") {
        val session: PendingSession?
        val timer: (() -> Unit)?
        synchronized(lock) {
            session = pending
            pending = null
            timer = cancelGraceTimer
            cancelGraceTimer = null
        }
        timer?.invoke()
        session?.deferred?.cancel(CancellationException(reason))
    }

    companion object {
        const val DEFAULT_CALLBACK_HOST: String = "callback"
        const val DEFAULT_RESUME_GRACE_MS: Long = 350L

        internal fun openInCustomTabsOrBrowser(context: Context, uri: Uri) {
            val provider = runCatching { CustomTabsClient.getPackageName(context, null) }.getOrNull()
            if (provider != null) {
                val openedCustomTab = runCatching {
                    val customTabsIntent = CustomTabsIntent.Builder()
                        .setShowTitle(true)
                        .build()
                    customTabsIntent.intent.setPackage(provider)
                    if (context !is Activity) {
                        customTabsIntent.intent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
                    }
                    customTabsIntent.launchUrl(context, uri)
                }.isSuccess
                if (openedCustomTab) return
            }

            val fallback = Intent(Intent.ACTION_VIEW, uri).apply {
                addCategory(Intent.CATEGORY_BROWSABLE)
                if (context !is Activity) {
                    addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
                }
            }
            try {
                context.startActivity(fallback)
            } catch (err: ActivityNotFoundException) {
                throw err
            }
        }
    }
}
