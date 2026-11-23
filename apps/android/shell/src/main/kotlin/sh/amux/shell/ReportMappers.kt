package sh.amux.shell

import sh.amux.core.runtime.FrozenFrame
import sh.amux.core.runtime.ReportDumpState
import sh.amux.core.runtime.ReportMark
import sh.amux.core.runtime.ReportScreenshotState
import sh.amux.core.runtime.ReportSending
import sh.amux.core.runtime.ReportState
import sh.amux.features.ReportDeliveryUi
import sh.amux.features.ReportMarkUi
import sh.amux.features.ReportUiState

/** Pure mappers between the report store and the problem report overlay. */
object ReportMappers {
    fun toReportUiState(
        report: ReportState,
        signedIn: Boolean,
    ): ReportUiState? {
        val capture = report.capture ?: return null
        val frame = capture.frame
        val width = frame.widthDp.takeIf { it > 0.0 } ?: 1.0
        val height = frame.heightDp.takeIf { it > 0.0 } ?: 1.0
        val aspectRatio = (width / height).toFloat().takeIf { it.isFinite() && it > 0f }
            ?: ReportUiState.DEFAULT_FRAME_ASPECT_RATIO
        val marks = report.draft.marks.map { mark ->
            val left = (mark.x / width).toFloat().coerceIn(0f, 1f)
            val top = (mark.y / height).toFloat().coerceIn(0f, 1f)
            val right = ((mark.x + mark.width) / width).toFloat().coerceIn(left, 1f)
            val bottom = ((mark.y + mark.height) / height).toFloat().coerceIn(top, 1f)
            ReportMarkUi(
                left = left,
                top = top,
                right = right,
                bottom = bottom,
                note = mark.note,
                redact = mark.redact,
            )
        }
        val delivery = when (val sending = report.sending) {
            ReportSending.Ready -> ReportDeliveryUi.Ready
            ReportSending.Sending -> ReportDeliveryUi.Sending
            is ReportSending.Failed -> ReportDeliveryUi.Failed(sending.reason)
            is ReportSending.Sent -> ReportDeliveryUi.Sent(sending.receipt.id)
        }
        val screenshotPending = report.screenshotState == ReportScreenshotState.Pending
        val screenshotPresent = report.screenshotState == ReportScreenshotState.Present && frame.png.isNotEmpty()
        val dumpPending = report.dumpState == ReportDumpState.Pending
        val dumpReady = report.dumpState == ReportDumpState.Ready
        return ReportUiState(
            imageBytes = frame.png.takeIf { it.isNotEmpty() },
            frameAspectRatio = aspectRatio,
            note = report.draft.note,
            marks = marks,
            signedIn = signedIn,
            screenshotPending = screenshotPending,
            screenshotPresent = screenshotPresent,
            dumpPending = dumpPending,
            dumpReady = dumpReady,
            locked = report.sealed != null,
            delivery = delivery,
        )
    }

    fun toReportMark(mark: ReportMarkUi, frame: FrozenFrame): ReportMark {
        val width = frame.widthDp.takeIf { it > 0.0 } ?: 1.0
        val height = frame.heightDp.takeIf { it > 0.0 } ?: 1.0
        return ReportMark(
            x = mark.left * width,
            y = mark.top * height,
            width = mark.width * width,
            height = mark.height * height,
            note = mark.note,
            redact = mark.redact,
        )
    }

}
