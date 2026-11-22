package sh.amux.push

import java.time.Instant
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import java.time.format.FormatStyle
import java.util.Locale

/**
 * What an agent wants of the person, in words. A notification says it and
 * so does the agent's row on the fleet, from this one place.
 */
object PushWords {
    private const val SHORT_RESET_WINDOW_MS = 20L * 3_600L * 1_000L

    /**
     * [provider] is the word for the agent's provider, which a row knows and
     * a push does not: a notice names none.
     */
    fun body(
        said: Said,
        provider: String? = null,
        now: Instant = Instant.now(),
        zone: ZoneId = ZoneId.systemDefault(),
        locale: Locale = Locale.getDefault(),
    ): String = when (said) {
        is Said.Ask -> {
            val words = wants(said.wants)
            if (said.more > 0) "$words · ${said.more} more" else words
        }
        Said.SignedOut ->
            provider?.let { "Signed out of $it" }
                ?: "Signed out of its provider · sign in on its device"
        is Said.UsageLimit -> {
            val resetsAtMs = said.resetsAtMs
            if (resetsAtMs == null) {
                "Usage limit reached"
            } else {
                val at = Instant.ofEpochMilli(resetsAtMs)
                if (!at.isAfter(now)) {
                    "Usage limit reached"
                } else {
                    "Usage limit reached · resets ${resets(at, now, zone, locale)}"
                }
            }
        }
        is Said.Failed -> firstLine(said.message).ifEmpty { "Its turn failed" }
        is Said.Died -> {
            val detail = firstLine(said.detail)
            if (detail.isEmpty()) "Stopped unexpectedly" else "Stopped unexpectedly: $detail"
        }
    }

    /**
     * The provider's reason for an ask, which a notification has room for
     * and a row does not.
     */
    fun reason(said: Said): String? {
        if (said !is Said.Ask) return null
        return firstLine(said.why).ifEmpty { null }
    }

    private fun wants(wants: Wants): String {
        val words = when (wants) {
            is Wants.Run -> "Wants to run ${firstLine(wants.command)}"
            is Wants.Change -> when {
                wants.files > 1 -> "Wants to edit ${wants.files} files"
                wants.created == true -> "Wants to create ${wants.path}"
                wants.created == false -> "Wants to edit ${wants.path}"
                else -> "Wants to write ${wants.path}"
            }
            is Wants.Use -> {
                val target = listOf(wants.server, wants.tool)
                    .filter { it.isNotEmpty() }
                    .joinToString(" ")
                "Wants to use $target"
            }
            is Wants.Question -> firstLine(wants.text)
            is Wants.Plan ->
                if (wants.title.isEmpty()) {
                    "Has a plan for you to decide"
                } else {
                    "Has a plan for you to decide: ${firstLine(wants.title)}"
                }
            is Wants.Access -> "Wants access: ${firstLine(wants.reason)}"
            is Wants.Other ->
                if (wants.server.isEmpty()) {
                    firstLine(wants.text)
                } else {
                    "${wants.server}: ${firstLine(wants.text)}"
                }
        }
        return words.ifEmpty { "Needs you" }
    }

    /**
     * When a limit resets: the time of day when it is within the day, else
     * the abbreviated weekday.
     */
    fun resets(
        at: Instant,
        now: Instant,
        zone: ZoneId = ZoneId.systemDefault(),
        locale: Locale = Locale.getDefault(),
    ): String {
        val zoned = at.atZone(zone)
        return if (at.toEpochMilli() - now.toEpochMilli() < SHORT_RESET_WINDOW_MS) {
            DateTimeFormatter.ofLocalizedTime(FormatStyle.SHORT)
                .withLocale(locale)
                .format(zoned)
        } else {
            DateTimeFormatter.ofPattern("EEE", locale).format(zoned)
        }
    }

    private fun firstLine(text: String): String {
        val newline = text.indexOf('\n')
        return if (newline < 0) text else text.substring(0, newline)
    }
}
