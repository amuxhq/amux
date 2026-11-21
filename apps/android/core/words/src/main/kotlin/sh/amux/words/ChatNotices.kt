package sh.amux.words

import sh.amux.values.ClientCommand
import sh.amux.values.Kind
import sh.amux.values.Refusal
import sh.amux.values.RefusalReason

/** What a chat says when something it was asked to do did not happen, in the iPhone app's words. */
object ChatNotices {
    const val NOT_CONFIRMED: String = "The connection dropped before the agent answered."
    const val CLOSED: String = "This chat is closed."

    /** Why older rows did not arrive once the chat closed under the request. */
    const val PAGING_CLOSED: String = "the chat is closed"

    /** The name a long paste takes as an attachment. */
    const val PASTED_TEXT: String = "Pasted text"

    fun overLimit(name: String, megabytes: Long): String =
        "$name is not attached because it is over the $megabytes MB limit for attachments."

    fun notAttached(name: String, why: String): String = "$name is not attached: $why"

    fun notSent(refused: Refusal): String = "Not sent: ${refusal(refused)}"

    fun refusal(refused: Refusal): String {
        refused.detail?.let { return it }
        return when (refused.reason) {
            RefusalReason.Exited -> "it had exited"
            RefusalReason.Exiting -> "it was exiting"
            RefusalReason.AgentClosed -> "it is settled"
            RefusalReason.Draining -> "its host is shutting down"
            RefusalReason.HostUnreachable -> "its host is away"
            RefusalReason.Unsupported -> "this agent can’t take it"
            RefusalReason.ClosedAsk -> "the ask had already closed"
            RefusalReason.NotQueued -> "it was no longer queued"
            RefusalReason.ConversationElsewhere ->
                "Claude’s own view switched to another conversation"
            RefusalReason.TooLong -> "it is longer than the agent takes"
            RefusalReason.Unspecified -> "it was refused"
        }
    }

    /** A slash command the agent does not run, with what it does run or the mention it takes instead. */
    fun refusedCommand(
        command: String,
        offered: List<String>,
        hint: String? = null,
        kind: Kind? = null,
    ): String {
        val name = if (command.startsWith("/")) command else "/$command"
        val agent = if (kind == Kind.Codex) "Codex" else "This agent"
        if (hint != null) {
            return "$agent takes a skill as a mention: type $hint."
        }
        val runs = offered.map { item ->
            if (item.startsWith("/") || item.startsWith("$")) item else "/$item"
        }
        return if (runs.isEmpty()) {
            "$agent does not run $name here."
        } else {
            "$agent does not run $name here. It runs ${runs.joinToString(", ")}."
        }
    }

    /** A client command naming a value the agent does not offer. */
    fun unknownCommand(
        command: ClientCommand,
        value: String,
        offered: List<String>,
    ): String {
        val list = offered.joinToString(", ")
        return when (command) {
            ClientCommand.Effort -> if (offered.isEmpty()) {
                "No effort level named $value."
            } else {
                "No effort level named $value. It offers $list."
            }
            ClientCommand.Model,
            ClientCommand.Rename,
            -> if (offered.isEmpty()) {
                "No model named $value."
            } else {
                "No model named $value. It offers $list."
            }
        }
    }
}
