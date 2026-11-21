package sh.amux.shell

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.provider.OpenableColumns
import androidx.browser.customtabs.CustomTabsIntent
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import sh.amux.core.runtime.ChatModel
import sh.amux.core.runtime.PickedAttachment
import sh.amux.core.runtime.RuntimeFailure
import sh.amux.words.OpenableLink

/**
 * Android platform helpers owned by `:shell` so `:features` stays pure Compose.
 */
internal object ShellPlatform {
    fun copyToClipboard(context: Context, text: String, label: String = "amux") {
        val manager = context.getSystemService(Context.CLIPBOARD_SERVICE) as? ClipboardManager ?: return
        manager.setPrimaryClip(ClipData.newPlainText(label, text))
    }

    fun openExternalLink(context: Context, link: OpenableLink) {
        val uri = runCatching { Uri.parse(link.url) }.getOrNull() ?: return
        val scheme = uri.scheme?.lowercase()
        when (scheme) {
            "http", "https" -> {
                val openedCustomTab = runCatching {
                    val customTabs = CustomTabsIntent.Builder().build()
                    customTabs.launchUrl(context, uri)
                }.isSuccess
                if (!openedCustomTab) {
                    launchViewIntent(context, uri)
                }
            }
            "mailto" -> launchViewIntent(context, uri)
        }
    }

    private fun launchViewIntent(context: Context, uri: Uri) {
        val intent = Intent(Intent.ACTION_VIEW, uri).apply {
            addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        }
        runCatching { context.startActivity(intent) }
    }

    /**
     * Reads display name, MIME type, and size from [ContentResolver] BEFORE
     * reading bytes, then hands off to [ChatModel.attachCheckingSize] so
     * oversized (>64 MB) files are refused before entering memory.
     */
    fun attachPickedUri(
        context: Context,
        uri: Uri,
        isPhoto: Boolean,
        model: ChatModel,
    ) {
        val resolver = context.contentResolver
        var displayName: String? = null
        var reportedSize: Long = -1L

        runCatching {
            resolver.query(
                uri,
                arrayOf(OpenableColumns.DISPLAY_NAME, OpenableColumns.SIZE),
                null,
                null,
                null,
            )?.use { cursor ->
                if (cursor.moveToFirst()) {
                    val nameIdx = cursor.getColumnIndex(OpenableColumns.DISPLAY_NAME)
                    if (nameIdx >= 0 && !cursor.isNull(nameIdx)) {
                        displayName = cursor.getString(nameIdx)
                    }
                    val sizeIdx = cursor.getColumnIndex(OpenableColumns.SIZE)
                    if (sizeIdx >= 0 && !cursor.isNull(sizeIdx)) {
                        reportedSize = cursor.getLong(sizeIdx)
                    }
                }
            }
        }

        if (reportedSize < 0L) {
            reportedSize = runCatching {
                resolver.openAssetFileDescriptor(uri, "r")?.use { afd ->
                    afd.length.takeIf { it >= 0L } ?: 0L
                }
            }.getOrNull() ?: 0L
        }

        val resolvedName = displayName?.takeIf { it.isNotBlank() }
            ?: uri.lastPathSegment?.substringAfterLast('/')?.takeIf { it.isNotBlank() }
            ?: if (isPhoto) "photo.jpg" else "attachment"

        val detectedMime = resolver.getType(uri)?.takeIf { it.isNotBlank() }
            ?: PickedAttachment.fromFile(ByteArray(0), resolvedName, null).mime
        val treatAsImage = isPhoto || PickedAttachment.isNativeImageMime(detectedMime)

        model.attachCheckingSize(
            name = resolvedName,
            size = reportedSize,
            mime = detectedMime,
            image = treatAsImage,
            readBytes = {
                withContext(Dispatchers.IO) {
                    resolver.openInputStream(uri)?.use { stream ->
                        stream.readBytes()
                    } ?: throw RuntimeFailure("could not read $resolvedName")
                }
            },
        )
    }
}
