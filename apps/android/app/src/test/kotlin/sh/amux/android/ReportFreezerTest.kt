package sh.amux.android

import android.app.Activity
import android.graphics.Bitmap
import android.graphics.Color
import android.view.PixelCopy
import android.view.View
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNotSame
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.Robolectric
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
@GraphicsMode(GraphicsMode.Mode.NATIVE)
class ReportFreezerTest {
    private fun setupSizedActivity(): Activity {
        val controller = Robolectric.buildActivity(Activity::class.java).setup()
        val activity = controller.get()
        val decor = activity.window.decorView
        decor.measure(
            View.MeasureSpec.makeMeasureSpec(120, View.MeasureSpec.EXACTLY),
            View.MeasureSpec.makeMeasureSpec(200, View.MeasureSpec.EXACTLY),
        )
        decor.layout(0, 0, 120, 200)
        return activity
    }

    @Test
    fun pixelCopyTimeoutUsesFreshFallbackBitmapAndLateCallbackRecyclesSafely() {
        val activity = setupSizedActivity()
        var pixelCopyBitmap: Bitmap? = null
        var lateListener: PixelCopy.OnPixelCopyFinishedListener? = null

        val captured = ReportFreezer.captureWindowBitmap(
            activity = activity,
            pixelCopyRequester = { _, dest, listener, _ ->
                pixelCopyBitmap = dest
                lateListener = listener
                // Intentionally never invoke listener before timeout.
            },
            timeoutMs = 10L,
            drawFallback = { _, canvas -> canvas.drawColor(Color.LTGRAY) },
        )

        assertNotNull(captured.bitmap)
        val fallback = captured.bitmap!!
        assertNotNull(pixelCopyBitmap)
        val copyBmp = pixelCopyBitmap!!
        assertNotSame("fallback must allocate its own bitmap instead of reusing PixelCopy bitmap", copyBmp, fallback)
        assertFalse("PixelCopy bitmap must stay unrecycled until its callback finishes", copyBmp.isRecycled)

        // Late callback arrives after fallback already completed: must recycle PixelCopy's bitmap without crashing.
        lateListener!!.onPixelCopyFinished(PixelCopy.SUCCESS)
        assertTrue("late callback must recycle abandoned PixelCopy bitmap", copyBmp.isRecycled)
        assertFalse("fallback bitmap must remain valid for background PNG encoding", fallback.isRecycled)

        val png = ReportFreezer.encodeAndRecycleBitmap(fallback)
        assertTrue("fallback bitmap should encode to non-empty PNG", png.isNotEmpty())
        assertTrue("encoder must recycle bitmap when done", fallback.isRecycled)
    }

    @Test
    fun failedPixelCopyAndFailingDrawFallbackReturnNullBitmapInsteadOfBlankPng() {
        val activity = setupSizedActivity()
        var pixelCopyBitmap: Bitmap? = null

        val captured = ReportFreezer.captureWindowBitmap(
            activity = activity,
            pixelCopyRequester = { _, dest, listener, _ ->
                pixelCopyBitmap = dest
                listener.onPixelCopyFinished(PixelCopy.ERROR_SOURCE_NO_DATA)
            },
            timeoutMs = 50L,
            drawFallback = { _, _ -> throw IllegalStateException("surface destroyed") },
        )

        assertNull("both capture paths failed -> bitmap must be null so report records frame absent", captured.bitmap)
        assertTrue("failed PixelCopy bitmap must be recycled", pixelCopyBitmap!!.isRecycled)
    }

    @Test
    fun pixelCopySuccessEncodesPngOffMainThreadAndBindsCaptureAccount() {
        runBlocking {
            val controller = Robolectric.buildActivity(MainActivity::class.java).setup()
            val activity = controller.get()
            val decor = activity.window.decorView
            decor.measure(
                View.MeasureSpec.makeMeasureSpec(120, View.MeasureSpec.EXACTLY),
                View.MeasureSpec.makeMeasureSpec(200, View.MeasureSpec.EXACTLY),
            )
            decor.layout(0, 0, 120, 200)
            val composition = (activity.application as AmuxApplication).composition
            composition.report.dismiss()

            val freezer = ReportFreezer(
                activity = activity,
                compositionProvider = { composition },
                pixelCopyRequester = { _, dest, listener, _ ->
                    dest.eraseColor(Color.WHITE)
                    listener.onPixelCopyFinished(PixelCopy.SUCCESS)
                },
            )

            assertTrue(composition.report.offer(freezer))
            val initialCapture = composition.report.state.value.capture
            assertNotNull(initialCapture)
            assertNotNull("framePng deferred should encode off main thread", initialCapture!!.framePng)

            val encoded = initialCapture.framePng!!.await()
            assertTrue("encoded PNG must be non-empty", encoded.isNotEmpty())
            // PNG magic signature: 89 50 4E 47
            assertEquals(0x89.toByte(), encoded[0])
            assertEquals(0x50.toByte(), encoded[1])
            assertEquals(0x4E.toByte(), encoded[2])
            assertEquals(0x47.toByte(), encoded[3])

            composition.report.dismiss()
            controller.pause().stop().destroy()
        }
    }
}
