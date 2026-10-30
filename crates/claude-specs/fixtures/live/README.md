# Claude live-run captures

Recordings taken by amux's live qualification run
(`just live claude_sdk`, `just live claude_pty`), on the account's own
sign-in. Unlike `fixtures/sdk` and `fixtures/pty`, no specification
produced them: amux's own interpreter drove each session, so no spec can
replay or regenerate them, and they are not in either registry. A later
live run replaces them.

| capture | what it holds |
|---|---|
| `sdk/plan` | a plan for a one-line change, approved from amux, and the change made |
| `sdk/questions` | two questions asked together, the first skipped and the second answered, then a third replied to instead |
| `sdk/usage` | one turn, then the usage windows read |
| `pty/plan` | the same plan, approved from amux on terminal Claude's permission hook (Claude 2.1.312) |
| `pty/questions` | the same questions, answered from amux on terminal Claude's permission hook (Claude 2.1.312) |

Each folder is sanitized like a recording: `io.jsonl`, `spawn.jsonl` and a
`manifest.json` naming the Claude version and model the capture shows; the
headless captures are of Claude 2.1.292. A terminal capture leaves out the
headless Claude the agent asks for the host's catalogue, whose answer lists
the owner's plugins, skills and agents.
`claude-probe join <capture dir> <name>` writes one from the live run's
`target/live/recordings/<kind>/<scenario>/`. The strict decode tests, the
fakes' conformance run and the interpreter's `recorded_live_*` goldens read
them.
