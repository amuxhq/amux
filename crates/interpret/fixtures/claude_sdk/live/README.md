# Headless Claude live-run captures

Recordings taken by amux's live qualification run (`just live claude_sdk`),
on the account's own sign-in. No specification produced them: amux's own
interpreter drove each session, so no spec can replay or regenerate them,
and a later live run replaces them.

This is the whole headless Claude corpus: every recording of headless
Claude that the interpreter's `claude_sdk` goldens (the `recorded_*.json`
fixtures beside this folder), the view goldens, the fakes' conformance and
scripted runs and the strict decode tests read. Each folder is named after
the live scenario that recorded it, except `text_turn` (the `respond`
scenario), `permission_callback` (`decide`), `interrupted` (`interrupt`)
and `resumed` (`resume`). The terminal corpus is
[`../../claude_pty/live`](../../claude_pty/live/README.md).

| capture | what it holds |
|---|---|
| `text_turn` | one prompt answered |
| `multi_turn` | two prompts, the second answered from the first |
| `compacted` | two turns, then `/compact` |
| `cleared` | a turn, then the conversation cleared from amux |
| `controls` | the permission set to accept edits and the model changed from amux, then a turn that creates a file |
| `effort` | a turn at a launch effort, the effort changed from amux, a second turn |
| `configured_turn` | a turn with Claude's thinking shown, on Haiku 4.5 (`--max-thinking-tokens`, `--thinking-display summarized`) |
| `permission_callback` | a command asked about and allowed once |
| `interrupted` | a long reply interrupted |
| `resumed` | a turn, the agent stopped and resumed, a second turn |
| `plan` | a plan for a one-line change, approved from amux, and the change made |
| `plan_reviewed` | a plan sent back with a note, the revised plan approved switching the permission, and the change made |
| `question_asked` | one question answered with an option |
| `question_every_shape` | four questions in one ask: single and multiple choice, previews, an answer of the person's own |
| `question_dismissed` | a question left open and the turn interrupted |
| `questions` | two questions asked together, the first skipped and the second answered, then a third replied to instead |
| `elicitation_accepted` | a tool server's tool asked about and allowed, then its form accepted with content |
| `elicitation_declined`, `_cancelled` | a tool server's form declined, cancelled |
| `elicitation_link_accepted` | a tool server's link accepted |
| `failed_tool_server` | a tool server whose command cannot start |
| `failing_command` | a command that exits non-zero |
| `image` | an image attached to a prompt, then read back from the project |
| `task_list` | the task tools: three tasks created and one worked through |
| `background_shell` | a shell run in the background and its output read |
| `subagent_task` | a task delegated to a subagent given at launch (`--agents`) |
| `sign_in_problem` | a rejected credential, set through the agent's `--settings` |
| `steer_folded` | a message sent into the running turn, and another left queued for the next |
| `usage` | one turn, then the usage windows read |

Tool-server forms are recorded headless only: a terminal capture does not
hold what amux answered on a hook, so its replay would read the form as
dismissed. The tool server the form scenarios ask through is
`claude-probe` run as one (`claude-probe __tool-server`): its one tool asks
the person to confirm a word with a form, or, for the word `LINK`, with a
link.

Each folder is sanitized like a recording: `io.jsonl`, `spawn.jsonl` and a
`manifest.json` naming the Claude version and model the capture shows (a
session whose every request failed names none, and the model it was
launched on stands in). `claude-probe join <capture dir> <name>` (a
binary of `crates/qualification`) writes one from the live run's
`target/live/recordings/claude_sdk/<scenario>/`.
