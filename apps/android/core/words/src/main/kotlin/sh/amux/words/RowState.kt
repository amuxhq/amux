package sh.amux.words

import sh.amux.values.Attention
import sh.amux.values.ExitCause
import sh.amux.values.FleetRow
import sh.amux.values.Reach

/**
 * Everything a fleet row can say about itself, in one closed set.
 *
 * A row draws one mark, the accent dot beside its age, for the one state that
 * is waiting on a person; everything else is said in words on the third line.
 */
sealed interface RowState {
    /** Stopped and cannot continue without you. The only case with a mark. */
    data object NeedsYou : RowState
    data object Working : RowState
    data object Starting : RowState

    /** The host that owns this agent is not answering. */
    data class HostOffline(val machine: String) : RowState

    /**
     * Seen by the relay, and not reachable from here: the row is what this
     * phone last held, and it stays on the list.
     */
    data class HostAway(val machine: String) : RowState

    /** A kind this build has no chat for. */
    data object Unsupported : RowState

    /** The agent's process ended, and how. */
    data class Exited(val cause: ExitCause) : RowState

    /**
     * The person settled it, and it did not fail: done with, and resumed by
     * a message.
     */
    data object Settled : RowState
    data object Idle : RowState

    /** The word the third line opens with. */
    val word: String
        get() = when (this) {
            NeedsYou -> "Needs you"
            Working -> "Working"
            Starting -> "Starting"
            is HostOffline -> "$machine offline"
            is HostAway -> "$machine away"
            Unsupported -> "Unknown agent"
            is Exited -> if (cause == ExitCause.Finished) "Finished" else "Stopped"
            Settled -> "Settled"
            Idle -> "Idle"
        }

    /**
     * What follows the word, where it says more than the place. Why an agent
     * exited is the row's second line, not this.
     */
    val elaboration: String?
        get() = when (this) {
            Unsupported -> "update amux to open it"
            is HostAway -> "not live"
            else -> null
        }

    /** Whether the word already names the host. */
    val namesTheHost: Boolean
        get() = this is HostOffline || this is HostAway

    val needsYou: Boolean
        get() = this == NeedsYou

    /** The same, as TalkBack says it. */
    val spoken: String?
        get() = when (this) {
            NeedsYou -> "Needs you"
            Working -> "Working"
            Starting -> "Starting"
            is HostOffline -> "$machine is offline"
            is HostAway -> "$machine is away, not live"
            Unsupported -> "Unknown agent, update amux to open it"
            is Exited -> when (cause) {
                ExitCause.Finished -> "Finished"
                is ExitCause.Failed -> null // The second line says it, with why.
                ExitCause.Ended -> "Stopped"
            }
            Settled -> "Settled"
            Idle -> "Idle"
        }

    /** What a test driver reads the row's state as. */
    val name: String
        get() = when (this) {
            NeedsYou -> "needs-you"
            Working -> "working"
            Starting -> "starting"
            is HostOffline -> "host-offline"
            is HostAway -> "host-away"
            Unsupported -> "unsupported"
            is Exited -> "exited"
            Settled -> "settled"
            Idle -> "idle"
        }

    companion object {
        /**
         * Read once, in one order: unreadable outranks everything, then how an
         * exited agent ended, then an unreachable host (outranking `needsYou`,
         * since a request on an unreachable host cannot be answered), then the
         * agent's live attention.
         */
        fun of(row: FleetRow): RowState {
            if (!row.readable) return Unsupported
            if (row.attention == Attention.Exited) {
                val cause = row.card.exitCause ?: ExitCause.Ended
                return when {
                    cause is ExitCause.Failed -> Exited(cause)
                    row.closed -> Settled
                    else -> Exited(cause)
                }
            }
            val machine = PlaceNames.host(row.hostName)
            when (row.hostReach) {
                is Reach.Online -> Unit
                Reach.Away -> return HostAway(machine)
                is Reach.Offline -> return HostOffline(machine)
            }
            return when (row.attention) {
                Attention.NeedsYou -> NeedsYou
                Attention.Working -> Working
                Attention.Starting -> Starting
                Attention.Exited -> Exited(row.card.exitCause ?: ExitCause.Ended)
                Attention.Idle -> Idle
            }
        }
    }
}
