package sh.amux.android

import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.graphics.Canvas
import android.graphics.Color
import android.graphics.Paint
import java.io.ByteArrayOutputStream
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import sh.amux.core.runtime.FrozenFrame
import sh.amux.core.runtime.ReportImageRedactor
import sh.amux.core.runtime.ReportMark

/**
 * Burns solid black rectangles over every redacted [ReportMark], off the main
 * thread, so what was marked never leaves the phone.
 */
object BitmapReportImageRedactor : ReportImageRedactor {
    override suspend fun redact(frame: FrozenFrame, marks: List<ReportMark>): ByteArray {
        val redactedMarks = marks.filter { it.redact }
        if (redactedMarks.isEmpty() || frame.png.isEmpty()) return frame.png
        return withContext(Dispatchers.Default) {
            val decoded = BitmapFactory.decodeByteArray(frame.png, 0, frame.png.size)
                ?: throw IllegalStateException("Could not decode screenshot to apply redactions.")
            val mutable = try {
                decoded.copy(Bitmap.Config.ARGB_8888, true)
                    ?: throw IllegalStateException("Could not allocate bitmap to apply redactions.")
            } finally {
                decoded.recycle()
            }
            try {
                val canvas = Canvas(mutable)
                val paint = Paint().apply {
                    style = Paint.Style.FILL
                    color = Color.BLACK
                    isAntiAlias = false
                }
                val scaleX = if (frame.widthDp > 0.0) mutable.width / frame.widthDp else frame.scale
                val scaleY = if (frame.heightDp > 0.0) mutable.height / frame.heightDp else frame.scale
                val maxW = mutable.width.toFloat()
                val maxH = mutable.height.toFloat()
                for (mark in redactedMarks) {
                    val left = (mark.x * scaleX).toFloat().coerceIn(0f, maxW)
                    val top = (mark.y * scaleY).toFloat().coerceIn(0f, maxH)
                    val right = ((mark.x + mark.width) * scaleX).toFloat().coerceIn(0f, maxW)
                    val bottom = ((mark.y + mark.height) * scaleY).toFloat().coerceIn(0f, maxH)
                    if (right > left && bottom > top) {
                        canvas.drawRect(left, top, right, bottom, paint)
                    }
                }
                ByteArrayOutputStream().use { out ->
                    if (!mutable.compress(Bitmap.CompressFormat.PNG, 100, out)) {
                        throw IllegalStateException("Could not encode redacted screenshot.")
                    }
                    out.toByteArray()
                }
            } finally {
                mutable.recycle()
            }
        }
    }
}
