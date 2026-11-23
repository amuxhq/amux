package sh.amux.push

import java.time.Instant
import java.time.ZoneId
import java.util.Locale

/**
 * The notification fields derived from an [OpenedPush], ready for the Android
 * notification builder.
 *
 * The agent's name and words replace the placeholder alert; [extras] carries
 * only the host and agent that a tap or notification clearing reads, never the
 * sealed blob; and [threadId] groups an agent's notifications together.
 */
data class PushAlert(
    val title: String,
    val body: String,
    val threadId: String,
    val host: String,
    val agent: String,
) {
    /** Metadata attached to the notification intent for tap routing and clearing. */
    val extras: Map<String, String>
        get() = mapOf("host" to host, "agent" to agent)

    companion object {
        const val DEFAULT_TITLE: String = "amux"

        fun from(
            push: OpenedPush,
            fallbackTitle: String = DEFAULT_TITLE,
            now: Instant = Instant.now(),
            zone: ZoneId = ZoneId.systemDefault(),
            locale: Locale = Locale.getDefault(),
        ): PushAlert {
            val title = push.name.ifEmpty { fallbackTitle }
            val body = listOfNotNull(
                PushWords.body(push.notice, now = now, zone = zone, locale = locale),
                PushWords.reason(push.notice),
            ).joinToString("\n")
            return PushAlert(
                title = title,
                body = body,
                threadId = "${push.host}/${push.agent}",
                host = push.host,
                agent = push.agent,
            )
        }
    }
}
