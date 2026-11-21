package sh.amux.android

import android.app.Activity
import android.graphics.Bitmap
import android.graphics.Canvas
import android.os.Handler
import android.os.HandlerThread
import android.view.PixelCopy
import android.view.View
import android.view.inspector.WindowInspector
import java.io.ByteArrayOutputStream
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import kotlinx.coroutines.async
import sh.amux.core.runtime.FrozenFrame
import sh.amux.core.runtime.ReportCapture
import sh.amux.core.runtime.ReportFreezing

/**
 * Captures the active [Activity] window into a [FrozenFrame] (using API 34+
 * [PixelCopy] with a safe [View.draw] fallback) and begins an asynchronous
 * diagnostic dump for [sh.amux.core.runtime.ReportStore].
 */
internal class ReportFreezer(
    private val activity: Activity,
    private val compositionProvider: () -> Composition,
) : ReportFreezing {
    override fun freeze(): ReportCapture {
        val comp = compositionProvider()
        val frame = captureWindowFrame(activity)
        val dump = comp.scope.async { comp.runtime.stores.dump("report") }
        val routerSnap = comp.router.snapshot
        val route = routerSnap.topRoute?.routeName ?: routerSnap.tab.wireName
        return ReportCapture(
            frame = frame,
            dump = dump,
            route = route,
            runtimeFailure = comp.runtime.snapshot.failure,
        )
    }

    companion object {
        private const val PIXEL_COPY_TIMEOUT_MS: Long = 250L

        private val copyHandler: Handler by lazy {
            val thread = HandlerThread("amux-report-pixel-copy").apply {
                isDaemon = true
                start()
            }
            Handler(thread.looper)
        }

        internal fun captureWindowFrame(activity: Activity): FrozenFrame {
            val metrics = activity.resources.displayMetrics
            val scale = metrics.density.coerceAtLeast(1f).toDouble()
            val window = activity.window
            val decor = window?.decorView
            val width = decor?.width ?: 0
            val height = decor?.height ?: 0
            if (window == null || decor == null || width <= 0 || height <= 0) {
                return FrozenFrame(
                    png = ByteArray(0),
                    widthDp = metrics.widthPixels / scale,
                    heightDp = metrics.heightPixels / scale,
                    scale = scale,
                )
            }

            val bitmap = Bitmap.createBitmap(width, height, Bitmap.Config.ARGB_8888)
            var copied = false
            if (decor.isAttachedToWindow) {
                val latch = CountDownLatch(1)
                var resultCode = PixelCopy.ERROR_UNKNOWN
                val requested = runCatching {
                    PixelCopy.request(
                        window,
                        bitmap,
                        { code ->
                            resultCode = code
                            latch.countDown()
                        },
                        copyHandler,
                    )
                }.isSuccess
                if (requested && latch.await(PIXEL_COPY_TIMEOUT_MS, TimeUnit.MILLISECONDS)) {
                    copied = resultCode == PixelCopy.SUCCESS
                }
            }

            if (!copied) {
                runCatching {
                    val canvas = Canvas(bitmap)
                    decor.draw(canvas)
                }
            }

            // Composite any visible modal sheets or dialogs rendered in overlay windows.
            val overlayViews = runCatching { WindowInspector.getGlobalWindowViews() }
                .getOrDefault(emptyList())
                .filter { it !== decor && it.visibility == View.VISIBLE && it.width > 0 && it.height > 0 }
            if (overlayViews.isNotEmpty()) {
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

            val png = try {
                ByteArrayOutputStream().use { out ->
                    bitmap.compress(Bitmap.CompressFormat.PNG, 100, out)
                    out.toByteArray()
                }
            } finally {
                bitmap.recycle()
            }

            return FrozenFrame(
                png = png,
                widthDp = width / scale,
                heightDp = height / scale,
                scale = scale,
            )
        }
    }
}
