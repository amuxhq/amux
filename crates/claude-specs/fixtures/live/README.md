# Claude live-run captures

Recordings taken by amux's live qualification run
(`just live claude_sdk`, `just live claude_pty`), on the account's own
sign-in. No specification produced them: amux's own interpreter drove each
session, so no spec can replay or regenerate them, and a later live run
replaces them.

`sdk/` is the whole headless Claude corpus: every recording of headless
Claude that the interpreter's `claude_sdk` goldens, the view goldens, the
fakes' conformance and scripted runs and the strict decode tests read.
Each folder is named after the live scenario that recorded it, except
`text_turn` (the `respond` scenario), `permission_callback` (`decide`),
`interrupted` (`interrupt`) and `resumed` (`resume`).

| capture | what it holds |
|---|---|
| `sdk/text_turn` | one prompt answered |
| `sdk/multi_turn` | two prompts, the second answered from the first |
| `sdk/compacted` | two turns, then `/compact` |
| `sdk/cleared` | a turn, then the conversation cleared from amux |
| `sdk/controls` | the permission set to accept edits and the model changed from amux, then a turn that creates a file |
| `sdk/effort` | a turn at a launch effort, the effort changed from amux, a second turn |
| `sdk/configured_turn` | a turn with Claude's thinking shown, on Haiku 4.5 (`--max-thinking-tokens`, `--thinking-display summarized`) |
| `sdk/permission_callback` | a command asked about and allowed once |
| `sdk/interrupted` | a long reply interrupted |
| `sdk/resumed` | a turn, the agent stopped and resumed, a second turn |
| `sdk/plan` | a plan for a one-line change, approved from amux, and the change made |
| `sdk/plan_reviewed` | a plan sent back with a note, the revised plan approved switching the permission, and the change made |
| `sdk/question_asked` | one question answered with an option |
| `sdk/question_every_shape` | four questions in one ask: single and multiple choice, previews, an answer of the person's own |
| `sdk/question_dismissed` | a question left open and the turn interrupted |
| `sdk/questions` | two questions asked together, the first skipped and the second answered, then a third replied to instead |
| `sdk/elicitation_accepted` | a tool server's tool asked about and allowed, then its form accepted with content |
| `sdk/elicitation_declined`, `_cancelled` | a tool server's form declined, cancelled |
| `sdk/elicitation_link_accepted` | a tool server's link accepted |
| `sdk/failed_tool_server` | a tool server whose command cannot start |
| `sdk/failing_command` | a command that exits non-zero |
| `sdk/image` | an image attached to a prompt, then read back from the project |
| `sdk/task_list` | the task tools: three tasks created and one worked through |
| `sdk/background_shell` | a shell run in the background and its output read |
| `sdk/subagent_task` | a task delegated to a subagent given at launch (`--agents`) |
| `sdk/sign_in_problem` | a rejected credential, set through the agent's `--settings` |
| `sdk/steer_folded` | a message sent into the running turn, and another left queued for the next |
| `sdk/usage` | one turn, then the usage windows read |
| `pty/plan` | the same plan, approved from amux on terminal Claude's permission hook (Claude 2.1.312) |
| `pty/questions` | the same questions, answered from amux on terminal Claude's permission hook (Claude 2.1.312) |

The tool server the form scenarios ask through is `claude-probe` run as
one (`claude-probe __tool-server`): its one tool asks the person to
confirm a word with a form, or, for the word `LINK`, with a link.

Each folder is sanitized like a recording: `io.jsonl`, `spawn.jsonl` and a
`manifest.json` naming the Claude version and model the capture shows. A
terminal capture leaves out the headless Claude the agent asks for the
host's catalogue, whose answer lists the owner's plugins, skills and agents.
`claude-probe join <capture dir> <name>` writes one from the live run's
`target/live/recordings/<kind>/<scenario>/`.
