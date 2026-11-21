package sh.amux.android

import android.app.Activity
import android.graphics.Bitmap
import android.graphics.Canvas
import android.os.Handler
import android.os.HandlerThread
import android.view.PixelCopy
import android.view.View
import android.view.Window
import android.view.inspector.WindowInspector
import java.io.ByteArrayOutputStream
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.async
import kotlinx.coroutines.launch
import sh.amux.core.runtime.FrozenFrame
import sh.amux.core.runtime.ReportCapture
import sh.amux.core.runtime.ReportFreezing

/**
 * Captures the active [Activity] window into a [FrozenFrame] (using API 34+
 * [PixelCopy] with race-free bitmap ownership, a fresh [View.draw] fallback,
 * and off-main-thread PNG compression) and begins an asynchronous diagnostic
 * dump bound to the active account for [sh.amux.core.runtime.ReportStore].
 */
internal class ReportFreezer(
    private val activity: Activity,
    private val compositionProvider: () -> Composition,
    private val pixelCopyRequester: PixelCopyRequester = PlatformPixelCopyRequester,
) : ReportFreezing {

    internal fun interface PixelCopyRequester {
        fun request(
            window: Window,
            dest: Bitmap,
            listener: PixelCopy.OnPixelCopyFinishedListener,
            handler: Handler,
        )
    }

    override fun freeze(): ReportCapture {
        val comp = compositionProvider()
        val captured = captureWindowBitmap(activity, pixelCopyRequester)
        val dump = comp.scope.async { comp.runtime.stores.dump("report") }
        val routerSnap = comp.router.snapshot
        val route = routerSnap.topRoute?.routeName ?: routerSnap.tab.wireName
        val accountSnap = comp.accounts.snapshot
        val framePngDeferred = captured.bitmap?.let { bmp ->
            comp.scope.async(Dispatchers.Default) {
                encodeAndRecycleBitmap(bmp)
            }
        }
        val capture = ReportCapture(
            frame = FrozenFrame(
                png = ByteArray(0),
                widthDp = captured.widthDp,
                heightDp = captured.heightDp,
                scale = captured.scale,
            ),
            dump = dump,
            route = route,
            runtimeFailure = comp.runtime.snapshot.failure,
            account = accountSnap.selected,
            profileId = accountSnap.profile,
            framePng = framePngDeferred,
        )
        if (framePngDeferred != null) {
            comp.scope.launch {
                val png = runCatching { framePngDeferred.await() }.getOrDefault(ByteArray(0))
                if (png.isNotEmpty()) {
                    comp.report.updateFramePng(capture, png)
                }
            }
        }
        return capture
    }

    internal data class CapturedBitmap(
        val bitmap: Bitmap?,
        val widthDp: Double,
        val heightDp: Double,
        val scale: Double,
    )

    companion object {
        private const val PIXEL_COPY_TIMEOUT_MS: Long = 250L
        private const val COPY_PENDING = 0
        private const val COPY_CALLER_ABANDONED = 1
        private const val COPY_CALLBACK_SUCCEEDED = 2
        private const val COPY_CALLBACK_FAILED = 3

        private val PlatformPixelCopyRequester = PixelCopyRequester { window, dest, listener, handler ->
            PixelCopy.request(window, dest, listener, handler)
        }

        private val copyHandler: Handler by lazy {
            val thread = HandlerThread("amux-report-pixel-copy").apply {
                isDaemon = true
                start()
            }
            Handler(thread.looper)
        }

        internal fun captureWindowBitmap(
            activity: Activity,
            pixelCopyRequester: PixelCopyRequester = PlatformPixelCopyRequester,
            timeoutMs: Long = PIXEL_COPY_TIMEOUT_MS,
            drawFallback: (View, Canvas) -> Unit = { view, canvas -> view.draw(canvas) },
        ): CapturedBitmap {
            val metrics = activity.resources.displayMetrics
            val scale = metrics.density.coerceAtLeast(1f).toDouble()
            val window = activity.window
            val decor = window?.decorView
            val width = decor?.width ?: 0
            val height = decor?.height ?: 0
            if (window == null || decor == null || width <= 0 || height <= 0) {
                return CapturedBitmap(
                    bitmap = null,
                    widthDp = metrics.widthPixels / scale,
                    heightDp = metrics.heightPixels / scale,
                    scale = scale,
                )
            }

            var ownedBitmap: Bitmap? = null
            if (decor.isAttachedToWindow) {
                val pixelCopyBitmap = Bitmap.createBitmap(width, height, Bitmap.Config.ARGB_8888)
                val latch = CountDownLatch(1)
                val state = AtomicInteger(COPY_PENDING)
                val requested = runCatching {
                    pixelCopyRequester.request(
                        window,
                        pixelCopyBitmap,
                        { code ->
                            val next = if (code == PixelCopy.SUCCESS) {
                                COPY_CALLBACK_SUCCEEDED
                            } else {
                                COPY_CALLBACK_FAILED
                            }
                            val prev = state.getAndSet(next)
                            if (prev == COPY_CALLER_ABANDONED || next == COPY_CALLBACK_FAILED) {
                                pixelCopyBitmap.recycle()
                            }
                            latch.countDown()
                        },
                        copyHandler,
                    )
                }.isSuccess

                if (!requested) {
                    if (state.compareAndSet(COPY_PENDING, COPY_CALLER_ABANDONED)) {
                        pixelCopyBitmap.recycle()
                    }
                } else {
                    latch.await(timeoutMs, TimeUnit.MILLISECONDS)
                    if (!state.compareAndSet(COPY_PENDING, COPY_CALLER_ABANDONED) &&
                        state.get() == COPY_CALLBACK_SUCCEEDED
                    ) {
                        ownedBitmap = pixelCopyBitmap
                    }
                }
            }

            if (ownedBitmap == null) {
                val fallbackBitmap = Bitmap.createBitmap(width, height, Bitmap.Config.ARGB_8888)
                val drew = runCatching {
                    val canvas = Canvas(fallbackBitmap)
                    drawFallback(decor, canvas)
                }.isSuccess
                if (drew) {
                    ownedBitmap = fallbackBitmap
                } else {
                    fallbackBitmap.recycle()
                }
            }

            if (ownedBitmap != null) {
                compositeOverlayWindows(decor, ownedBitmap)
            }

            return CapturedBitmap(
                bitmap = ownedBitmap,
                widthDp = width / scale,
                heightDp = height / scale,
                scale = scale,
            )
        }

        internal fun encodeAndRecycleBitmap(bitmap: Bitmap): ByteArray = try {
            ByteArrayOutputStream().use { out ->
                if (bitmap.compress(Bitmap.CompressFormat.PNG, 100, out)) {
                    out.toByteArray()
                } else {
                    ByteArray(0)
                }
            }
        } catch (_: Throwable) {
            ByteArray(0)
        } finally {
            if (!bitmap.isRecycled) {
                bitmap.recycle()
            }
        }

        private fun compositeOverlayWindows(decor: View, bitmap: Bitmap) {
            val overlayViews = runCatching { WindowInspector.getGlobalWindowViews() }
                .getOrDefault(emptyList())
                .filter { it !== decor && it.visibility == View.VISIBLE && it.width > 0 && it.height > 0 }
            if (overlayViews.isEmpty()) return

            val mainLoc = IntArray(2)
            decor.getLocationOnScreen(mainLoc)
            val overlayLoc = IntArray(2)
            val canvas = Canvas(bitmap)
            for (overlay in overlayViews) {
                runCatching {
                    overlay.getLocationOnScreen(overlayLoc)
                    canvas.save()
                    canvas.translate(
                        (overlayLoc[0] - mainLoc[0]).toFloat(),
                        (overlayLoc[1] - mainLoc[1]).toFloat(),
                    )
                    overlay.draw(canvas)
                    canvas.restore()
                }
            }
        }
    }
}
