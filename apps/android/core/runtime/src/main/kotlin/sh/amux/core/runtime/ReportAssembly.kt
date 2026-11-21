package sh.amux.core.runtime

import java.io.File
import java.nio.charset.StandardCharsets
import java.time.Instant
import java.time.ZoneOffset
import java.time.format.DateTimeFormatter
import kotlin.math.roundToInt
import kotlinx.coroutines.Deferred
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonArray
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put

/**
 * Turns a frozen capture and draft into the `report.json` header and upload
 * payload handed to [Profile.submitReport].
 */
object ReportAssembly {
    const val SCHEMA_VERSION: Int = 1
    const val REPORT_KIND: String = "report"

    private val prettyJson = Json { prettyPrint = true }
    private val rfc3339Formatter: DateTimeFormatter =
        DateTimeFormatter.ofPattern("yyyy-MM-dd'T'HH:mm:ss.SSS'Z'").withZone(ZoneOffset.UTC)

    fun upload(
        capture: ReportCapture,
        draft: ReportDraft,
        build: String,
        gitSha: String = "",
        createdAtMs: Long = System.currentTimeMillis(),
        dump: Result<File>,
    ): ReportUpload {
        val path: String
        val dumpAbsent: String?
        if (dump.isSuccess) {
            path = dump.getOrThrow().absolutePath
            dumpAbsent = null
        } else {
            path = ""
            val cause = dump.exceptionOrNull()
            dumpAbsent = (cause as? PartAbsent)?.why
                ?: cause?.message
                ?: "nothing was running, so there was nothing to dump"
        }
        return ReportUpload(
            header = header(
                capture = capture,
                draft = draft,
                build = build,
                gitSha = gitSha,
                createdAtMs = createdAtMs,
                dumpAbsent = dumpAbsent,
            ),
            frame = capture.frame.png,
            path = path,
        )
    }

    suspend fun dumped(dump: Deferred<Result<File>>?): Result<File> {
        if (dump == null) {
            return Result.failure(PartAbsent("nothing was running, so there was nothing to dump"))
        }
        return dump.await()
    }

    /**
     * Builds `report.json` with explicit contract keys (`schema_version`,
     * `kind`, `parts`, `git_sha`, `image_frame`, `marks`).
     */
    fun header(
        capture: ReportCapture,
        draft: ReportDraft,
        build: String,
        gitSha: String,
        createdAtMs: Long,
        dumpAbsent: String?,
    ): ByteArray {
        val detailLines = listOfNotNull(capture.route, capture.runtimeFailure)
        val detailElement: JsonElement = if (detailLines.isEmpty()) {
            JsonNull
        } else {
            JsonPrimitive(detailLines.joinToString("\n"))
        }

        val framePart: JsonElement = if (capture.frame.png.isNotEmpty()) {
            JsonPrimitive("present")
        } else {
            absentPart("the screen frame could not be captured")
        }
        val dumpPart: JsonElement = if (dumpAbsent == null) {
            JsonPrimitive("present")
        } else {
            absentPart(dumpAbsent)
        }

        val parts = buildJsonObject {
            put("dump", dumpPart)
            put("frame", framePart)
            put("trace", absentPart("the phone records no view trace"))
        }

        val marks = buildJsonArray {
            for (mark in draft.marks) {
                add(
                    buildJsonObject {
                        put("height", mark.height)
                        put("note", mark.note)
                        put("width", mark.width)
                        put("x", mark.x)
                        put("y", mark.y)
                    },
                )
            }
        }

        val imageFrame = buildJsonObject {
            put("height_pt", capture.frame.heightDp)
            put("scale", capture.frame.scale.roundToInt())
            put("width_pt", capture.frame.widthDp)
        }

        val root = buildJsonObject {
            put("build", build)
            put("created_at", formatRfc3339(createdAtMs))
            put("detail", detailElement)
            put("git_sha", gitSha)
            put("image_frame", imageFrame)
            put("kind", REPORT_KIND)
            put("marks", marks)
            put("note", draft.note)
            put("parts", parts)
            put("schema_version", SCHEMA_VERSION)
            put("viewport", JsonNull)
        }

        return prettyJson.encodeToString(JsonObject.serializer(), root)
            .toByteArray(StandardCharsets.UTF_8)
    }

    internal fun formatRfc3339(epochMs: Long): String =
        rfc3339Formatter.format(Instant.ofEpochMilli(epochMs))

    private fun absentPart(reason: String): JsonObject = buildJsonObject {
        put(
            "absent",
            buildJsonObject {
                put("reason", reason)
            },
        )
    }
}
