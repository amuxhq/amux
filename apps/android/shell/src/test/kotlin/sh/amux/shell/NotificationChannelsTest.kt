package sh.amux.shell

import android.Manifest
import android.app.NotificationChannel
import android.app.NotificationManager
import android.content.Intent
import android.provider.Settings
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config
import sh.amux.core.runtime.AccountId
import sh.amux.core.runtime.PushAccount
import sh.amux.core.runtime.PushRegistrar

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class NotificationChannelsTest {
    private class RecordingPushRegistrar : PushRegistrar {
        val permittedHistory = mutableListOf<Boolean>()

        override fun updateAccounts(accounts: List<PushAccount>) = Unit
        override fun updatePermitted(permitted: Boolean) {
            permittedHistory += permitted
        }
        override fun updateToken(token: ByteArray) = Unit
        override suspend fun withdraw(account: AccountId) = Unit
        override suspend fun settled() = Unit
    }

    @Test
    fun ensureCreatedCreatesAttentionAndGeneralChannelsIdempotently() {
        val app = RuntimeEnvironment.getApplication()
        val manager = app.getSystemService(NotificationManager::class.java)

        NotificationChannels.ensureCreated(app)
        NotificationChannels.ensureCreated(app)

        val channels = manager.notificationChannels
        assertEquals(2, channels.size)

        val attention = manager.getNotificationChannel(NotificationChannels.CHANNEL_ATTENTION)
        assertNotNull(attention)
        assertEquals(NotificationManager.IMPORTANCE_HIGH, attention.importance)
        assertEquals("Needs your attention", attention.name.toString())

        val general = manager.getNotificationChannel(NotificationChannels.CHANNEL_GENERAL)
        assertNotNull(general)
        assertEquals(NotificationManager.IMPORTANCE_DEFAULT, general.importance)
        assertEquals("General", general.name.toString())
    }

    @Test
    fun syncPermissionReflectsPermissionAndChannelStateInPushRegistrar() {
        val app = RuntimeEnvironment.getApplication()
        val manager = app.getSystemService(NotificationManager::class.java)
        val registrar = RecordingPushRegistrar()

        NotificationChannels.ensureCreated(app)

        // Initially denied
        shadowOf(app).denyPermissions(Manifest.permission.POST_NOTIFICATIONS)
        val deniedStatus = NotificationChannels.syncPermission(app, registrar)
        assertFalse(deniedStatus.permissionGranted)
        assertFalse(deniedStatus.permitted)
        assertEquals(listOf(false), registrar.permittedHistory)

        // Grant permission with enabled notifications and attention channel
        shadowOf(app).grantPermissions(Manifest.permission.POST_NOTIFICATIONS)
        shadowOf(manager).setNotificationsEnabled(true)
        val grantedStatus = NotificationChannels.syncPermission(app, registrar)
        assertTrue(grantedStatus.permissionGranted)
        assertTrue(grantedStatus.notificationsEnabled)
        assertTrue(grantedStatus.attentionChannelEnabled)
        assertTrue(grantedStatus.permitted)
        assertEquals(listOf(false, true), registrar.permittedHistory)

        // Mute the attention channel
        manager.createNotificationChannel(
            NotificationChannel(
                NotificationChannels.CHANNEL_ATTENTION,
                "Needs your attention",
                NotificationManager.IMPORTANCE_NONE,
            ),
        )
        val mutedStatus = NotificationChannels.syncPermission(app, registrar)
        assertFalse(mutedStatus.attentionChannelEnabled)
        assertFalse(mutedStatus.permitted)
        assertEquals(listOf(false, true, false), registrar.permittedHistory)
    }

    @Test
    fun buildsAppAndChannelNotificationSettingsIntents() {
        val app = RuntimeEnvironment.getApplication()

        val appIntent = NotificationChannels.appNotificationSettingsIntent(app)
        assertEquals(Settings.ACTION_APP_NOTIFICATION_SETTINGS, appIntent.action)
        assertEquals(app.packageName, appIntent.getStringExtra(Settings.EXTRA_APP_PACKAGE))
        assertTrue((appIntent.flags and Intent.FLAG_ACTIVITY_NEW_TASK) != 0)

        val channelIntent = NotificationChannels.appNotificationSettingsIntent(
            context = app,
            channelId = NotificationChannels.CHANNEL_ATTENTION,
        )
        assertEquals(Settings.ACTION_CHANNEL_NOTIFICATION_SETTINGS, channelIntent.action)
        assertEquals(app.packageName, channelIntent.getStringExtra(Settings.EXTRA_APP_PACKAGE))
        assertEquals(
            NotificationChannels.CHANNEL_ATTENTION,
            channelIntent.getStringExtra(Settings.EXTRA_CHANNEL_ID),
        )
    }
}
