package sh.amux.shell

import android.Manifest
import android.app.Activity
import android.app.NotificationChannel
import android.app.NotificationManager
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.provider.Settings
import androidx.core.content.ContextCompat
import sh.amux.core.runtime.PushRegistrar

/** Snapshot of Android notification permission and attention-channel state. */
data class NotificationStatus(
    val permissionGranted: Boolean,
    val notificationsEnabled: Boolean,
    val attentionChannelEnabled: Boolean,
) {
    val permitted: Boolean
        get() = permissionGranted && notificationsEnabled && attentionChannelEnabled
}

/**
 * Sets up Android notification channels on API 34+, queries permission and
 * channel state, builds system notification-settings intents, and syncs
 * permission status into [PushRegistrar].
 */
object NotificationChannels {
    const val CHANNEL_ATTENTION: String = "amux_attention"
    const val CHANNEL_GENERAL: String = "amux_general"

    /**
     * Creates the app's notification channels idempotently. Android preserves
     * any user-adjusted settings on existing channels when called again.
     */
    fun ensureCreated(context: Context) {
        val manager = context.getSystemService(NotificationManager::class.java) ?: return
        val attention = NotificationChannel(
            CHANNEL_ATTENTION,
            context.getString(R.string.shell_channel_attention_name),
            NotificationManager.IMPORTANCE_HIGH,
        ).apply {
            description = context.getString(R.string.shell_channel_attention_description)
            enableVibration(true)
            setShowBadge(true)
        }
        val general = NotificationChannel(
            CHANNEL_GENERAL,
            context.getString(R.string.shell_channel_general_name),
            NotificationManager.IMPORTANCE_DEFAULT,
        ).apply {
            description = context.getString(R.string.shell_channel_general_description)
            setShowBadge(false)
        }
        manager.createNotificationChannels(listOf(attention, general))
    }

    /** Queries runtime `POST_NOTIFICATIONS` permission and channel status. */
    fun queryStatus(context: Context): NotificationStatus {
        val permissionGranted = ContextCompat.checkSelfPermission(
            context,
            Manifest.permission.POST_NOTIFICATIONS,
        ) == PackageManager.PERMISSION_GRANTED

        val manager = context.getSystemService(NotificationManager::class.java)
        val notificationsEnabled = manager?.areNotificationsEnabled() ?: false
        val attentionChannel = manager?.getNotificationChannel(CHANNEL_ATTENTION)
        val attentionChannelEnabled = attentionChannel == null ||
            attentionChannel.importance != NotificationManager.IMPORTANCE_NONE

        return NotificationStatus(
            permissionGranted = permissionGranted,
            notificationsEnabled = notificationsEnabled,
            attentionChannelEnabled = attentionChannelEnabled,
        )
    }

    /** Queries current notification status and syncs it into [registrar]. */
    fun syncPermission(
        context: Context,
        registrar: PushRegistrar,
    ): NotificationStatus {
        val status = queryStatus(context)
        registrar.updatePermitted(status.permitted)
        return status
    }

    /**
     * Builds an [Intent] opening Android's notification settings for this app
     * (or for a specific [channelId] when provided).
     */
    fun appNotificationSettingsIntent(
        context: Context,
        channelId: String? = null,
    ): Intent {
        val action = if (channelId != null) {
            Settings.ACTION_CHANNEL_NOTIFICATION_SETTINGS
        } else {
            Settings.ACTION_APP_NOTIFICATION_SETTINGS
        }
        return Intent(action).apply {
            putExtra(Settings.EXTRA_APP_PACKAGE, context.packageName)
            if (channelId != null) {
                putExtra(Settings.EXTRA_CHANNEL_ID, channelId)
            }
            if (context !is Activity) {
                addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
            }
        }
    }
}
