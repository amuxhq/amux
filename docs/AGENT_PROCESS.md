# The agent process

*For developers changing the agent process or adding a provider.*

Every agent runs in a process of its own: `amux agent <dir>`, a hidden
subcommand of the amux binary, started on the agent's directory. It hosts
exactly one provider child (Claude in a terminal, headless Claude, or a Codex
app server), runs that kind's interpreter over everything the provider says,
and appends the interpreter's steps to the journal in its directory. It never
dials the daemon, never reads the store and never talks to a client over the
network. The directory and the journal are its whole interface; the daemon
dials in over the directory's control socket when it wants something.

Because the agent is its own process, a daemon restart or update does not
interrupt it: the daemon goes away, comes back, dials `ctl.sock` again and
carries on reading the journal where it stopped. If no daemon comes back, the
agent finishes its turn and exits on its own.

![The interpreter runs inside the agent process under one rule: every provider event yields one step, zero or more items plus the snapshot when it changed, and each item is self-contained. What is in private/ is the agent's business; the daemon never reads it.](figures/inside-agent.svg)

How provider facts become items is the subject of
[the interpreters](INTERPRETERS.md). How the daemon reads the journal and
commits it is in [the journal and the store](JOURNAL_AND_STORE.md). This page
covers the process around the interpreter: how it starts, its sockets, its
provider children, and how it ends.

## Where the code lives

| Crate or file | What it holds |
|---|---|
| [`crates/agent`](../crates/agent/src/lib.rs) | The process: `run` and `main` in `lib.rs`; the lifecycle, the control connection and the interpreter loop in `host.rs`; provider children in `provider.rs`; typing into terminal Claude in `terminal.rs`; reading terminal Claude's first screen in `ready.rs`; the facts ring in `ring.rs`; raw attach on `pty.sock` in `attach.rs`; the tool server in `tools.rs`; the agent's part of a dump in `dump.rs`; directory helpers in `dir.rs` |
| [`crates/agent-dir`](../crates/agent-dir/src/lib.rs) | What the agent and the daemon must agree on: the file and socket names, the directory lock, the frame format on `ctl.sock` and `pty.sock` (`ctl.rs`), local sockets (`local_socket.rs`) and the injectable clock (`clock.rs`) |
| [`crates/journal`](../crates/journal/src/lib.rs) | The segmented, append-only journal writer and reader |
| [`crates/pty-host`](../crates/pty-host/src/lib.rs) | Provider-neutral pseudo-terminal hosting: spawn in a process group, output stream, input, resize, signals |
| [`crates/interpret`](../crates/interpret/src/lib.rs) | The per-kind interpreters the process runs |
| [`crates/amux/src/main.rs`](../crates/amux/src/main.rs) | The hidden subcommands a harness runs from the install path: `amux agent <dir>`, `amux mcp <dir>` (the tool server) and `amux hooks claude` (terminal Claude's hook command) |

The daemon's side of the relationship (spawning, dialling, ingesting,
stopping) is the agent registry in
[`crates/node/src/runtime.rs`](../crates/node/src/runtime.rs).

## The agent directory

The directory is `<profile dir>/agents/<agent id>/`. The agent id is amux's
own UUID, stable across resumes and `/clear`; it is never the provider's
session id.

```text
agents/<agent id>/
  spec.1, spec.2, …      one immutable spec per incarnation, written by the daemon
  lock                   held by the agent process for its lifetime
  ctl.sock               the agent listens; the daemon is its only client
  pty.sock               the agent listens; local terminal clients connect
                         (terminal Claude and Codex only)
  tools.sock             the daemon listens; the agent's tool server is its only client
  journal/0000000000 …   steps, in segments named by the offset of their first byte
  pty/0000000000 …       terminal Claude's raw terminal bytes, named the same way
  blobs/<sha256>         bytes items refer to, and what the agent offers as an
                         encoded Catalogue its snapshot names, each named by its hash
  agent.log              the agent process's stderr
  private/               the agent's own state; the daemon never reads it
    hooks.sock           terminal Claude's hook command connects here
    messaging.sock       terminal Claude's messaging socket (Claude binds it)
    codex.sock           Codex's app server listens here; the agent is one client
    facts/               the facts ring and its checkpoints
    transcript-cursor    how far Claude's transcript has been read, and which file
    provider-session     the provider's session or thread id, for the next incarnation
    provider.log         the provider child's stderr
```

`private/` is created with mode 0700. A socket whose path is too long for a
Unix socket address is bound through a short symbolic link in a per-user
runtime directory, so the file still appears at its path; both ends derive
the same address from the same path (`agent_dir::local_socket`). On Windows
each socket is a named pipe made afresh by every bind, and the file at the
path holds its name.

## Starting

The daemon writes `spec.<n>` and starts `amux agent <dir>` detached from
itself: in its own process group, with stdin and stdout closed and stderr
appended to `agent.log`, so neither the daemon's terminal nor its death
reaches the agent.

`agent::run` then:

1. Takes `<dir>/lock`. If another live process holds it, the agent exits with
   code 1 ("another agent process holds …"). The kernel releases the lock
   when the process dies, which is how the daemon tells a live agent from a
   dead one.
2. Reads the newest `spec.<n>` and creates `private/`.
3. Binds `ctl.sock` first, so a daemon can dial in as soon as the process
   exists; then `pty.sock` for terminal Claude and Codex, and
   `private/hooks.sock` for terminal Claude.
4. Opens the journal writer and, for terminal Claude, the terminal log in
   `pty/`, and starts serving `pty.sock`.
5. Builds the interpreter's state: from nothing for a first incarnation, or
   from the facts ring for a later one (see [the facts ring](#the-facts-ring)),
   and journals the steps it starts with.
6. Arms the grace timer, since no daemon is connected yet.
7. Starts the provider child. If it cannot be started, the agent writes a
   final boundary ("could not start the provider: …") and exits.

The spec is frozen for the incarnation. A change to settings affects the next
spawn or resume, never a running agent.

The spec's `bootstrap` is what seeds the new incarnation's queue, if anything
does: the first prompt a create or a resume carried, with its attachments and
input id, or, when a parent's message resumes its exited child, that message.
Only the daemon writes it; what a client sends on create or resume can only be
a prompt.

## Lifecycle

The host in [`host.rs`](../crates/agent/src/host.rs) is a single loop over
five sources: provider events, daemon connections, control frames, keys and
resizes from attached terminals, and the next deadline. The process is in one
of three modes:

| Mode | Meaning |
|---|---|
| `Running` | Ordinary life. Inputs go to the interpreter. |
| `Draining` | On its way out once the turn in progress ends; carries why (the daemon was lost, a graceful stop, an abort). Input is answered `rejected{draining}`. |
| `Exiting` | The final boundary is written and the provider has been asked to finish. Input is answered `rejected{exiting}`. |

These modes are the process's lifecycle. They are not the phase a client
shows (starting, working, needs you, idle), which the interpreter derives
and carries in its snapshots.

![Self-destruct is the Draining path: EOF on ctl.sock starts a grace timer (a parameter, in minutes); if no daemon reconnects, the agent writes a "daemon lost" item, finishes its turn, and exits. The finished turn is in the journal, so a later resume loses nothing. An orphaned agent mid-turn lives as long as the turn, never longer.](figures/agent-states.svg)

### One decision, one way out

After every event the host calls `settle`, the only place that decides
whether this incarnation ends, and every decision to end goes through one
action, `exit(cause)`:

1. feed the interpreter `Exiting { cause }`, which closes every open ask and
   writes the final `Exited` boundary with the cause;
2. close the provider's input: end of input for a child on pipes, a
   terminate signal to the process group for terminal Claude;
3. enter `Exiting` with a ten-second deadline, after which the provider's
   process group is killed.

The provider's own exit then ends the loop. Two paths skip `exit`: the
provider exiting by itself (the interpreter records it as the final boundary,
with the exit code or "killed by a signal"), and a kill stop, which kills the
provider's process group at once and writes nothing.

| While | What happens | Result |
|---|---|---|
| `Running`, no daemon connected | The grace timer runs out | The interpreter writes a daemon-lost boundary; `Draining` (daemon lost) |
| `Running` | `Stop(Graceful)` arrives | `Draining` (graceful) |
| `Running` | `Stop(Abort)` arrives | The interpreter cancels the turn the provider's own way; `Draining` (abort) |
| `Draining` | The turn ends, or no turn is running | `exit` with "daemon lost", "stopped" or "aborted" |
| `Draining` after the daemon was lost | An ask is open (phase needs you) | A drain deadline is armed; when it passes, `exit` with "orphaned while waiting for you" |
| `Running`, the agent has a parent | A turn ends with nothing running, nothing queued and no accepted agent message still unconsumed | `exit` with "finished" (the one-shot rule) |
| Any | The interpreter asks to exit (`Effect::Exit`) | `exit` with the interpreter's cause |
| Any | A write to the directory fails (in practice, a full disk): a journal frame, a blob, or the provider's own state in `private/` that the next incarnation resumes from (Codex's thread id, the transcript position) | `exit` with "could not write to its directory: …"; if even that cannot be written, the provider is killed |
| Any | The provider exits | The loop ends |
| Any | `Stop(Kill)` arrives | The provider's process group is killed; "killed" |
| `Exiting` | Ten seconds pass | The provider's process group is killed |

A dump request is answered in every mode, since a draining agent is often the
one someone wants to look at.

Nothing a person does in a chat stops the process. A chat's stop is the
interrupt input, which cancels the turn and leaves the agent live and idle.
Process stops come from the daemon: `amux stop <agent> --mode
graceful|abort|kill` sends the matching frame (graceful by default), and a
delete sends `Abort`, because a turn allowed to finish into a directory about
to be removed is work thrown away. If a stopped agent overruns the daemon's
stop deadline (30 seconds), the daemon falls back to killing the agent
process's own process group.

The one-shot rule is what having a parent means: there is no flag. An input
that arrives while such an agent is exiting is answered `rejected{exiting}`,
and the daemon then treats it as exited and resumes it if the sender is its
parent. See [agent tools](AGENT_TOOLS.md) for families.

## The control socket

`ctl.sock` carries `CtlFrame` messages
([`agent.proto`](../crates/wire/proto/amux/v1/agent.proto)), each a varint
length followed by the encoded message, the same framing the journal uses for
steps. A frame over 64 MiB is treated as a broken peer. The surface has no
version integer, and a frame a reader does not know is ignored.

| Frame | Direction | Meaning |
|---|---|---|
| `hello` | agent → daemon | Sent at once on every connection: the agent id, the agent's version, and the journal's current end offset |
| `nudge` | agent → daemon | The journal grew. Sent after every step that wrote bytes; an empty step writes nothing and sends nothing |
| `input` | daemon → agent | A person's `Input` (a prompt, an answer, an interrupt, a setting, …), its body as the client encoded it |
| `agent_message` | daemon → agent | An `Envelope` with the sender the daemon set; the recipient's interpreter picks the carrier |
| `dump_request` | daemon → agent | `AgentDumpRequest { dump_id }`: asks for the agent's part of a dump bundle, answered in any mode |
| `rename` | daemon → agent | `Rename { name }`: a Codex agent's thread takes the agent's new name, so Codex's own app shows what amux does; other kinds have nothing to rename |
| `stop` | daemon → agent | `Stop { mode }`: `GRACEFUL`, `ABORT` or `KILL`; a mode this agent cannot read is ignored, never taken as another |
| `dump` | agent → daemon | `DumpPart`, answering a dump request |
| `reply` | agent → daemon | `Verdict { id, verdict }`: the interpreter's verdict on an input, keyed by its input id, which the daemon relays as the `SendInput` reply; or on an agent message, keyed by its envelope id, which settles the message's delivery |

Within one step the order is fixed: blobs the step refers to are written
first, then the step is appended to the journal, then the daemon is nudged,
then the effects run (replies are sent, bytes go to the provider). A reply
therefore never reaches the daemon before the step that explains it is on
disk. Before each event, when the clock has moved since the last one, the
host feeds the interpreter a `Tick`, so the interpreter never reads a clock
itself.

A later connection takes over from an earlier one: a restarted daemon may
dial in before the previous daemon's socket has closed. Frames from a
superseded connection are ignored, and frames for a daemon that is not
connected are dropped, since whoever sent the input is gone with it.

End of stream on the current connection means the daemon went away, because
a deliberate stop is always a `Stop` frame. In `Running` mode it arms the
grace timer; a daemon that dials in before the timer runs out cancels it.

## Provider children

Every child runs in its own process group, so a kill reaches whatever it
started, with its working directory, command and arguments from the spec. Its
environment is the agent's, minus the variables a parent Claude session sets
(`claude::launch::CHILD_SESSION_ENV_SCRUB`), plus the spec's additions. Its
stderr goes to `private/provider.log`.

The child's exit ends the agent, never the end of its output. A straggler the
provider left behind can hold a terminal or a pipe open forever, so after the
exit the output already on its way is collected for 300 ms and the reader is
abandoned, not joined. Before the exit is recorded, the agent feeds the
interpreter everything the provider said first: events already queued, then
any transcript rows a follower has not read yet.

A provider's background jobs do not outlive a clean exit: Codex and headless
Claude end them when their input closes. A provider process killed on its
own leaves them running, orphaned, and terminal Claude's `/exit` lets the
person move the session and its jobs into Claude's own background, where
they keep running after the pane closes. Either way the agent's list of
jobs is emptied at the exit ([background jobs](INTERPRETERS.md#background-jobs)).

| Kind | Child | Facts | Input |
|---|---|---|---|
| `claude_pty` | `claude` in a pseudo-terminal | Hook payloads on `private/hooks.sock`, transcript rows, and the agent's own launch, trust-dialog and ready facts | Keystrokes, typed through the keymap resolved for the running Claude |
| `claude_sdk` | `claude --print` over stream-JSON on stdin and stdout | Each stdout line | Stream-JSON lines on stdin |
| `codex` | `codex app-server` listening on `private/codex.sock` (stdio on Windows), the agent one of its clients | Each JSON-RPC message the server sends the agent | JSON-RPC messages from the agent's client |

Terminal Claude is hosted on Unix only. On Windows an agent of kind
`claude_pty` ends before spawning anything, with the cause "could not start
the provider: terminal Claude is not hosted on Windows in this build;
headless Claude and Codex are"; the other two kinds run there as here.
[Windows, as a stated cost](ARCHITECTURE.md#windows-as-a-stated-cost) gives
the reasons.

### Terminal Claude

Before Claude starts, the agent runs `claude --version` and resolves the
keymap for that version (see
[keys as programs](INTERPRETERS.md#keys-as-programs)). It reports both as its
launch fact, the first fact the interpreter sees, and the boundary item
records them. No keymap covering the version means the agent runs but cannot
type into Claude.

Claude is started through `pty_host::spawn` with the spec's arguments plus:

- `--session-id <id>` for the first incarnation, `--resume <id>` for later
  ones. The id is a UUID the agent makes up and keeps in
  `private/provider-session`. A session Claude never began (it exited at its
  folder-trust dialog, say) has no transcript and cannot be resumed, so it is
  started again under the same id.
- `--settings <json>`: the user's own settings with the agent's merged over
  them. The agent's part registers `<install path> hooks claude` as the hook
  command for `SessionStart`, `SessionEnd`, `UserPromptSubmit`,
  `PermissionRequest`, `PreToolUse`, `PostToolUse`, `PostToolUseFailure`,
  `Stop` and `Notification` (unless the spec's hook policy names others),
  accepts messages from other sessions at once, and allows amux's own tools
  without asking.
- `--mcp-config` naming amux's tool server (see [the tool server](#the-tool-server)).
- `--messaging-socket-path private/messaging.sock`.

Its environment also carries `CLAUDE_HOOK_SOCKET`, the address of
`private/hooks.sock`.

Every byte Claude draws is appended to `pty/` and watched for two things on
its first screen. If the screen is Claude's folder-trust dialog, the agent
sends a trust-dialog fact, which the interpreter turns into a question.
Claude takes keys once bracketed paste is on and it has drawn its first
screen; the agent sends a ready fact once the first text drawn with bracketed
paste on has been followed by 100 ms of quiet, or 500 ms at most. The
`SessionStart` hook is no signal on its own, because some Claude versions run
it only after the first prompt.

Keystrokes are typed by one task, in order, with the pauses the keymap's
programs ask for; what an attached terminal types goes through the same task,
so it never interleaves with a program half-way.

### Headless Claude

The agent runs the spec's command with the spec's arguments plus
`--print --input-format stream-json --output-format stream-json --verbose
--permission-prompt-tool stdio --replay-user-messages`, `--session-id <id>`
or `--resume <id>` (kept in `private/provider-session` as for terminal
Claude), `--settings <json>` and `--mcp-config`. Claude reports nothing until
asked, so the agent writes an `initialize` control request at once, built
from `claude_protocol::stream` types like every line written to Claude; its
answer is what says Claude takes input, and it lists the models and commands
the agent offers.

The interpreter writes everything after that: user messages carry a UUID
derived from the input or envelope id, which Claude echoes on its replay of
the message, and images a person attached go as base64 image blocks beside
the element that names their blob. Closing the child is closing its stdin.

### Codex

The agent runs `codex <args> app-server --listen unix://<private/codex.sock>`
(the socket's address, through a short link when the path is too long), with
amux's tool server added as `--config mcp_servers.amux.command=…` and
`--config mcp_servers.amux.args=…`, and connects to it as one client. Codex's
socket speaks WebSocket: one JSON-RPC message per text frame. The server
outlives any one client, so other clients (Codex's own app) can join the same
thread there. A socket file a killed server left is removed before the next
start. The agent waits up to 30 seconds for the server to listen; a server
that exits first ends the incarnation with its exit code. On Windows the
agent runs `--listen stdio://` and speaks JSON lines on the child's stdin and
stdout instead. It performs the handshake itself, every
message a `codex_protocol` value: `initialize` (client `amux`, experimental
API on), `initialized`, then `thread/start`, or `thread/resume` with the
thread id from `private/provider-session` for a later incarnation, with the
spec's working directory and model. The thread id the server answers with is written to
`private/provider-session`. Straight after, the interpreter names the thread
with the agent's name (`thread/name/set`), and names it again when the agent
is renamed: the daemon tells a running Codex agent with a `rename` frame, and a
resumed agent's spec carries its current name, so Codex's own app shows the
name amux does. A thread that has not run a turn has nothing on disk, and
Codex's own app, which resumes from what is on disk, cannot attach to it;
naming does not change that. When the thread has no turns the interpreter
also reads it with its turns (`thread/read`), which has Codex write it, so
the app can attach before the first turn. Every message the server sends the agent is a fact; the
interpreter writes every request after the handshake with ids of its own
(`amux-<n>`). A turn whose prompt carries attachments has them appended to
its input: an image as a local image at its blob's path, anything else as the
element text the model reads.

Closing the agent's connection ends a stdio server. On a socket, the end of
the agent's connection (closed by the agent, or dropped by the server) is
when the agent sends the server SIGTERM; Codex then finishes any running
turn and exits. Nothing tells a socket server that the agent died, so the
agent also starts a tether beside it: a shell in the server's process group
reading a pipe only the agent holds. When the agent dies, however it dies,
the pipe closes and the shell sends the server's group SIGTERM; after the
server exits on its own, the agent lets the tether go, which ends whatever
the server left in its group.

## Git facts

The snapshot carries the agent folder's branch, base branch and change
totals, which the fleet row shows. The agent process reads them with the
[`git-facts`](../crates/git-facts/src/lib.rs) crate at start and again each
time a turn ends, off its event loop and one read at a time, and feeds each
result to the interpreter as an event; the snapshot changes only when the
facts do. A folder outside a repository, or a machine without git, has none.
The base is the branch the agent's worktree was made from when its host
made one (the spec's `base_branch`), else the repository's default branch:
what `origin/HEAD` names, else the configured default, else `main` or
`master`.

## The facts ring

`private/facts/` records every event the interpreter was fed, in order, so
its state can be rebuilt and a dump can show what the provider actually said.
It is segmented like the journal, and each segment has a checkpoint beside it,
`<offset>.checkpoint`: the interpreter's whole state before the segment's
first entry.

- An entry is one JSON line: a fact with its channel and its payload (as text
  when it is UTF-8, as hex otherwise), a person's input or an agent message
  as its encoded protobuf in hex, a Codex agent's new name, a tick, the
  provider's exit, the daemon being lost, a stop, the
  process exiting, or the folder's git facts as an encoded `Git` in hex
  (`interpret::ring::Entry`).
- The ring's size, from the spec (4 MiB by default), is split across two
  segments; a segment rotates once full, checkpointing the state the next
  event meets, and only the newest two are kept.
- An event is recorded before it is stepped. If its step cannot be journaled
  the entry is taken back out, so the ring never claims more than the journal
  holds.
- Every incarnation opens a segment of its own, whose checkpoint is the state
  it resumed.

A later incarnation starts from the newest checkpoint and feeds the entries
after it through the interpreter again. If those entries do not end with the
provider's exit or the process exiting, the previous incarnation was killed
or crashed, and the agent writes its missing final boundary now ("ended
unexpectedly"). Then the interpreter's `reincarnate` continues the state
under the latest spec: item keys carry on, so none is reused, and the step it
starts with re-emits every open item in full. A ring that cannot be read
starts over from nothing. [Checkpoints](INTERPRETERS.md#checkpoints) covers
what an interpreter must keep.

The agent's part of a dump is these segments, their checkpoints and every
spec file, each redacted by the kind's interpreter before it leaves the
process (`dump.rs`). A file that cannot be read is named in `dump-errors`,
never included unredacted. See [debugging](DEBUGGING.md).

## Hooks and the transcript

Terminal Claude has no protocol a program can read. What the agent knows
about it comes from its hooks, its transcript file and its terminal.

A hook reaches the agent like this:

1. Claude runs the hook command, `<install path> hooks claude`, for each
   registered hook event, with the payload on stdin.
2. The command reads `CLAUDE_HOOK_SOCKET`, connects to `private/hooks.sock`,
   writes the payload and closes the connection. When Claude put its
   messaging socket's path and token in the hook's environment
   (`CLAUDE_CODE_MESSAGING_SOCKET`, `CLAUDE_CODE_MESSAGING_TOKEN`), the
   command wraps the payload in an envelope that carries them
   (`claude::hooks::forward_from_env`).
3. The command always exits 0, even when it could not deliver: Claude reads a
   hook's exit code as a verdict, and 2 would block the action.
4. The agent reads one connection at a time, in the order Claude ran its
   hooks. Messaging credentials go to the provider, which uses them to hand
   agent messages to Claude; the payload Claude wrote becomes a hook fact,
   unchanged, which the interpreter decodes with `claude_protocol::hooks`.

The transcript is found through the hooks. `SessionStart` names the
transcript file; when the path changes (a start, a resume, `/clear`, a
compaction) the interpreter asks the agent to follow it. A follower reads the
file every 25 ms, passes on whole lines only, and records its position in
`private/transcript-cursor` (the offset and the file's path) after every row.
A later incarnation following the same file continues from the saved
position, so a resumed session's rows are not read twice.

An agent message for terminal Claude goes onto Claude's messaging socket,
which queues it the way Claude queues a peer's message. When the socket is
not known yet or refuses the message, the agent pastes it into the terminal
wrapped as Claude wraps what arrives on the socket
(`<cross-session-message from="amux">`), so the interpreter reads its
reflection the same way.

## Raw attach on pty.sock

A terminal client on the agent's own machine can take over a terminal and
talk to the provider's own interface: `amux attach <agent>`, or entering an
agent from the terminal client (see [the terminal client](TERMINAL.md)).
Nothing on the wire carries terminal bytes, so raw attach is local only. The
client finds `pty.sock` by the directory convention and connects directly;
the daemon is not involved. An agent on another host, or headless Claude,
which has no terminal, is only a chat.

![The daemon is not in this picture. A local caller derives the agent directory by convention and connects directly. This is files mode; codex uses stream mode with a codex resume per connection and no files. Late attachers replay the retained tail through their terminal; retention only needs to cover one full-screen redraw.](figures/pty-socket.svg)

Frames on `pty.sock` are `PtyFrame` messages, framed as on `ctl.sock`. The
agent speaks first with a `PtyHello` naming the mode.

**Files mode**, for terminal Claude, whose one terminal's bytes are already
on disk in `pty/`:

- The hello carries `start`, the oldest byte the `pty/` files still hold, and
  `written`, where the log ends now. The client reads the files itself, by
  position, from `start`.
- As the terminal draws, the agent sends `written` frames with where the log
  ends now; the client reads up to it.
- The client sends `keys` and `resize`. Keys go to the one terminal, in order
  with whatever the interpreter types; with two clients, the last resize
  wins.
- The log keeps four segments of 256 KiB, enough to rebuild one full screen.
  When a client replays history from before it attached, it strips the
  terminal queries out of those bytes (`agent::attach::without_queries`),
  since the terminal replaying them would answer, and the answers would reach
  Claude as typed keys.

**Stream mode**, for Codex, whose terminal is a view on the thread rather
than the agent: each connection gets its own Codex app in a terminal of its
own, `codex resume <thread> --remote unix://<private/codex.sock>`, which joins
the agent's app server as one more client on the live thread. It takes no
configuration of its own; the server has the agent's. What a person does in
it (a prompt, a steer, an interrupt, a settings change, an answer to an
approval) reaches amux as what the server reports, and an approval either
side is asked can be answered from either; nothing is locked. The view's
bytes arrive as `output` frames and are never retained; its TUI repaints the
thread each time it starts. The view ends with its connection, and two
connections are two views on one thread. A client that attaches before the
thread exists is told so in a `closed` frame. A thread that has not run a
turn can be attached to because the agent has Codex write it to disk as soon
as it starts (see [Codex](#codex)). On Windows, where the server is on stdio and has room for
the agent alone, `amux attach` on a Codex agent says attach is not available
and starts nothing.

The last frame from the agent is `closed` with the reason, when it has one;
end of stream means the view ended. The client side is
`agent::attach::Attached`.

## The tool server

amux's tools (agents, hosts, send, spawn, stop, status, attach) are served by
`amux mcp <dir>`, a separate process that the provider launches from the
install path, like any MCP server it is configured with; it is not the agent
process. The agent only names it in the provider's launch (`--mcp-config`
for Claude, `--config mcp_servers.amux.…` for Codex), and only when the
spec's configuration has an install path. It speaks newline-delimited
JSON-RPC over stdio.

The fleet tools (agents, hosts, send, spawn, stop) dial the daemon at
`<dir>/tools.sock`, a socket the daemon listens on for this agent alone, so
the daemon knows who is calling from the connection and nothing in a request
claims an id. A call made while the daemon is restarting retries for five
seconds; with no daemon it answers "amux daemon isn't running". The other two
never reach the daemon: status answers ok and the interpreter reads the call
from the provider's own record, and attach writes the file into `blobs/` and
returns the element the model puts in its reply. The tool server never writes
the journal; the agent process is its only writer. [Agent tools](AGENT_TOOLS.md)
describes each tool.

## Exit and self-destruct

The agent never exits because the daemon went away, only because nobody came
back:

1. End of stream on `ctl.sock` arms the grace timer (the `agent.grace_secs`
   setting, five minutes by default). The timer also runs from the moment the
   process starts until a daemon first dials in.
2. A daemon that dials in before it runs out cancels it. Nothing else
   happened.
3. When it runs out, the interpreter writes a daemon-lost boundary and the
   agent drains: the turn in progress finishes, the agent writes its final
   boundary with the cause "daemon lost", and it exits.
4. A pending ask is mid-turn, not an end. An agent draining after the daemon
   was lost with an ask open waits for the drain deadline (the
   `agent.drain_secs` setting, five minutes by default) and exits "orphaned
   while waiting for you". A resume asks again, since providers drop an
   unanswered tool call when they resume.

The finished turn is in the journal. Whichever daemon comes next ingests the
rest of it, shows the agent as exited with its cause, and a resume (from any
client) writes the next `spec.<n>` and starts the directory again with the
same id.

On the way out, the listeners are dropped before the lock is released, which
removes the sockets, so the next incarnation never has its socket removed by
this one. The runtime is shut down without waiting for its blocking threads:
a terminal read can stay blocked for as long as a straggler holds the
terminal. `amux agent` exits 0 whatever the cause; 1 means it could not start
(the lock was held, there was no spec, a socket could not be bound).

## Parameters

| Parameter | Default | Where it is set |
|---|---|---|
| Grace after the daemon leaves | 5 minutes | `agent.grace_secs` in settings, copied into the spec's `grace_ms`; 5 minutes if the spec leaves it unset |
| Drain deadline for an orphaned ask | 5 minutes | `agent.drain_secs`, copied into `drain_ms`; the same 5 minutes if the spec leaves it unset |
| Facts ring size, across its two segments | 4 MiB | `agent.facts_ring_mib`, copied into `facts_ring_bytes` |
| Journal segment size | 1 MiB | the spec's `journal_segment_bytes` |
| Terminal log | 4 segments of 256 KiB | constants in `host.rs` |
| Provider asked to finish, then killed | 10 s | `PROVIDER_STOP_MS` in `host.rs` |
| Output collected after a child exits | 300 ms | `TRAILING_OUTPUT` in `provider.rs` |
| Transcript poll | 25 ms | `TRANSCRIPT_POLL` in `provider.rs` |
| Terminal Claude's first screen: quiet, cap | 100 ms, 500 ms | `FIRST_SCREEN_QUIET`, `FIRST_SCREEN_WAIT` in `provider.rs` |
| Codex view asked to end, then killed | 2 s | `VIEW_STOP` in `attach.rs` |
| Tool server retry window | 5 s | `RETRY_WINDOW` in `tools.rs` |

Every deadline reads the injected `agent_dir::Clock`, so tests drive them by
hand with `ManualClock`. [Parameters](PARAMETERS.md) lists every tunable in
the system.

## Testing

| Suite | Run it with | What it holds |
|---|---|---|
| Provider hosting | `just test-crate agent -- --lib --test providers` | Each kind's child is launched, completes its handshake and a turn, continues its own session in a later incarnation, and takes agent messages through its own channel |
| Lifecycle | `just test-crate agent -- --test lifecycle` | Grace, drain, abort, kill, a failed journal write and a provider exit, with every deadline driven by the test's clock; the snapshot's git facts at start and after a turn |
| Raw attach | `just test-crate agent -- --test attach` | Two clients share terminal Claude's one terminal; each Codex client gets its own view |
| Codex attach | `just test-crate amux -- --test codex_attach` | Codex's own app joins a fresh agent's server; its prompt shows in amux's chat, and an approval is answered from amux, then from the app |
| Codex attach, live | `just live codex attach` | The same against the real Codex, from amux's client and Codex's app in two terminals: a timed capture of each (`app.cast`, `amux.cast`, raw bytes), a text frame of both per step and a verdict, in `target/live/codex-attach/` |
| Codex thread name | `just test-crate agent -- --test codex_thread_name` | The thread is named after the agent at start and on rename |
| Dump | `just test-crate agent -- --test dump` | The dump part carries no planted secret |
| Tool server | `just test-crate agent -- --test tools` | Tool calls reach a scripted daemon on `tools.sock`, retry across an update window, and come back as the items the interpreter draws |

These run the real process code against the scripted fake providers in
[`crates/provider-fakes`](../crates/provider-fakes/src/lib.rs) and recorded
replays in `crates/agent/tests/replay`. [Testing](TESTING.md) explains the
suites, lanes and the live provider lane.
