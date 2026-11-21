package sh.amux.words

import sh.amux.push.PushWords
import sh.amux.push.Said
import sh.amux.values.ActivityKind
import sh.amux.values.ActivityLine
import sh.amux.values.ExitCause
import sh.amux.values.Kind
import sh.amux.values.RelayLink
import sh.amux.values.SecondLine
import java.time.Instant
import java.time.ZoneId
import java.util.Locale

/**
 * What a fleet row says under its name: the words for the runtime's
 * second line, as every client says them.
 */
object FleetWords {
    /**
     * How loud a second line reads. What an agent asks is the loudest line
     * on home; a line that says the agent cannot go on warns.
     */
    enum class Ink {
        Ask,
        Quiet,
        Warning,
        Error,
    }

    data class Line(
        val text: String,
        val ink: Ink,
    )

    /**
     * Null when there is nothing to say (starting, or nothing known yet),
     * and for what the row's state word already says: that its host is away,
     * or that it exited with no cause.
     *
     * Jobs the agent runs in the background leave it idle, so an idle or
     * working line leads with how many there are, ahead of anything a long
     * line could cut off.
     */
    fun secondLine(
        line: SecondLine,
        kind: Kind,
        now: Instant = Instant.now(),
        backgroundJobs: Int = 0,
        zone: ZoneId = ZoneId.systemDefault(),
        locale: Locale = Locale.getDefault(),
    ): Line? {
        if (backgroundJobs > 0) {
            val count = if (backgroundJobs == 1) {
                "1 background job"
            } else {
                "$backgroundJobs background jobs"
            }
            when (line) {
                is SecondLine.LastSaid ->
                    return if (line.value.isNotEmpty()) {
                        Line("$count · ${line.value}", Ink.Quiet)
                    } else {
                        Line("$count running", Ink.Quiet)
                    }
                is SecondLine.Step -> return Line("$count · ${step(line.value)}", Ink.Quiet)
                SecondLine.Blank -> return Line("$count running", Ink.Quiet)
                else -> Unit
            }
        }
        return when (line) {
            is SecondLine.Said -> {
                val ink = if (line.value is Said.Ask) Ink.Ask else Ink.Warning
                Line(
                    PushWords.body(
                        line.value,
                        provider = provider(kind),
                        now = now,
                        zone = zone,
                        locale = locale,
                    ),
                    ink,
                )
            }
            is SecondLine.Step -> Line(step(line.value), Ink.Quiet)
            is SecondLine.LastSaid -> if (line.value.isEmpty()) null else Line(line.value, Ink.Quiet)
            is SecondLine.Exited -> when (val cause = line.value) {
                is ExitCause.Failed -> Line("Stopped · ${cause.value}", Ink.Error)
                ExitCause.Finished, ExitCause.Ended -> null
            }
            SecondLine.HostRevoked -> Line("Unpaired by its computer · pair again to reach it", Ink.Warning)
            SecondLine.Blank -> null
        }
    }

    /**
     * The step a working agent is on, as the chat's activity line names it:
     * the step's own subject when it is running one, and while it waits on
     * its subagents and one of them waits on the person, how they stand.
     */
    fun step(line: ActivityLine): String {
        val step = line.step
        if (line.activity.kind is ActivityKind.Running && !step.isNullOrEmpty()) {
            return ChatWords.firstLine(step)
        }
        return ChatWords.activityParts(
            kind = line.activity.kind,
            elapsedMs = 0L,
            subject = null,
            subagents = line.subagents,
        ).words
    }

    /** The provider a sign-in belongs to. */
    fun provider(kind: Kind): String = when (kind) {
        Kind.ClaudeSdk, Kind.ClaudePty -> "Claude"
        Kind.Codex -> "Codex"
        Kind.Unspecified -> "the provider"
    }

    /** What home says under its title: who needs you, else how many work. */
    fun subtitle(needYou: Int, working: Int, agents: Int): String {
        if (needYou <= 0) return "Nothing needs you · $working working"
        val noun = if (agents == 1) "agent" else "agents"
        return "$needYou need you · $agents $noun"
    }

    /** Why the relay cannot reach this account's computers, when the person can fix it. */
    fun relayTrouble(relay: RelayLink): String? = when (relay) {
        RelayLink.SignInAgain -> "Sign in again to reach your computers from anywhere"
        RelayLink.VersionMismatch -> "Update amux to reach your computers from anywhere"
        RelayLink.Off,
        RelayLink.Connecting,
        RelayLink.Retrying,
        RelayLink.Connected,
        -> null
    }
}
