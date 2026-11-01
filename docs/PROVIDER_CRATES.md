# Provider crates

*For developers changing how amux talks to Claude Code or Codex.*

Each provider is split into three layers, so that what the provider says,
what amux does with it, and how amux reaches it can each change on its
own. The layers are a protocol crate, an interpreter module and an agent
host module, one of each per provider; neither provider's protocol crate
or host module uses the other's.

| Layer | Claude | Codex | Holds |
|---|---|---|---|
| Protocol crate | [`claude-protocol`](../crates/claude-protocol/src/lib.rs) | [`codex-protocol`](../crates/codex-protocol/src/lib.rs) | Every message in both directions, as types, with a decoder and an encoder. Nothing else. |
| Interpreter module | [`interpret::claude_pty`](../crates/interpret/src/claude_pty/mod.rs) and [`interpret::claude_sdk`](../crates/interpret/src/claude_sdk/mod.rs) | [`interpret::codex`](../crates/interpret/src/codex/mod.rs) | Deciding what to send and what what was received means. |
| Agent host module | [`agent::claude`](../crates/agent/src/claude.rs) | [`agent::codex`](../crates/agent/src/codex.rs) | Starting the provider and carrying its bytes: launch arguments, the process, the terminal, sockets, the transcript file, the hook command. |

## Protocol

A protocol crate is pure: it depends on `serde` and `serde_json` alone, and
the dependency policy refuses anything else, so the interpreters the phone
links can depend on it. It holds no process, socket or async code.

- `codex-protocol` types Codex's app-server JSON-RPC: what Codex sends
  (`ServerMessage`: notifications, requests for approval, input, tool calls
  and elicitations, and answers to amux's requests) and what amux sends
  (`ClientMessage`: requests, the `initialized` notification, and answers).
  An answer's result stays JSON until it is read as the response type of the
  request it answers, with `codex_protocol::result`.
- `claude-protocol` types three things Claude Code produces: its headless
  stream (`stream`: the frames it prints and the control requests and
  answers in both directions), the rows of the transcript file it writes in a
  terminal (`transcript`), and the payloads its hook command forwards
  (`hooks`). It also types the two lines amux writes to Claude's messaging
  socket (`messaging`).

Decoding is tolerant, because a provider update must not stop a running
agent. A message the crate does not know is kept whole as `Unknown`; an
unknown kind of a nested object, or an unknown spelling of a string value,
is kept as written; unknown fields of a known message are kept in its
`extra` (Codex) or `extensions` (Claude) map. Codex's crate encodes all of
it back, so decoding and then encoding a recorded Codex line gives the same
JSON; Claude's encodes only the lines amux writes, since nothing writes
Claude's output back.
A missing field the provider always writes, which amux's hand-written test
inputs often leave out, reads as empty, zero, or none where amux treats it
as optional. Each crate also has a `strict` decoder that refuses every one
of those fallbacks. The recording checks use it, which is how a change in what a
provider sends shows up: as a failed test naming the line, not as a quiet
`Unknown` in production.

## Interpreter

An interpreter decides what to say and what a message means: it reads
provider facts and decides what to write, and the agent process carries
those writes. The [interpreters page](INTERPRETERS.md) describes them.

No provider has a recording client of its own: amux's agent process drives
headless Claude, terminal Claude and Codex in the live lane, and those
captures, joined by the `claude-probe` and `codex-probe` binaries of
`qualification`, are their corpora.

## Host

A host is a module of the agent process, the one production crate that
reads a provider. It knows how to start the provider and move bytes, and
takes every message type from its protocol crate.

- `agent::claude` builds Claude Code's launch settings and the environment
  it scrubs, carries hook payloads from the hook command to the agent
  process over a socket, reaches Claude's messaging socket, finds its
  session files, and probes its version. The rest of the agent process
  uses it for terminal Claude, and hosts the terminal and follows the
  transcript itself.
- `agent::codex` starts `codex app-server` listening on a socket in the
  agent's folder (stdio on Windows) and carries whole messages to and from
  it as bytes; the agent process and its interpreter build and read them
  with `codex-protocol`. The agent process uses it for every Codex agent
  and for the catalogue probe.

## Who checks what

| Check | Where | What it holds |
|---|---|---|
| Recording corpus | `codex-protocol/tests/corpus.rs`, `claude-protocol/tests/stream_corpus.rs` and `pty_corpus.rs` | Every recorded line decodes strictly (and, for Codex, encodes back to the same JSON), and every recorded answer reads strictly as the response type of the request it answers. |
| Fakes' shape check | [`provider-fakes/src/shape.rs`](../crates/provider-fakes/src/shape.rs) | Every frame a fake provider composes has a recorded shape and decodes strictly with its protocol crate. The fakes never use the agent's host modules, so a host bug cannot hide behind code the fakes share. |
| Dependency policy | `scripts/check-dependency-policy.py` | The protocol crates stay pure, and only the crates listed there use them. |
| Typed provider check | [`scripts/typed-provider-check.py`](../scripts/typed-provider-check.py) | The interpreters, the agent's provider handshake and Claude's messaging socket build and read no provider JSON by hand; each place that keeps a tool payload as JSON is listed with its reason. |
