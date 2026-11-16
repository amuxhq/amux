# Terminal Claude live-run captures

Recordings taken by amux's live qualification run (`just live claude_pty`),
on the account's own sign-in. No specification produced them: amux's own
interpreter drove each session, so no spec can replay or regenerate them,
and a later live run replaces them.

This is the whole terminal Claude corpus: every recording of terminal
Claude that the interpreter's `claude_pty` goldens (the `recorded_*.json`
fixtures beside this folder), the view goldens, the fakes' conformance and
scripted runs and the strict decode tests read. Two transcripts no capture
covers are in [`../transcripts`](../transcripts/README.md).

The corpus, recorded on Claude 2.1.316 on Haiku 5.5 unless a row
says otherwise, holds the scenarios of the same names in the
[headless corpus](../../claude_sdk/live/README.md) (the same exceptions in
naming) where terminal Claude has them: `text_turn`,
`permission_callback`, `interrupted` (a running command interrupted),
`resumed`, `plan`, `questions`, `multi_turn`, `compacted` (`/compact`
typed), `cleared`, `configured_turn`, `question_asked`,
`question_every_shape`, `question_dismissed`, `plan_reviewed`,
`failing_command`, `task_list`, `background_shell`, `subagent_task`,
`image`, `sign_in_problem` (the rejected key printed by a key helper the
agent's `--settings` names, which Claude takes without asking) and
`steer_folded`; and what only terminal Claude does:

| capture | what it holds |
|---|---|
| `mode_cycle` | the permission mode cycled from amux, then a turn |
| `long_prompt` | a prompt of many lines, far past the length Claude collapses a paste at |
| `api_error` | a provider that cannot be reached (`ANTHROPIC_BASE_URL` on the discard port, in the agent's `--settings`): Claude retries, then writes an error |
| `permission_scoped` | a command allowed from amux with the first scope Claude offered |
| `permission_denied` | a command denied from amux with a note |
| `terminal_allow` | a command asked about, allowed by typing 1 in Claude's own terminal (`amux attach --in native`) |
| `terminal_escape` | a command asked about, refused with Escape in Claude's own terminal |
| `terminal_question` | a question answered by typing 2 in Claude's own terminal |

Claude's own subagents (its Agent tool), recorded on Claude 2.1.325, have
the names of the headless corpus's subagent scenarios. Claude 2.1.325
starts every subagent in the background: the Agent call returns at once,
the parent's turn ends, and each subagent's end starts a turn of its own
with a task notification.

| capture | what it holds |
|---|---|
| `subagents_parallel` | three subagents launched in one message, each reading a file and listing the folder |
| `subagent_nested` | a subagent that launches its own; the inner one's report continues the outer one, which then reports to the parent |
| `subagent_ask` | a subagent asks before a command, and amux allows it on the hook |
| `subagent_stopped` | a background subagent outlives an interrupted turn; the parent then stops it with `TaskStop` |
| `subagent_stopped_foreground` | a subagent the parent waits on, interrupted from amux; background tasks are off in the agent's `--settings` (`CLAUDE_CODE_DISABLE_BACKGROUND_TASKS`), or Claude starts it in the background |
| `subagent_continued` | a subagent that answered, continued by its parent with `SendMessage` |
| `subagent_resumed` | a subagent running when its host is settled, then the host resumed |
| `subagent_amux_tools` | a subagent calling amux's `status` and `agents` tools |
| `subagent_elicitation` | a subagent's tool server asking a question mid-call, answered from amux |

A terminal capture holds what Claude drew and what was typed into its
terminal (`pty` lines), its hook payloads (`hook`) and its transcript rows
(`transcript`), but not what amux answered on a hook: its replay reads a
tool server's form, or a question amux replied to instead, as dismissed.
Tool-server forms are therefore recorded headless only, but for
`subagent_elicitation`, kept for the hook payload a subagent's form raises.

What Claude writes for each subagent is in `subagent` lines: a row of
`<session>/subagents/agent-<id>.jsonl`, or the whole `agent-<id>.meta.json`
beside it each time Claude rewrites it, named by the session's transcript
(`<TRANSCRIPT_1>/subagents/agent-<id>.jsonl`). These captures were made
before amux registered the `SubagentStart` hook, so the recorder registered
one that only records its payload, and their `hook` lines for it are the
recorder's. A replay reads `subagent` lines as the agent process delivers
them: a file's meta before its first row, its rows on the subagent channel,
and the file drained once its last recorded row is read. What passed between
Claude and each tool server (amux's own, and the probe's in the form
scenarios) is in `mcp:<server>` lines, `stdin` what Claude sent: a call's
`_meta` is there. The fakes play `subagent` lines, writing the files where
Claude would, and leave `mcp:` lines alone; the interpreter reads no `mcp:`
line.

Each folder is sanitized like a recording: `io.jsonl`, `spawn.jsonl` and a
`manifest.json` naming the Claude version and model the capture shows (a
session whose every request failed names none, and the model it was
launched on stands in). A terminal capture leaves out the headless Claude
the agent asks for the host's catalogue, whose answer lists the owner's
plugins, skills and agents.
`claude-probe join <capture dir> <name>` (a binary of
`crates/qualification`) writes one from the live run's
`target/live/recordings/claude_pty/<scenario>/`.
