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

A terminal capture holds what Claude drew and what was typed into its
terminal, its hook payloads and its transcript rows, but not what amux
answered on a hook: its replay reads a tool server's form, or a question
amux replied to instead, as dismissed. Tool-server forms are therefore
recorded headless only.

Each folder is sanitized like a recording: `io.jsonl`, `spawn.jsonl` and a
`manifest.json` naming the Claude version and model the capture shows (a
session whose every request failed names none, and the model it was
launched on stands in). A terminal capture leaves out the headless Claude
the agent asks for the host's catalogue, whose answer lists the owner's
plugins, skills and agents.
`claude-probe join <capture dir> <name>` (a binary of
`crates/qualification`) writes one from the live run's
`target/live/recordings/claude_pty/<scenario>/`.
