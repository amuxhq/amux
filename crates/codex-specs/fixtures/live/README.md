# Codex live-run captures

Recordings of Codex CLI 0.166.0 taken by amux's live qualification run
(`just live codex`), on the account's default model. No specification
produced them: amux's own interpreter drove each session over the
app-server socket, so no spec can replay or regenerate them, and a later
live run replaces them.

This is the whole Codex corpus: every recording of Codex that the
interpreter's `codex` goldens, the view goldens, the fakes' conformance and
scripted runs and the strict decode tests read. Each folder is named after
the live scenario that recorded it, except `turn_round_trip` (the `respond`
scenario) and `approval_allow` (`decide`).

| capture | what it holds |
|---|---|
| `turn_round_trip` | one prompt answered |
| `approval_allow` | a command in a read-only sandbox, asked about and allowed once |
| `approval_deny` | the same, denied |
| `approval_scopes` | a command allowed with the rule for similar commands, then file changes allowed for the session |
| `automatic_review` | under amux's `auto` permission, a command decided by Codex's reviewer |
| `access_grant` | network access asked for with Codex's permissions tool and granted for the turn, under a granular approval policy in the agent's Codex config |
| `interrupt` | a long reply interrupted |
| `resume` | one turn, the agent stopped and resumed from amux, then a second turn on the same thread |
| `inject_idle` | a message sent to the idle agent |
| `inject_drain` | a message sent while a command runs, answered by the running turn |
| `steer_folded` | two messages queued while a command runs: one sent into the running turn, the other run as the next turn |
| `two_assistant_messages` | a working note, then the final answer |
| `reasoning_summary` | a turn at high effort, with detailed summaries in the agent's Codex config |
| `exploring` | a file read, the files listed and a search |
| `failing_command` | a command that exits non-zero |
| `file_changes` | a file added, changed, and another deleted, with apply_patch |
| `file_moved` | a file renamed and its line changed in one apply_patch hunk |
| `image` | an image attached to a prompt, then looked at in the project |
| `compaction` | a turn, then `/compact` |
| `turn_error` | the model changed from amux to one that does not exist, then a turn |
| `turn_retries` | a provider that cannot be reached, named in the agent's Codex config |
| `signed_out` | a turn with no login in the agent's Codex home |
| `web_search` | a web search, with live search on in the agent's Codex config |
| `subagent` | a child thread spawned and waited for |
| `background_terminal` | a command left running, written to and read |
| `tool_server_form` | a tool server's tool approved, its form accepted and its link cancelled |
| `plan` | a plan for a one-line change, approved from amux, and the change made |
| `questions` | two questions asked together while planning, the first skipped and the second answered, then a third replied to instead |
| `usage` | one turn, then the usage windows read |
| `permission` | the agent made read-only from amux |
| `mode` | the agent put in plan mode from amux |
| `attach` | Codex's own app attached before any turn: its prompt, and the approval it led to answered from amux |

A scenario that needs a setting amux names none for writes it into the
scratch Codex home's `config.toml` for as long as it runs, as a person's
own config would hold it. The tool server the form scenario configures is
`codex-probe` run as one (`codex-probe __tool-server`): its one tool asks
the person to confirm a word with a form, or, for the word `LINK`, with a
link.

Each folder is sanitized like a recording: `io.jsonl`, `spawn.jsonl` and a
`manifest.json` naming the Codex version and model the capture shows. Only
amux's own client's lines join: what Codex's app caused in `attach` reaches
amux as what Codex tells it. `codex-probe join <capture dir> <name>` writes
one from the live run's `target/live/recordings/codex/<scenario>/`.
