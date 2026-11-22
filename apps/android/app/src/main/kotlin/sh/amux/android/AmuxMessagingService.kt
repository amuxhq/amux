package sh.amux.android

import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.os.Bundle
import androidx.core.app.NotificationCompat
import com.google.firebase.messaging.FirebaseMessagingService
import com.google.firebase.messaging.RemoteMessage
import sh.amux.push.PushAlert
import sh.amux.push.PushKeyStore
import sh.amux.push.SealedPush
import sh.amux.shell.NotificationChannels

/**
 * Receives FCM token refreshes and high-priority data-only sealed pushes from
 * `amux.sh`, decrypting them with [AndroidPushKeyStore] + [SealedPush] before
 * posting onto [NotificationChannels.CHANNEL_ATTENTION].
 */
class AmuxMessagingService : FirebaseMessagingService() {
    @Suppress("DEPRECATION")
    override fun onNewToken(token: String) {
        if (token.isBlank()) return
        val comp = (application as? AmuxApplication)?.compositionOrNull ?: return
        comp.pushRegistrar.updateToken(token.toByteArray(Charsets.UTF_8))
    }

    override fun onMessageReceived(message: RemoteMessage) {
        val comp = (application as? AmuxApplication)?.compositionOrNull
        PushNotificationDelivery.deliver(
            context = this,
            data = message.data,
            keys = AndroidPushKeyStore(this),
            isViewingChat = { host, agent -> comp?.isViewingChat(host, agent) == true },
        )
    }
}

/**
 * Testable push decryption and notification posting logic invoked by
 * [AmuxMessagingService].
 */
internal object PushNotificationDelivery {
    const val EXTRA_PUSH_TAP: String = "sh.amux.android.extra.PUSH_TAP"
    const val EXTRA_PUSH_HOST: String = "sh.amux.android.extra.PUSH_HOST"
    const val EXTRA_PUSH_AGENT: String = "sh.amux.android.extra.PUSH_AGENT"

    private const val NOTIFICATION_ID: Int = 1
    private const val FALLBACK_TAG: String = "amux_fallback"

    enum class Outcome {
        Posted,
        PostedFallback,
        SuppressedViewingChat,
        NotPermitted,
    }

    fun deliver(
        context: Context,
        data: Map<String, String>,
        keys: PushKeyStore,
        isViewingChat: (host: String, agent: String) -> Boolean = { _, _ -> false },
    ): Outcome {
        NotificationChannels.ensureCreated(context)
        if (!NotificationChannels.queryStatus(context).permitted) {
            return Outcome.NotPermitted
        }

        val to = data["to"]?.trim().orEmpty()
        val sealed = data["sealed"]?.trim().orEmpty()
        val pushId = data["id"]?.trim().orEmpty()

        val opened = if (to.isNotEmpty() && sealed.isNotEmpty()) {
            SealedPush.openForProfile(to = to, sealed = sealed, keys = keys)
        } else {
            null
        }
        val alert = opened?.let { PushAlert.from(it) }

        if (alert != null && isViewingChat(alert.host, alert.agent)) {
            return Outcome.SuppressedViewingChat
        }

        val title = alert?.title ?: context.getString(R.string.app_name)
        val body = alert?.body ?: context.getString(R.string.notification_fallback_body)
        // Deduplicate retries using `data.id` (server omits FCM collapse_key so
        // offline alerts are never dropped by FCM's 4-collapse-key limit).
        val tag = when {
            pushId.isNotEmpty() -> pushId
            alert != null -> alert.threadId
            else -> FALLBACK_TAG
        }

        val tapIntent = Intent(context, MainActivity::class.java).apply {
            flags = Intent.FLAG_ACTIVITY_SINGLE_TOP or Intent.FLAG_ACTIVITY_CLEAR_TOP
            putExtra(EXTRA_PUSH_TAP, true)
            if (alert != null) {
                putExtra(EXTRA_PUSH_HOST, alert.host)
                putExtra(EXTRA_PUSH_AGENT, alert.agent)
            }
        }
        val pendingIntent = PendingIntent.getActivity(
            context,
            tag.hashCode(),
            tapIntent,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )

        val extras = Bundle().apply {
            if (alert != null) {
                putString(EXTRA_PUSH_HOST, alert.host)
                putString(EXTRA_PUSH_AGENT, alert.agent)
            }
        }

        val notification = NotificationCompat.Builder(context, NotificationChannels.CHANNEL_ATTENTION)
            .setSmallIcon(R.drawable.ic_launcher_monochrome)
            .setContentTitle(title)
            .setContentText(body)
            .setStyle(NotificationCompat.BigTextStyle().bigText(body))
            .setCategory(NotificationCompat.CATEGORY_MESSAGE)
            .setPriority(NotificationCompat.PRIORITY_HIGH)
            .setAutoCancel(true)
            .setContentIntent(pendingIntent)
            .addExtras(extras)
            .apply {
                if (alert != null) {
                    setGroup(alert.threadId)
                }
            }
            .build()

        val manager = context.getSystemService(NotificationManager::class.java)
            ?: return Outcome.NotPermitted
        manager.notify(tag, NOTIFICATION_ID, notification)
        return if (alert != null) Outcome.Posted else Outcome.PostedFallback
    }

    fun clearForChat(context: Context, host: String, agent: String) {
        val manager = context.getSystemService(NotificationManager::class.java) ?: return
        val active = runCatching { manager.activeNotifications }.getOrNull() ?: return
        for (item in active) {
            val extras = item.notification.extras ?: continue
            val itemHost = extras.getString(EXTRA_PUSH_HOST)
            val itemAgent = extras.getString(EXTRA_PUSH_AGENT)
            if (itemHost.equals(host, ignoreCase = true) && itemAgent.equals(agent, ignoreCase = true)) {
                manager.cancel(item.tag, item.id)
            }
        }
    }
}
