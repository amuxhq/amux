package sh.amux.words

import sh.amux.values.CallPhase
import sh.amux.values.Composer
import sh.amux.values.ExitCause
import sh.amux.values.GroupState
import sh.amux.values.JobStop
import sh.amux.values.Kind
import sh.amux.values.OpenedAgent
import sh.amux.values.PhaseView

/**
 * Where an agent a call opened stands, as the call's row says it.
 *
 * A call can return while the agent it started runs on, and only the fleet
 * knows of an ask that agent waits on, so the agent's fleet row decides this
 * whenever this client's fleet holds it. The call's own state stands in only
 * when the fleet does not, or for a call from history that names no agent.
 */
enum class AgentStanding {
    Running,
    NeedsYou,

    /** Live and waiting for its parent, as an amux child between turns. */
    Idle,
    Done,
    Stopped,
    Failed,
    ;

    /** `"running"`, `"waiting on you"`, `"done"`, as the row's meta. */
    val words: String
        get() = when (this) {
            Running -> "running"
            NeedsYou -> "waiting on you"
            Idle -> "idle"
            Done -> "done"
            Stopped -> "stopped"
            Failed -> "failed"
        }

    /** Still at work, so a row draws it bright. */
    val live: Boolean
        get() = this == Running

    companion object {
        fun of(phase: PhaseView): AgentStanding = when (phase) {
            PhaseView.Starting, PhaseView.Working -> Running
            PhaseView.NeedsYou -> NeedsYou
            PhaseView.Idle -> Idle
            is PhaseView.Exited -> when (phase.cause) {
                ExitCause.Finished -> Done
                ExitCause.Ended -> Stopped
                is ExitCause.Failed -> Failed
            }
        }

        fun of(call: CallPhase): AgentStanding = when (call) {
            CallPhase.Pending, CallPhase.Running -> Running
            CallPhase.Asking -> NeedsYou
            CallPhase.Succeeded -> Done
            CallPhase.Failed -> Failed
            CallPhase.Cancelled, CallPhase.Denied -> Stopped
        }

        /** The agent [opened] when the fleet holds it, else the call's [call]. */
        fun of(opened: OpenedAgent?, call: CallPhase): AgentStanding =
            opened?.let { of(it.phase) } ?: of(call)
    }
}

/** No agents at all: what a chat whose rows open none comes to. */
val EMPTY_GROUP_STATE: GroupState = GroupState(
    agents = 0,
    running = 0,
    needsYou = 0,
    idle = 0,
    finished = 0,
    ended = 0,
    failed = 0,
    unknown = 0,
)

data class SubagentGroupPart(
    val count: Int,
    val standing: AgentStanding,
)

/** `"4 subagents"`: the one row the calls of a turn fold into. */
fun ChatWords.subagentGroup(count: Int): String =
    if (count == 1) "1 subagent" else "$count subagents"

/**
 * `"2 running · 1 waiting on you · 1 done"`: what a group's agents come to,
 * by where each stands, in the order a reader acts on them.
 */
fun ChatWords.subagentGroupState(state: GroupState): String =
    subagentGroupParts(state).joinToString(" · ") { "${it.count} ${it.standing.words}" }

/**
 * `"3 subagents · 2 running · 1 waiting on you"`: the activity line while the
 * agent waits on its subagents and one of them waits on the person.
 */
fun ChatWords.subagentActivity(live: GroupState): String =
    "${subagentGroup(live.agents)} · ${subagentGroupState(live)}"

/**
 * The counts [subagentGroupState] says, apart, so a client can ink each.
 * Agents this client's fleet does not hold yet are left unsaid rather than
 * guessed at.
 */
fun ChatWords.subagentGroupParts(state: GroupState): List<SubagentGroupPart> =
    listOf(
        SubagentGroupPart(state.running, AgentStanding.Running),
        SubagentGroupPart(state.needsYou, AgentStanding.NeedsYou),
        SubagentGroupPart(state.idle, AgentStanding.Idle),
        SubagentGroupPart(state.finished, AgentStanding.Done),
        SubagentGroupPart(state.ended, AgentStanding.Stopped),
        SubagentGroupPart(state.failed, AgentStanding.Failed),
    ).filter { it.count > 0 }

/**
 * The one standing a group's mark takes: the person first, then work under
 * way, then a failure, else done.
 */
fun ChatWords.subagentGroupStanding(state: GroupState): AgentStanding = when {
    state.needsYou > 0 -> AgentStanding.NeedsYou
    state.running > 0 -> AgentStanding.Running
    state.failed > 0 -> AgentStanding.Failed
    state.finished == 0 && state.ended > 0 -> AgentStanding.Stopped
    else -> AgentStanding.Done
}

/**
 * `"Claude runs this subagent"`: the head of a provider subagent's composer,
 * which has no text box because the provider lets only the parent's model
 * talk to it.
 */
fun ChatWords.nativeRunner(kind: Kind?): String = when (kind) {
    Kind.ClaudeSdk, Kind.ClaudePty -> "Claude runs this subagent"
    Kind.Codex -> "Codex runs this subagent"
    Kind.Unspecified, null -> "Its provider runs this subagent"
}

/** Under the head: why there is nowhere to type. */
const val NATIVE_TAKES_NO_MESSAGES: String = "It takes no messages."

val ChatWords.nativeTakesNoMessages: String
    get() = NATIVE_TAKES_NO_MESSAGES

/**
 * A provider subagent's own Stop, when the chat is one; null for every other
 * chat, which has a text box.
 */
val Composer.nativeStop: JobStop?
    get() = (this as? Composer.Native)?.stop
