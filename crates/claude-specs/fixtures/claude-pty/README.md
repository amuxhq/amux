# Claude terminal transcript captures

Two Claude transcripts that no `fixtures/live/pty` capture covers,
kept as inputs for the interpreter's `claude_pty` tests
(`crates/interpret/fixtures/claude_pty/recorded_*.json` name them as
`transcript_rows` recordings):

| capture | what it holds |
|---|---|
| `socket_delivery` | two messages delivered to Claude while idle and while busy, with the hook payloads inline |
| `task_tools` | six unmodified transcript rows: TaskCreate, TaskUpdate, an auto-denied Write and their results |

Each `.meta.json` sidecar records the Claude version and where the capture came
from. They cannot be regenerated in-tree; a new capture replaces them.
