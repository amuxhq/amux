package sh.amux.features

import android.graphics.BitmapFactory
import android.util.LruCache
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Immutable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asAndroidBitmap
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.platform.LocalConfiguration
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import sh.amux.design.LocalAmuxPhotographed

/** Where an image being drawn from bytes has got to. */
@Immutable
sealed interface DecodedImage {
    /** No bytes yet, or they are still being decoded. */
    data object Pending : DecodedImage

    /** The bytes arrived but are not an image this phone can draw. */
    data object Unreadable : DecodedImage

    data class Ready(val bitmap: ImageBitmap) : DecodedImage

    val bitmapOrNull: ImageBitmap?
        get() = (this as? Ready)?.bitmap
}

/**
 * Draws [bytes] as an image no larger than [maxPx] on its longer side, or,
 * when [crop], on its shorter side.
 *
 * [key] names the image for the shared cache (a blob's hash); without one the
 * image is decoded again whenever [bytes] changes and is not kept.
 */
@Composable
fun rememberDecodedImage(
    bytes: ByteArray?,
    maxPx: Int,
    key: Any? = null,
    crop: Boolean = false,
): DecodedImage = rememberDecodedImage(key = key, maxPx = maxPx, revision = bytes, crop = crop) { bytes }

/**
 * Draws the image [load] returns, no larger than [maxPx] on its longer side.
 *
 * Decoding a full-size photo takes long enough to drop frames, and an
 * attachment can be many times the screen's size, so the bytes are first
 * read for their bounds, then decoded off the main thread at the largest
 * power-of-two reduction that still covers [maxPx]. Decoded images are kept
 * by [key] in a small cache shared across the app, so a row scrolled back
 * into view or a chip redrawn by the composer draws at once.
 *
 * [load] runs outside composition, on the main thread, whenever [key],
 * [maxPx] or [revision] changes; a caller whose bytes can land later passes
 * something that changes when they do as [revision].
 *
 * An image drawn [crop]ped fills its box and is cut to it, so it is
 * decoded with its shorter side covering [maxPx]; a wide panorama in a square
 * chip would otherwise be stretched up from a few pixels and blur.
 *
 * Under deterministic capture ([LocalAmuxPhotographed]) the image is decoded
 * in place, so a picture or a test sees it in its first frame.
 */
@Composable
fun rememberDecodedImage(
    key: Any?,
    maxPx: Int,
    revision: Any? = null,
    crop: Boolean = false,
    load: () -> ByteArray?,
): DecodedImage {
    val photographed = LocalAmuxPhotographed.current
    val currentLoad by rememberUpdatedState(load)
    val cacheKey = key?.let { DecodedImageKey(it, maxPx, crop) }
    val state = remember(cacheKey, maxPx, crop, revision) {
        val cached = cacheKey?.let(DecodedImages::get)
        mutableStateOf(
            when {
                cached != null -> DecodedImage.Ready(cached)
                photographed -> DecodedImages.decode(load(), maxPx, crop).also { decoded ->
                    if (cacheKey != null && decoded is DecodedImage.Ready) DecodedImages.put(cacheKey, decoded.bitmap)
                }
                else -> DecodedImage.Pending
            },
        )
    }
    LaunchedEffect(state) {
        if (state.value != DecodedImage.Pending) return@LaunchedEffect
        val bytes = currentLoad() ?: return@LaunchedEffect
        val decoded = withContext(Dispatchers.Default) { DecodedImages.decode(bytes, maxPx, crop) }
        if (cacheKey != null && decoded is DecodedImage.Ready) DecodedImages.put(cacheKey, decoded.bitmap)
        state.value = decoded
    }
    return state.value
}

/** The longer side of the screen in pixels, the most a full-screen image is drawn at. */
@Composable
fun rememberScreenMaxPx(): Int {
    val configuration = LocalConfiguration.current
    val density = LocalDensity.current
    return remember(configuration.screenWidthDp, configuration.screenHeightDp, density) {
        with(density) { maxOf(configuration.screenWidthDp, configuration.screenHeightDp).dp.roundToPx() }
    }
}

/** [size] in pixels, for an image drawn into a box that size. */
@Composable
fun rememberPx(size: Dp): Int {
    val density = LocalDensity.current
    return remember(size, density) { with(density) { size.roundToPx() } }
}

internal data class DecodedImageKey(val key: Any, val maxPx: Int, val crop: Boolean)

internal object DecodedImages {
    /** Enough for a screenful of thumbnails and a preview or two. */
    private const val MAX_KILOBYTES: Int = 24 * 1024

    private val cache = object : LruCache<DecodedImageKey, ImageBitmap>(MAX_KILOBYTES) {
        override fun sizeOf(key: DecodedImageKey, value: ImageBitmap): Int =
            (value.asAndroidBitmap().allocationByteCount / 1024).coerceAtLeast(1)
    }

    fun get(key: DecodedImageKey): ImageBitmap? = cache.get(key)

    fun put(key: DecodedImageKey, bitmap: ImageBitmap) {
        cache.put(key, bitmap)
    }

    internal fun clear() = cache.evictAll()

    fun decode(bytes: ByteArray?, maxPx: Int, crop: Boolean = false): DecodedImage {
        if (bytes == null) return DecodedImage.Pending
        if (bytes.isEmpty()) return DecodedImage.Unreadable
        val bitmap = runCatching {
            val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
            BitmapFactory.decodeByteArray(bytes, 0, bytes.size, bounds)
            val options = BitmapFactory.Options().apply {
                inSampleSize = sampleSize(bounds.outWidth, bounds.outHeight, maxPx, crop)
            }
            BitmapFactory.decodeByteArray(bytes, 0, bytes.size, options)?.asImageBitmap()
        }.getOrNull()
        return if (bitmap != null) DecodedImage.Ready(bitmap) else DecodedImage.Unreadable
    }

    /**
     * The largest power of two that keeps the longer side at or above
     * [maxPx], or the shorter side when the image is drawn [crop]ped.
     */
    fun sampleSize(width: Int, height: Int, maxPx: Int, crop: Boolean = false): Int {
        val side = if (crop) minOf(width, height) else maxOf(width, height)
        if (side <= 0 || maxPx <= 0) return 1
        var sample = 1
        while (side / (sample * 2) >= maxPx) {
            sample *= 2
        }
        return sample
    }
}
