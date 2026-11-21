package sh.amux.words

import sh.amux.values.AgentKey
import sh.amux.values.Attention
import sh.amux.values.Filing
import sh.amux.values.FleetRow
import sh.amux.values.Kind
import sh.amux.values.Reach
import java.time.Instant

val FleetRow.id: AgentKey
    get() = card.agent

val FleetRow.name: String
    get() = card.name.ifEmpty { "agent" }

val FleetRow.attention: Attention
    get() = card.attention

val FleetRow.children: Int
    get() = card.children

val FleetRow.hostName: String
    get() = card.host

/** How this phone reaches the agent's host. */
val FleetRow.hostReach: Reach
    get() = card.hostReach

val FleetRow.workingDirectory: String
    get() = card.cwd

/** The branch its folder is on, when it is on one. */
val FleetRow.branch: String?
    get() = card.branch

/** When the agent's state last changed. */
val FleetRow.lastActivity: Instant
    get() = Instant.ofEpochMilli(card.phaseSinceMs)

/** A kind this build knows how to open. */
val FleetRow.readable: Boolean
    get() = card.kind != Kind.Unspecified

val FleetRow.needsYou: Boolean
    get() = attention == Attention.NeedsYou

/** How the person filed it, apart from its lifecycle. */
val FleetRow.filing: Filing
    get() = card.filing

val FleetRow.pinned: Boolean
    get() = card.filing == Filing.Pinned

val FleetRow.closed: Boolean
    get() = card.filing == Filing.Closed

/** It has no parent, so it may be pinned. */
val FleetRow.pinnable: Boolean
    get() = card.topLevel && !card.native && !closed

/**
 * A subagent its parent's provider runs: it goes with its parent and
 * has no folder or terminal, so it is never pinned, deleted, resumed or
 * opened in a terminal on its own.
 */
val FleetRow.native: Boolean
    get() = card.native

/** Settled, and not a provider subagent, which goes with its parent. */
val FleetRow.deletable: Boolean
    get() = closed && !card.native

/** A folded family's head speaking for a member that needs the person. */
val FleetRow.speaksForAMember: Boolean
    get() = loud != null

fun FleetRow.age(at: Instant): String =
    Elapsed.spelled(maxOf(0L, at.toEpochMilli() - card.phaseSinceMs) / 1_000.0)
