package sh.amux.core.runtime

import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.graphics.Color
import java.io.ByteArrayOutputStream
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode
import sh.amux.values.ReportSent

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
@GraphicsMode(GraphicsMode.Mode.NATIVE)
class ReportRedactionTest {
    @Test
    fun bitmapRedactorBurnsOpaqueBlackOverRedactedMarksAndPreservesHighlightMarks() = runTest {
        val source = Bitmap.createBitmap(200, 400, Bitmap.Config.ARGB_8888)
        source.eraseColor(Color.WHITE)
        val originalPng = ByteArrayOutputStream().use { out ->
            assertTrue(source.compress(Bitmap.CompressFormat.PNG, 100, out))
            out.toByteArray()
        }
        source.recycle()

        val capture = ReportCapture(
            frame = FrozenFrame(png = originalPng, widthDp = 100.0, heightDp = 200.0, scale = 2.0),
            account = AccountId("ada"),
        )
        val store = ReportStore()
        assertTrue(store.begin { capture })
        // Mark 0: redacted area at dp (10..50, 20..60) -> px (20..100, 40..120)
        store.mark(ReportMark(x = 10.0, y = 20.0, width = 40.0, height = 40.0, note = "token", redact = true))
        // Mark 1: non-redacted highlight at dp (60..90, 100..150) -> px (120..180, 200..300)
        store.mark(ReportMark(x = 60.0, y = 100.0, width = 30.0, height = 50.0, note = "look here", redact = false))

        var uploaded: ReportUpload? = null
        store.send(
            submitter = { upload ->
                uploaded = upload
                Result.success(ReportSent(reportPath = "/saved/r2", id = "rep-2", receivedAtMs = 200L, notSent = ""))
            },
            account = AccountId("ada"),
            build = "0.10.0",
        )

        val payload = uploaded
        assertNotNull(payload)
        val decoded = BitmapFactory.decodeByteArray(payload!!.frame, 0, payload.frame.size)
        assertNotNull(decoded)
        try {
            // Inside redacted mark -> solid opaque black
            assertEquals(Color.BLACK, decoded.getPixel(50, 80))
            // Outside all marks -> untouched white
            assertEquals(Color.WHITE, decoded.getPixel(5, 5))
            // Inside non-redacted highlight mark -> untouched white
            assertEquals(Color.WHITE, decoded.getPixel(150, 250))
        } finally {
            decoded.recycle()
        }
    }
}
