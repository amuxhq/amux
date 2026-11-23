package sh.amux.shell

import android.app.Activity
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.content.Intent
import android.graphics.Bitmap
import android.graphics.ImageDecoder
import android.net.Uri
import android.provider.OpenableColumns
import android.webkit.MimeTypeMap
import androidx.browser.customtabs.CustomTabsIntent
import androidx.core.content.FileProvider
import java.io.BufferedOutputStream
import java.io.ByteArrayOutputStream
import java.io.File
import java.io.FileOutputStream
import java.io.InputStream
import java.io.OutputStream
import java.nio.ByteBuffer
import java.util.zip.ZipEntry
import java.util.zip.ZipOutputStream
import kotlin.math.floor
import kotlin.math.sqrt
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import sh.amux.core.runtime.ChatModel
import sh.amux.core.runtime.PickedAttachment
import sh.amux.core.runtime.RuntimeFailure
import sh.amux.words.OpenableLink

/**
 * Android platform helpers owned by `:shell` so `:features` stays pure Compose.
 */
object ShellPlatform {
    internal const val MAX_TRANSCODE_DIMENSION: Int = 4096
    internal const val MAX_TRANSCODE_PIXELS: Long = 16_777_216L
    internal const val EXPORT_RETENTION_MS: Long = 3_600_000L

    private val UNSAFE_VIEW_EXTENSIONS = setOf(
        "apk",
        "sh",
        "command",
        "bat",
        "cmd",
        "exe",
        "jar",
    )

    internal data class BoundedReadResult(
        val bytes: ByteArray,
        val exceededLimit: Boolean,
    )

    internal data class PickedMetadata(
        val name: String,
        val size: Long,
        val mime: String?,
    )

    fun copyToClipboard(context: Context, text: String, label: String = "amux") {
        val manager = context.getSystemService(Context.CLIPBOARD_SERVICE) as? ClipboardManager ?: return
        manager.setPrimaryClip(ClipData.newPlainText(label, text))
    }

    fun openExternalLink(context: Context, link: OpenableLink) {
        openExternalUrl(context, link.url)
    }

    /**
     * Opens an external `http`/`https` URL in Chrome Custom Tabs (falling back
     * to `ACTION_VIEW`), or a `mailto:` URL via `ACTION_VIEW`.
     */
    fun openExternalUrl(context: Context, url: String) {
        val uri = runCatching { Uri.parse(url.trim()) }.getOrNull() ?: return
        val scheme = uri.scheme?.lowercase()
        when (scheme) {
            "http", "https" -> {
                val openedCustomTab = runCatching {
                    val customTabs = CustomTabsIntent.Builder().build()
                    if (context !is Activity) {
                        customTabs.intent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
                    }
                    customTabs.launchUrl(context, uri)
                }.isSuccess
                if (!openedCustomTab) {
                    launchViewIntent(context, uri)
                }
            }
            "mailto" -> launchViewIntent(context, uri)
        }
    }

    /**
     * Prepares a diagnostic dump file or directory for sharing via [FileProvider]
     * by pruning expired export zips and copying a single file or zipping a dump
     * bundle directory into `cacheDir/exports/`.
     */
    suspend fun prepareShareableExport(
        context: Context,
        source: File,
        nowMs: Long = System.currentTimeMillis(),
    ): File = withContext(Dispatchers.IO) {
        val exportsDir = File(context.cacheDir, "exports").apply { mkdirs() }
        pruneStaleExports(exportsDir, nowMs = nowMs)
        val baseName = source.name.ifBlank { "amux-diagnostics" }
        if (source.isDirectory) {
            val zipName = if (baseName.endsWith(".zip", ignoreCase = true)) baseName else "$baseName.zip"
            val target = File(exportsDir, zipName)
            ZipOutputStream(BufferedOutputStream(FileOutputStream(target))).use { zos ->
                val rootPath = source.canonicalFile.toPath()
                source.walkTopDown().filter { it.isFile }.forEach { entryFile ->
                    val rel = rootPath.relativize(entryFile.canonicalFile.toPath()).toString()
                        .replace(File.separatorChar, '/')
                    zos.putNextEntry(ZipEntry(rel))
                    entryFile.inputStream().use { input -> input.copyTo(zos) }
                    zos.closeEntry()
                }
            }
            target
        } else {
            val canonicalExports = exportsDir.canonicalFile
            val canonicalSource = source.canonicalFile
            if (canonicalSource.path.startsWith("${canonicalExports.path}${File.separator}")) {
                canonicalSource
            } else {
                val target = File(exportsDir, baseName)
                source.copyTo(target, overwrite = true)
                target
            }
        }
    }

    internal fun pruneStaleExports(
        exportsDir: File,
        nowMs: Long = System.currentTimeMillis(),
        retentionMs: Long = EXPORT_RETENTION_MS,
    ) {
        val cutoffMs = nowMs - retentionMs
        exportsDir.listFiles()
            ?.filter { file ->
                file.isFile &&
                    file.extension.equals("zip", ignoreCase = true) &&
                    file.lastModified() < cutoffMs
            }
            ?.forEach { it.delete() }
    }

    /**
     * Opens a downloaded local attachment in a viewer app via [FileProvider]
     * (`ACTION_VIEW` with read URI permission), falling back to [shareFile]
     * for unsafe executable extensions or when no viewer handles the MIME type.
     */
    fun openOrShareFile(context: Context, file: File) {
        val ext = file.extension.lowercase()
        if (ext in UNSAFE_VIEW_EXTENSIONS) {
            shareFile(context, file)
            return
        }
        val uri = fileProviderUri(context, file) ?: return
        val mime = resolveShareMime(file)
        val viewIntent = Intent(Intent.ACTION_VIEW).apply {
            setDataAndType(uri, mime)
            addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
            if (context !is Activity) {
                addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
            }
        }
        val opened = runCatching { context.startActivity(viewIntent) }.isSuccess
        if (!opened) {
            shareFile(context, file)
        }
    }

    /**
     * Shares or exports a local file via Android's system chooser using [FileProvider],
     * attaching [ClipData] and read URI permissions on both the send and chooser
     * intents and returning [Result] so callers never report false success.
     */
    fun shareFile(context: Context, file: File): Result<Unit> {
        val uri = fileProviderUri(context, file)
            ?: return Result.failure(IllegalStateException("Could not create share URI for ${file.name}"))
        val chooser = buildShareIntent(context, file, uri)
        return runCatching { context.startActivity(chooser) }
    }

    internal fun buildShareIntent(context: Context, file: File, uri: Uri): Intent {
        val mime = resolveShareMime(file)
        val sendIntent = Intent(Intent.ACTION_SEND).apply {
            type = mime
            putExtra(Intent.EXTRA_STREAM, uri)
            putExtra(Intent.EXTRA_SUBJECT, file.name)
            clipData = ClipData.newRawUri(file.name, uri)
            addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
        }
        return Intent.createChooser(sendIntent, file.name).apply {
            addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
            if (context !is Activity) {
                addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
            }
        }
    }

    private fun fileProviderUri(context: Context, file: File): Uri? =
        runCatching {
            FileProvider.getUriForFile(context, "${context.packageName}.fileprovider", file)
        }.getOrNull()

    private fun resolveShareMime(file: File): String {
        val ext = file.extension.lowercase()
        if (ext.isNotEmpty()) {
            MimeTypeMap.getSingleton().getMimeTypeFromExtension(ext)?.let { return it }
        }
        return PickedAttachment.fromFile(ByteArray(0), file.name, null).mime
    }

    private fun launchViewIntent(context: Context, uri: Uri) {
        val intent = Intent(Intent.ACTION_VIEW, uri).apply {
            addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        }
        runCatching { context.startActivity(intent) }
    }

    /**
     * Reads at most [maxBytes] + 1 bytes from [input] without buffering an
     * unbounded document into memory. Returns `exceededLimit = true` as soon
     * as the stream produces more than [maxBytes] bytes.
     */
    internal fun readStreamBounded(
        input: InputStream,
        maxBytes: Long,
    ): BoundedReadResult {
        require(maxBytes >= 0L) { "maxBytes must be non-negative" }
        val cap = maxBytes.coerceAtMost(Int.MAX_VALUE.toLong() - 1L).toInt()
        val out = ByteArrayOutputStream()
        val buffer = ByteArray(16 * 1024)
        var total = 0L
        while (true) {
            val remainingToExceed = (cap.toLong() + 1L - total).coerceAtMost(buffer.size.toLong()).toInt()
            if (remainingToExceed <= 0) {
                return BoundedReadResult(bytes = ByteArray(0), exceededLimit = true)
            }
            val read = input.read(buffer, 0, remainingToExceed)
            if (read < 0) {
                return BoundedReadResult(bytes = out.toByteArray(), exceededLimit = false)
            }
            total += read
            if (total > maxBytes) {
                return BoundedReadResult(bytes = ByteArray(0), exceededLimit = true)
            }
            out.write(buffer, 0, read)
        }
    }

    /**
     * Classifies picked bytes according to the shared native image rule
     * (PNG, JPEG, GIF, or WebP from magic bytes). Unsupported photo/image
     * formats (such as HEIC/HEIF/AVIF) are transcoded via [transcodeToNativeImage]
     * when possible, otherwise attached as a non-image file (`image = false`).
     */
    internal fun classifyPickedBytes(
        bytes: ByteArray,
        name: String,
        reportedMime: String?,
        isPhoto: Boolean,
        transcodeToNativeImage: ((ByteArray) -> Pair<ByteArray, String>?)? = null,
    ): PickedAttachment {
        val sniffed = PickedAttachment.sniffNativeImageMime(bytes)
        if (sniffed != null) {
            return PickedAttachment(
                data = bytes,
                name = name,
                mime = sniffed,
                image = true,
            )
        }

        val shouldTryTranscode = isPhoto || reportedMime?.lowercase()?.startsWith("image/") == true
        if (shouldTryTranscode && transcodeToNativeImage != null && bytes.isNotEmpty()) {
            val converted = runCatching { transcodeToNativeImage(bytes) }.getOrNull()
            if (converted != null) {
                val (convertedBytes, _) = converted
                val convertedSniffed = PickedAttachment.sniffNativeImageMime(convertedBytes)
                if (convertedSniffed != null) {
                    val convertedName = replaceImageExtension(name, convertedSniffed)
                    return PickedAttachment(
                        data = convertedBytes,
                        name = convertedName,
                        mime = convertedSniffed,
                        image = true,
                    )
                }
            }
        }

        return PickedAttachment.fromFile(
            data = bytes,
            name = name,
            mime = reportedMime,
        )
    }

    private fun replaceImageExtension(name: String, mime: String): String {
        val ext = when (mime) {
            "image/png" -> "png"
            "image/jpeg" -> "jpg"
            "image/gif" -> "gif"
            "image/webp" -> "webp"
            else -> return name
        }
        val stem = name.substringBeforeLast('.', name).ifEmpty { "photo" }
        return "$stem.$ext"
    }

    internal class OutputLimitExceededException : RuntimeException("Encoded output exceeded limit")

    internal class BoundedByteArrayOutputStream(
        private val maxBytes: Long,
    ) : OutputStream() {
        private val out = ByteArrayOutputStream()
        private var written: Long = 0L

        init {
            require(maxBytes >= 0L) { "maxBytes must be non-negative" }
        }

        override fun write(b: Int) {
            if (written + 1L > maxBytes) {
                throw OutputLimitExceededException()
            }
            out.write(b)
            written += 1L
        }

        override fun write(b: ByteArray, off: Int, len: Int) {
            if (off < 0 || len < 0 || off > b.size || off + len > b.size) {
                throw IndexOutOfBoundsException()
            }
            if (len == 0) return
            if (written + len.toLong() > maxBytes) {
                throw OutputLimitExceededException()
            }
            out.write(b, off, len)
            written += len.toLong()
        }

        fun toByteArray(): ByteArray = out.toByteArray()
    }

    /**
     * Computes a bounded decode target size preserving aspect ratio so neither
     * dimension exceeds [maxDimension] and total pixels never exceed [maxPixels].
     * Returns `null` for non-positive dimensions.
     */
    internal fun boundedTranscodeSize(
        width: Int,
        height: Int,
        maxDimension: Int = MAX_TRANSCODE_DIMENSION,
        maxPixels: Long = MAX_TRANSCODE_PIXELS,
    ): Pair<Int, Int>? {
        if (width <= 0 || height <= 0 || maxDimension <= 0 || maxPixels <= 0L) return null
        if (width <= maxDimension && height <= maxDimension && width.toLong() * height.toLong() <= maxPixels) {
            return width to height
        }
        val dimScale = minOf(
            maxDimension.toDouble() / width.toDouble(),
            maxDimension.toDouble() / height.toDouble(),
        )
        val pixelScale = sqrt(maxPixels.toDouble() / (width.toDouble() * height.toDouble()))
        val scale = minOf(1.0, dimScale, pixelScale)
        var targetW = floor(width * scale).toInt().coerceIn(1, maxDimension)
        var targetH = floor(height * scale).toInt().coerceIn(1, maxDimension)
        while (targetW.toLong() * targetH.toLong() > maxPixels && (targetW > 1 || targetH > 1)) {
            if (targetW >= targetH && targetW > 1) {
                targetW -= 1
            } else if (targetH > 1) {
                targetH -= 1
            }
        }
        return targetW to targetH
    }

    private fun transcodeViaImageDecoder(
        bytes: ByteArray,
        maxOutputBytes: Long = ChatModel.ATTACHMENT_LIMIT_BYTES,
    ): Pair<ByteArray, String>? {
        val source = ImageDecoder.createSource(ByteBuffer.wrap(bytes))
        val bitmap = ImageDecoder.decodeBitmap(source) { decoder, info, _ ->
            decoder.allocator = ImageDecoder.ALLOCATOR_SOFTWARE
            val size = info.size
            val bounded = boundedTranscodeSize(size.width, size.height)
                ?: throw IllegalArgumentException("Invalid image dimensions: ${size.width}x${size.height}")
            if (bounded.first != size.width || bounded.second != size.height) {
                decoder.setTargetSize(bounded.first, bounded.second)
            }
        }
        return try {
            val out = BoundedByteArrayOutputStream(maxOutputBytes)
            val hasAlpha = bitmap.hasAlpha()
            val format = if (hasAlpha) Bitmap.CompressFormat.PNG else Bitmap.CompressFormat.JPEG
            val mime = if (hasAlpha) "image/png" else "image/jpeg"
            val quality = if (hasAlpha) 100 else 90
            val compressed = try {
                bitmap.compress(format, quality, out)
            } catch (_: OutputLimitExceededException) {
                return null
            }
            if (compressed) {
                out.toByteArray() to mime
            } else {
                null
            }
        } finally {
            bitmap.recycle()
        }
    }

    /**
     * Queries display name, size, and MIME type on [Dispatchers.IO], enforces
     * the 64 MB attachment cap both before opening and while streaming bytes,
     * classifies/transcodes images by their magic bytes, and uploads to [model].
     */
    suspend fun attachPickedUri(
        context: Context,
        uri: Uri,
        isPhoto: Boolean,
        model: ChatModel,
    ) = model.withAttachmentPreparation {
        val resolver = context.contentResolver
        val metadata = withContext(Dispatchers.IO) {
            var displayName: String? = null
            var reportedSize = -1L

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

            val detectedMime = runCatching { resolver.getType(uri) }.getOrNull()?.takeIf { it.isNotBlank() }
            PickedMetadata(name = resolvedName, size = reportedSize, mime = detectedMime)
        }

        if (metadata.size > 0L && !model.admits(metadata.name, metadata.size)) {
            return@withAttachmentPreparation
        }

        val limit = ChatModel.ATTACHMENT_LIMIT_BYTES
        val readOutcome = withContext(Dispatchers.IO) {
            runCatching {
                resolver.openInputStream(uri)?.use { stream ->
                    readStreamBounded(stream, maxBytes = limit)
                } ?: throw RuntimeFailure("could not read ${metadata.name}")
            }
        }

        val bounded = readOutcome.getOrElse { failure ->
            model.attachCheckingSize(
                name = metadata.name,
                size = 0L,
                mime = metadata.mime ?: "application/octet-stream",
                image = false,
                readBytes = { throw failure },
            )
            return@withAttachmentPreparation
        }

        if (bounded.exceededLimit) {
            model.admits(metadata.name, limit + 1L)
            return@withAttachmentPreparation
        }

        val classified = withContext(Dispatchers.IO) {
            classifyPickedBytes(
                bytes = bounded.bytes,
                name = metadata.name,
                reportedMime = metadata.mime,
                isPhoto = isPhoto,
                transcodeToNativeImage = { raw -> transcodeViaImageDecoder(raw, maxOutputBytes = limit) },
            )
        }
        model.attach(classified)
    }
}
