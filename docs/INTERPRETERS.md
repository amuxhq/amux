# Interpreters

*For developers adding or changing how a provider's output becomes chat items.*

An interpreter turns one provider's output into the items and snapshots a
chat shows. There is one per agent kind, in
[`crates/interpret`](../crates/interpret/src/lib.rs):

| Kind | Provider | Module |
|---|---|---|
| `claude_pty` | Claude Code in a terminal | [`interpret::claude_pty`](../crates/interpret/src/claude_pty/mod.rs) |
| `claude_sdk` | Headless Claude over stream-JSON | [`interpret::claude_sdk`](../crates/interpret/src/claude_sdk/mod.rs) |
| `codex` | The Codex app server over JSON-RPC | [`interpret::codex`](../crates/interpret/src/codex/mod.rs) |

The interpreters are the only place provider facts are read, and, with the
client's thin per-kind layer, the only code that decodes an item or snapshot
body. They run inside [the agent process](AGENT_PROCESS.md), which feeds
them events and carries out what they ask; what they emit is journaled,
committed by the daemon and served to clients unchanged (see
[the journal and the store](JOURNAL_AND_STORE.md)). The items themselves are
described from the client's side in [the chat vocabulary](CHAT_VOCABULARY.md).

Provider messages reach an interpreter only as types. Every fact is decoded
by its provider's protocol crate (`codex_protocol` for Codex,
`claude_protocol::stream` for headless Claude, `claude_protocol::transcript`
and `claude_protocol::hooks` for terminal Claude) and every line an
interpreter writes is built from the same crate's types and encoded by it;
see [the provider crates](PROVIDER_CRATES.md). Decoding is tolerant, so a
provider update reads as `Unknown` where it is new rather than stopping the
agent. What stays JSON is what no protocol fixes: a tool's input and result,
which are whatever the tool wrote, and a tool server's form content; small
`Deserialize` structs read the fields an interpreter needs out of them.
`just typed-provider-check`, part of `just ci`, fails on `json!`, a field
looked up by name or bytes parsed into a `serde_json::Value` anywhere else
in the interpreters, the agent's provider handshake or Claude's messaging
socket.

## The interface

An interpreter is pure. It has no clock (time arrives as a tick), generates
no ids (they arrive on inputs and facts) and does no I/O. It implements
`interpret::Interpreter`:

| Member | Purpose |
|---|---|
| `type State: Checkpoint` | Everything it holds; serialized as the checkpoint |
| `const KIND` | The kind tag its items and snapshots carry |
| `initial(spec, version)` | The starting state and the first journal frame: a snapshot with phase starting and every field at its explicit unknown |
| `step(state, event)` | One event in; a `Stepped` out: the journal `Step` and the `Effect`s to perform |
| `reincarnate(state, spec, version)` | Continues a checkpointed state in the agent's next incarnation |
| `redact(target)` | Removes secrets from a body, a facts-ring entry, a checkpoint, a spec, a step or an inventory row, keeping its structure |
| `unknown_snapshot()` | The snapshot body with every field unknown |
| `pending_messages(state)` | Agent messages the provider accepted and has not consumed yet; a one-shot agent waits for this to be empty |
| `typed_unrecorded(state)` | A prompt typed that the provider has not recorded yet, where the record starts the turn (terminal Claude); a one-shot agent waits for it too |
| `describe_item`, `describe_snapshot`, `fixture_input`, `recording` | What the golden harness needs to render bodies, build authored inputs and read recorded corpora |

An `Event` is a provider `Fact`, a person's `Input`, an agent message with
the envelope its daemon set, the daemon's word that a Codex agent was renamed,
a `Tick`, the provider's exit, the daemon being lost, a stop request, the process
exiting, or the agent folder's git facts (`Git`, none outside a repository),
which the shared core publishes on the snapshot. A fact carries its `Channel`: `Transcript`, `Hook` and `Agent` for
terminal Claude, `Stream` for headless Claude, `Rpc` for Codex.

An `Effect` is something the agent process does on the interpreter's behalf,
in order: write bytes to the provider (`ProviderWrite`), reply to an input
(`Reply`), hand an agent message to the provider's own injection channel
(`Inject`), type a semantic input into terminal Claude (`Terminal`), send a
headless user message (`UserMessage`), follow a transcript file
(`FollowTranscript`), send a Codex turn whose input carries attachments
(`CodexTurnInput`), write a blob an item refers to (`WriteBlob`), or end the
process (`Exit`).

## The step rule

Every event yields exactly one step: zero or more items, the snapshot when it
changed, and a turn end when the step ended a turn. The agent appends the
step to the journal as one frame; a step with nothing in it writes nothing.

- **Items are self-contained.** Each item is emitted at its full current
  state under a stable key. Emitting the same key again is a revision; a key
  never changes its body's arm. A client never needs an earlier revision to
  understand a later one.
- **One appendable field.** An item that is still streaming is emitted open,
  and later steps extend its `text` with appends. Once it is emitted
  complete, no append follows. Any item still open may be appended to, so a
  command running below newer items streams its output as appends of the
  new text alone.
- **Command output is bounded.** A running command keeps only its newest
  output (`Shared::append_output`): past twice `OUTPUT_CAP` (64 KiB) the
  kept text is cut to its last cap's worth at a line boundary, the item is
  sent again whole with the bytes dropped in its body, and appends continue
  from there. A command that prints without end costs bounded state.
- **Order is journal order.** The interpreter numbers nothing. The daemon
  gives a key its place in the chat when the key first commits, in the order
  frames were written and items sit in a step, and a revision number on
  every commit; a later revision keeps the key's place.
- **The snapshot rides along when it changed.** `Shared::finish` builds the
  snapshot from the kind-neutral state and the kind's body and includes it
  only when something other than its timestamp differs from the last one
  emitted.
- **A refusal is a reply and nothing else.** An input the interpreter
  rejects produces a `Reply` effect with the reason and no item and no
  snapshot change.

The kind-neutral part every interpreter composes is `interpret::Shared`
([`shared.rs`](../crates/interpret/src/shared.rs)): the person's queue, the
open asks, the set of accepted but unconsumed agent messages, `working_on`,
the turn counter, the prompts awaiting their reflection, the items still
open, and the last snapshot. A person's prompt goes to the provider at once
when it is ready (started, no turn running, no ask open, no typed prompt
still waiting for the provider to record it) and nothing waits ahead of it, and is answered accepted; otherwise it waits in the queue,
answered accepted and queued, and goes when the provider next becomes ready.
`Shared` also derives the phase: needs you whenever an ask is open; starting
until the provider accepts input; working while a turn runs; idle otherwise.

The first frame is always a snapshot with phase starting and every body field
at its unknown. [`unknown.rs`](../crates/interpret/src/unknown.rs) builds
those unknowns with exhaustive struct literals, so a field added to a
snapshot body does not compile until someone decides what its unknown is.

amux's own tools are recognised in the provider's record, never drawn as tool
calls: a `status` call sets `working_on` in the next snapshot, and a `send`
call becomes the agent message it sent.

## Checkpoints

The state is the checkpoint. Every `State` implements `Checkpoint`: it
serializes as JSON (protobuf values and byte ids inside it as hex, through
`serde_pb`) so a dump reader can see it, and `resume` returns the state with
the step a resume starts with: every open item again in full on its existing
key, and the snapshot.

The agent process writes a checkpoint at the start of each facts-ring segment
and at the start of every incarnation (see
[the facts ring](AGENT_PROCESS.md#the-facts-ring)). Rebuilding the state is
the newest checkpoint plus the ring's entries after it, fed through `step`
again, so everything a kind needs to continue must live in `State`: open
asks, the queue and the prompts awaiting reflection, streaming accumulators,
the provider's session or thread, counters behind item keys.

`reincarnate` continues that state under the next spec: the spec's prompt
joins the queue, what belonged to the previous provider process starts over,
and every counter behind an item key carries on, so a later incarnation never
reuses a key.

The golden harness holds every interpreter to this: for every fixture, it
checkpoints after every prefix of the events, resumes, runs the rest, and
requires the same items and final snapshot as the uninterrupted run, with a
resume that re-emits only keys readers already have, at their current
content.

## Background jobs

Every snapshot body carries `BackgroundJobs`: each command or task the
provider runs past the call that started it, with the key of that call's
item, the command (or the task's description), when it started, and its
kind (shell, subagent, monitor or other, unspecified while the interpreter
does not say), and its key: the provider's own id for it (Claude's task id,
the process id Codex lists), which a stop names. The snapshot envelope carries how many there are, which the daemon
puts on the fleet row. The shared part holds the list and publishes it; each
interpreter sets it from what its provider says:

- **Headless Claude** states its jobs: `background_tasks_changed` replaces
  the list, and a `task_started` or `task_updated` that marks a task
  backgrounded (a task sent to the background after it started says so only
  in `task_updated`'s `is_backgrounded`) adds it with its call's key,
  command and start. A task that finishes leaves the list. Tasks Claude
  marks ambient (its own housekeeping, such as memory consolidation) are
  nothing the person started: they are neither listed nor drawn.
- **Terminal Claude** adds a job when a Bash result's `backgroundTaskId` or
  a background subagent's launch says one started, lists its jobs in the
  `Stop` hook at each turn's end, and drops one when its task notification
  row arrives or, for a subagent, its `SubagentStop` hook.
- **Codex** is asked: at a turn's end with a command still running, the
  interpreter sends `thread/backgroundTerminals/list` (ids
  `amux-jobs-<n>-<page>`, following `nextCursor`) and publishes the commands
  it names. A listed command whose item completes later, outside any turn,
  leaves the list; a turn's end with nothing still running empties it.

A listed job no call is known to have started shows by its description, from
when it was first listed. The list is emptied when the provider exits, and a
new incarnation starts with none. A clean exit of Codex or headless Claude
ends their jobs; a provider killed alone can leave them running, orphaned,
and terminal Claude's `/exit` can move the session and its jobs into
Claude's own background. Once the provider has exited nothing reports on
such jobs, so amux lists none.

**Stopping one job.** `StopJob` names a job by its key and leaves the agent
running. Headless Claude sends `stop_task` with the task id; Codex sends
`thread/backgroundTerminals/terminate` with the thread and process id (ids
`amux-stop-<key>`). The job reads `stopping` until the provider says it
ended (Claude's task notification or a list without it; Codex's item
completing); a refusal (Claude's error, Codex's error or `terminated:
false`) clears `stopping` and sets `stop_error` in the provider's words,
and the job can be stopped again. A key no running job has is refused
unsupported. Terminal Claude has no way to stop one job, and its interpreter
takes no `StopJob`; `ui_view` offers the stop only on the other two kinds.

## The catalogue

What an agent offers to pick from (models with their efforts, commands,
permissions, modes) is its `Catalogue`, kept out of the snapshot because it
is large and rarely changes. An interpreter builds it whenever its provider
states or restates what it offers; the shared part encodes it, hashes the
bytes with SHA-256, and when the hash differs from the last one emits
`WriteBlob` with those exact bytes and publishes the hash as
`Snapshot.catalogue`. The agent process writes the bytes to
`blobs/<hex of hash>` in the agent's directory before it journals the step,
so a snapshot never names a catalogue that is not on disk, and a client
reads it with `GetBlob` like any other blob. An unchanged list writes
nothing.

- **Headless Claude** offers what its `initialize` answer lists; a plugin
  reload's answer and Claude's unasked `commands_changed` event replace the
  commands.
- **Codex** offers what `model/list` and `skills/list` answer. Codex sends
  `skills/changed` (empty params) when a skill appears or goes; the
  interpreter asks `skills/list` again with the same parameters (ids
  `amux-skills-<n>` after the first) and rebuilds from the answer. Notices
  that arrive while an ask is out cost one more ask after its answer. Codex
  says nothing when its models change, so they are asked once per server.
- **Terminal Claude** cannot be asked, so it offers what its host's Claude
  offers in the agent's own folder: the daemon asks the host's Claude from
  inside that folder (so a project's own commands and skills are listed),
  hands it the models and commands in `AgentSpec.offered`, and it publishes
  them, with its own permissions, when it launches. A folder's answer is
  kept in memory as long as the host's copy would be; a failed ask falls
  back to the host's copy.

Each snapshot also states the running model's display name: the catalogue
entry amux chose the model as, while that entry stands for the reported
model; else the entry whose value is the model's id; else the id tidied by
the kind's interpreter ("claude-opus-4-1-20250805" reads "Opus 4.1",
"gpt-5-codex" reads "GPT-5 Codex").

## Plans

A plan the agent puts to the person is an item of its own, kind `plan`: the
item's text is the plan and its body says how it was decided (undecided,
approved, approved switching to the permission it names, sent back with
the person's note, or dismissed). Its ask, `PlanAsk`, lists the choices the
provider takes and, where one switches permission, which permission; the
answer names one, with a note when it keeps the agent planning, and a
switch names the permission it agrees to, so an answer that agreed to
something other than what the provider would do is refused. Clients
draw both from these facts alone.

- **Claude, both kinds.** `ExitPlanMode` is drawn as its plan, keyed by the
  call, once its input carries the plan. The choices are start building,
  start switching permission, and keep planning. The switch is the one
  Claude's own plan menu makes, by one rule for both kinds: never ask where
  Claude was launched allowing it, else auto mode where Claude offers it
  for the running model, else accept edits. Terminal Claude's switch
  follows from its launch arguments and the host's catalogue. The ask names the switch when
  it opens; an answer agreeing to it names no permission, so it can only
  agree to the one the person was shown. While Claude is in plan mode,
  its `Write`, `Edit`, `MultiEdit` and `NotebookEdit` calls draw nothing:
  plan mode lets Claude write only its plan file, and the declaration
  carries the whole plan. The ask closes on the person's answer, when the
  agent leaves plan (an approval, read by the permission it left for), when
  the next turn starts, or when a prompt is typed instead, which dismisses
  the plan: headless Claude has the call refused and its turn ended, and
  terminal Claude has its menu cancelled with the interrupt key, so the
  prompt starts the next turn. Headless Claude leaves plan with the plan
  waiting only through amux's own permission change, which answers the
  call as approved with that permission.
- **Codex** streams its plan item as a plan. Codex asks nothing about a
  plan, so when a turn in plan mode completes with one the interpreter opens
  a plan ask of its own (key `plan:<item id>`) and the agent needs you. The
  choices are implement, which sets the mode to default, leaves the
  permission alone and starts a turn with "Implement the plan." (what
  Codex's own app sends), and stay, which sends the note, if any, as the
  next prompt in plan mode. Whoever decides, the ask closes when the agent
  leaves plan (approved; Codex reports the new mode before the turn it
  starts) or when the next turn starts (sent back if it runs in plan mode,
  approved otherwise). A prompt typed in amux instead dismisses the plan.

## Questions

A question the agent asks is an ask item of its own, keyed by the call or
request that asked it, whose record lists each question's answer. An answer
gives one response per question: the options picked, typed text, and a
note. A response with nothing picked and no text skips its question. The
record reads answered N of M, each skipped question marked, with its notes.

Instead of answering, the person can reply: the answer carries their words
and any answers given so far, and the record reads replied instead with the
words.

- **Headless Claude.** `AskUserQuestion` is drawn as its ask item rather than
  a tool row. A skipped question is left out of the answers handed back to
  the tool, as Claude's own form leaves it, and each note goes in the input's
  `annotations` under its question's text. A reply refuses the call with
  the words as the reason and leaves the turn running, so the model reads
  them as the call's result. A question answered or dismissed elsewhere
  takes its record from the call's result.
- **Terminal Claude.** The same item, answered the same way: the answer goes
  back on the `PermissionRequest` hook Claude waits on, built by the same
  code as headless Claude's control reply, so skips, notes and a reply
  behave as they do there.
- **Codex.** `item/tool/requestUserInput` is the ask. A skipped question goes
  back with no answers, as Codex's own form sends it, and a note is one more
  answer, `user_note: <note>`. A reply interrupts the turn and sends the
  words as the next prompt.

## Redaction

Dumps leave the machine, so every body in them is redacted, and only an
interpreter can decode its bodies. `interpret::redact`
([`redact.rs`](../crates/interpret/src/redact.rs)) dispatches to the kind's
`Interpreter::redact` for each `RedactTarget`:

- Protobuf values are walked by their descriptor: every string field is
  redacted as text, or as JSON when it holds a JSON object or array; every
  `*_json` bytes field as JSON; the per-kind `body` bytes as the kind's body
  message. Ids and hashes are kept, and a field the descriptor does not name
  is dropped.
- Facts are walked as JSON. A checkpoint is decoded as the kind's state and
  written again with every protobuf value inside it walked the same way.
- The values of environment maps are blanked, as are secret-named
  environment assignments in text and tokens with well-known prefixes.
  Provider session and thread ids become, wherever they appear,
  `<SESSION-` followed by eight hex digits of a hash of the id and `>`, so one
  session stays recognisable across a dump.

Underneath is [`crates/redaction`](../crates/redaction/src/lib.rs), the
structural redactor for free text and JSON: secret-named keys, known token
shapes, machine paths, emails, the local user and host. It knows no record's
schema. The specs' capture sanitizer and reports use it directly.

`crates/interpret/tests/redaction.rs` runs a short session per kind with
secrets planted in a tool's input and output, the prompts, an answer, an
environment-bearing payload and the session id, and checks that no planted
secret survives in any item body, snapshot body, facts-ring entry or
checkpoint, that each still decodes as what it was, and that redacting again
changes nothing.

## Terminal Claude

`claude_pty` reads four kinds of fact: rows of Claude's transcript file
(decoded as `claude_protocol::transcript::Row`), hook payloads
(`claude_protocol::hooks::Payload`), the agent process's own facts (the launch
with Claude's version, the folder-trust dialog, and "ready"), and the
terminal's exit. Nothing is read off Claude's screen beyond its first.
Transcript rows arrive whole; a reply streams from the `MessageDisplay` hook
until its row lands, and every other item is emitted complete and revised by
re-emission.

Terminal Claude's asks arrive on hooks that wait for amux's decision, but a
person can also answer one in Claude's own terminal, which reports nothing
until the call's row or result lands, so how an ask ends is inferred. Every
inference rule is written down in the module's own documentation at the top
of
[`claude_pty/mod.rs`](../crates/interpret/src/claude_pty/mod.rs); change the
rules there and here together.

### Rows come from the transcript

Every row of the conversation (prompts, replies, thinking, tool calls,
steers, slash commands, compactions, errors, the turn's end) comes from its
transcript row, in transcript order:

- A user row is the person's prompt (its pasted block unwrapped, since amux
  types a prompt as a paste, and its attachment elements made attachments).
  A prompt sent through amux gets its input id from the queue of prompts
  awaiting reflection, and its turn starts with that row: keys typed into a
  terminal may land in one of Claude's own screens and never reach the
  conversation, so until Claude records the prompt the agent is not
  working. The prompt is still in flight, though: the next queued prompt
  waits until the row lands and its turn ends (typed straight after it,
  Claude would take both as one prompt), and a one-shot agent does not exit
  on it. A prompt lost to one of Claude's own screens is never recorded: it
  stays sent, the agent stays idle, and what is queued waits behind it, with
  no timer to release it. The next prompt row (one typed in Claude's own
  view), a slash command, the provider's exit or a stop clears it; a
  one-shot child in that state stays idle until one of those comes.
- An assistant row's blocks become message, thinking and tool-call items,
  keyed by the row's uuid (`<uuid>#<index>` for the second block on). A
  reply Claude already drew keeps the key it streamed under.
- A `tool_use` block opens a call, running, keyed by its tool-use id. The
  `tool_result` in a later user row lands it: succeeded, failed, denied or
  cancelled, with its output, any images it read (written as blobs), and its
  end time. Claude writes the call's row as it starts and its result as it
  ends, so a running call is live in the chat and the text that introduced
  it sits above it.
- A queued-command attachment row is a prompt Claude took into the running
  turn at a tool boundary: a steer item.
- Slash commands (`<command-name>` rows and their `<local-command-stdout>`
  output, with terminal colour codes stripped), compaction boundaries and summaries, API error rows and the
  interruption row (one beginning `[Request interrupted by user`) each become
  their item.
- A `!command` typed in Claude's own view (`<bash-input>` and its
  `<bash-stdout>`/`<bash-stderr>` rows) is a Bash step with its output.
- A user row of task-notification origin carries a background subagent's
  result onto the Agent call that launched it; that call stays running,
  across the end of the turn that launched it, until the notification
  arrives.
- A prompt's row precedes the rows of the turn it began, so nothing waits for
  it.
- The facts that end the main thread's work without saying how its asks
  went (the `Stop` hook, a turn's duration row, a task's notification, an
  interruption, a prompt typed outside amux) close its asks with their
  outcome unknown, and spare those a subagent still running in the
  background raised: Claude keeps that dialog open, so amux does, until the
  subagent's `SubagentStop`.

The rest of the items a chat shows for terminal Claude do not come from hooks
either: the agent-message item is written when the interpreter accepts the
message (Claude's reflection of it only marks it consumed), the turn item
comes from the transcript's own turn-duration row, the folder-trust question
from the agent's trust-dialog fact, and the exit and daemon-lost boundaries
from the agent process.

### What hooks fill in

Hooks carry only what the transcript never does:

| Hook | What it does |
|---|---|
| `SessionStart` | Names the session and the transcript path, and the interpreter asks the agent to follow that file. It marks the session boundary (started, resumed, cleared or compacted), and it means input is live. A `resume` of a session that is not the agent's means Claude's own view switched to another conversation: the snapshot says so (`switched_away`), prompts are refused, the other session's hooks are ignored, and switching back types `/resume <the agent's session>`. A clear is the agent's own conversation starting afresh. |
| `PermissionRequest` | Opens the permission card: an ask carrying the hook's tool, input and scope suggestions (or a question or plan ask for `AskUserQuestion` and `ExitPlanMode`), answered on the hook itself (see [asks and answers](#asks-and-answers)). The hook names no tool-use id; the call the `PreToolUse` before it announced is the one it gates. Claude writes a gated call's row only once the call is decided, so the card points at no row until that call's row lands. |
| `PermissionDenied` | Auto mode refused a call without asking anyone: the call's row reads denied, with the reason, whichever side of the row the hook lands. |
| `Elicitation` | A tool server asks for a form or a link while one of its calls runs. Claude waits on the hook before drawing its own dialog, so the ask, a form or link with an item of its own, is answered on the hook. Someone at Claude's own terminal must see the dialog: while a terminal client shows Claude's screen, the hook is let go as it arrives, and one that comes to the screen lets go of the hook that waits. Claude then draws its dialog, and the open ask is an unanswerable one saying it is answered there, while its item keeps the form or link. It closes cancelled on an interrupt through amux (which cancels it on the hook too), and dismissed, releasing the hook, on that call's result, a row from a later assistant message, or any fact that closes every ask. |
| `MessageDisplay` | A batch of finished lines of the reply Claude is drawing. The first opens a message item keyed `stream:<message id>`, later ones append to it, and the reply's transcript row takes the item over, whole. The hook's message id is not the row's, but Claude writes rows in the order it draws replies, so the oldest reply not yet written is the next text row's; a row that lands before its reply's first batch keeps its own key, and the late batch draws nothing. Hooks and rows travel apart, so a reply's last batch can come after the row that ended its turn, even after the next prompt began; every hook names the prompt its turn answers, and a batch for a prompt whose turn is over draws nothing. A reply's first batch can also come before the row of the prompt it answers: a turn's batches wait from its prompt's `UserPromptSubmit` until a user row of that prompt lands, so the reply is drawn below the prompt. Likewise a reply can open after a call's `PostToolUse` but before the call's row is read (a gated call's row is written only once it is decided): a new reply's batches wait until the row of every call whose `PostToolUse` came first lands, so the reply is drawn below the call. A reply cut short with no row ends as drawn when the turn ends. |
| `PostModelSwitch` | The model Claude switched to, as when plan mode moves to a bigger model, shown until the next assistant row names the model it ran. |
| `Stop` | Lists the background jobs still running (see [background jobs](#background-jobs)), clears the running marks, and closes every ask still open with its outcome unknown, drawn dismissed until the call's result says how it went, except one raised by a subagent still running in the background, whose dialog Claude keeps open. It does **not** end the turn. |
| `SubagentStop` | A subagent finished: it leaves the background jobs, and any ask it left open closes with its outcome unknown. |
| `PreToolUse`, `PostToolUse`, `PostToolUseFailure` | Mark a call running, and then not running, for the snapshot's running calls (the activity line while the call's row is on the way); `PreToolUse` also names the call an ask that follows is raised for. They never make a row, except `ExitPlanMode`'s, which opens the plan from its `PreToolUse` since Claude writes the plan's row only once it is decided. `PostToolUse`'s duration gives the call the time it began running, after any wait for approval (`started_at_ms`), which the call's result row carries. |
| `UserPromptSubmit` | Input is live. |
| `SessionEnd` | Closes every open ask with its outcome unknown. |

The turn ends on the transcript's `system` row with subtype `turn_duration`,
which closes any ask still open and writes the turn item with the duration
Claude measured and the step's turn end. An interruption row also ends the
turn, as interrupted. A slash command that runs no model turn ends when its
output row lands, without a turn end, since no turn happened. The provider
exiting cuts a turn short with no turn end at all.

Besides the session boundary `SessionStart` marks, a tool server's dialog,
a streamed reply and a plan are the only items hooks make. When the call a
dialog was raised under has only been announced by `PreToolUse` and its row
has not landed, the dialog's item waits for that row, so it sits below the
call. Recordings made before amux registered `PreToolUse` carry none, so an
ask with no announced call is the oldest one for the same tool and input
when that call's row lands.

A subagent's calls reach the hooks socket carrying its agent id and never
reach the followed transcript. They are counted as steps on the Agent row
that started the subagent, never drawn as rows of their own. An ask a
subagent raises points at that Agent row and closes on the subagent's own
`PostToolUse` for the call.

### Asks and answers

An ask closes on an answer sent through amux (outcome known), on the call's
tool result (answered in the terminal: a refusal is a deny, anything else an
allow), or on a later fact that proves the call is over without saying how: a
prompt, an interruption, the turn's end, a session change, the provider
exiting, or a row from a later assistant message. Nothing else closes one; a
tick never does.

Claude runs the hook command for `PermissionRequest` and `Elicitation` and
reads what it prints as the decision; the agent process holds the hook's
connection until the interpreter answers (see
[the agent process](AGENT_PROCESS.md)). An answer through amux is checked
against the open ask's shape and goes back on that hook as the decision
headless Claude's control reply would carry, built by the same code
([`claude_answers.rs`](../crates/interpret/src/claude_answers.rs)): allow,
with the suggestion a scope names applied exactly as offered; deny, with a
note Claude reads as the reason and a choice to stop the turn; a plan
approved with the permission to leave plan for (Claude ignores a bare allow
of a plan) or sent back with a note; the answers to each question, with
skips and notes; a form's action and content. An answer that does not fit
the ask is refused. An ask whose hook amux does not hold, as in a
recording, refuses every answer.

A permission request's dialog stays usable in Claude's own terminal while its
hook waits: a terminal answer wins, the hook's late answer is ignored, and
the ask closes on the call's row or result as above, releasing the hook with
no decision. A tool server's form is drawn only once its hook returns, so
while amux holds it, only amux can answer it. The hook waits up to seven
days. In manual mode, where Claude offers auto mode for the running model,
the card offers one more scope, a switch to auto, which allows the call with
the permission set to auto and records it at once. A prompt sent while a
plan waits refuses the plan on its hook, which ends the turn, and the prompt
follows.

The permission hook carries neither of Claude's flags
(`suppress_always_allow_rule`, `default_to_no`). An answer goes back on
the hook rather than typed into Claude's menu, so how the menu is laid
out, or which row it opens on, changes nothing about where an answer
lands. An ask Claude leaves only to a person comes with no suggestions in
its hook, and its menu reads Yes / No, with no "don't ask again" row and
no switch to auto mode (recorded live on Claude 2.1.312 for a write into
its jobs folder): an ask with no suggestion offers no scope, the switch to
auto included. A few flagged asks keep their suggestions (some tool-server
policy and ask-rule paths, read from Claude's source rather than
recorded), so amux offers a scope there that Claude's own menu leaves out.

Terminal Claude offers no list of models, efforts or commands a program could
read, so its catalogue, written when it launches, holds the models and
commands its host's Claude offers in its folder (from `AgentSpec.offered`,
empty when the host could not be asked) and Claude's permissions, each
marked not settable. It takes no model or effort input: a person types
`/model <name>` or `/effort <level>` as a prompt.
The model is what Claude's newest assistant row ran, or, from a
`PostModelSwitch` hook until such a row, the model it names; the effort
is the one the newest assistant row ran with. The permission changes only
by cycling. amux knows Claude's cycle (default, accept edits, plan, then
never ask where Claude was launched allowing it and auto mode where its
model offers it, and back to default), so the snapshot shows the
permission a cycle moves to at once, and the permission Claude next records
on a row or hook confirms or corrects it. Whether the model offers auto mode
is the catalogue's word: its auto permission names each model Claude lists
by its alias and by the model the alias resolves to, since Claude's records
name the running model by id; until Claude names it, the model it was
launched with runs, else its default. Its inputs are
prompt, withdraw, send now, interrupt, clear, the permission-mode cycle,
switch back, and answer.

### Keys

The interpreter never produces bytes for the terminal. Everything with a
hook is answered on the hook, so what it emits as a semantic `TerminalInput`
is only what has none: a prompt, send now, interrupt, clear, the
permission-mode cycle and the folder-trust dialog Claude shows before any
hook runs. The agent process types fixed keys for each
([`crates/agent/src/terminal.rs`](../crates/agent/src/terminal.rs)): a prompt
as bracketed pastes of at most 700 characters, which Claude keeps as typed
rather than wrapping as pasted content, then Enter; interrupt as Escape; the
cycle as Shift+Tab; trusting the folder as Down then Enter. These keys are
Claude's own and have not changed across the versions amux hosts. Send now
types the prompt's keys while the turn runs, which Claude takes at the
turn's next tool boundary (the `steer_queued` and `steer_send_now`
recordings, from 2.1.283).

The recording harness still answers Claude's menus with keys, as a person at
its terminal does, so recordings keep covering asks answered in Claude's own
view. Its keymap and the programs that interpret it live in
[`crates/claude-specs/src/terminal/keymap.rs`](../crates/claude-specs/src/terminal/keymap.rs).

## Headless Claude

`claude_sdk` reads the stream-JSON lines headless Claude writes on stdout,
events and control requests alike, each decoded as a
`claude_protocol::stream::Output`. Asks arrive as named control requests, so
nothing is inferred.

- **Streaming.** Text and thinking stream: `content_block_start` opens an
  item keyed `<message id>:<block index>`, each delta is an append, and
  `content_block_stop` completes it. Tool calls are keyed by their tool-use
  id and land on their `tool_result`.
- **Correlation by id.** Every user message the interpreter hands Claude
  carries a UUID derived from its input or envelope id (`client_uuid`).
  Claude echoes it on the message's replay and on its `command_lifecycle`
  frames, so a reflection is matched to what was sent by id. Messages
  written while a turn runs join that turn, so arrival order would mismatch
  them. The prompt's item is written when it is submitted.
- **Asks.** A `can_use_tool` control request opens a permission ask (with its
  scope suggestions, reason and description), a question ask for
  `AskUserQuestion`, or a plan ask for `ExitPlanMode`, pointing at the call
  it names. The reason is Claude's own, else "outside the allowed
  directories" when the path it blocked is beyond the agent's folder;
  Claude names the path a command would write inside the folder too, and
  that ask has no reason. A request marked `suppress_always_allow_rule`
  offers no scope, though it carries suggestions: Claude would narrow or
  drop a rule sent back, so amux neither offers nor records one. One marked
  `default_to_no` says so on the ask (`default_to_no`), and keyboard views
  open it on its refusal. An `elicitation` request opens a tool server's form or link, with
  an item of its own. Answers go back as control responses. A control
  request of any other subtype is answered with an error.
- **Offers.** The answer to the agent process's `initialize` request lists
  the models, each with its effort levels, and the slash commands, which
  make the agent's catalogue; `commands_changed` rebuilds it. Its
  permissions are Claude's own: ask (`default`), accept edits, plan, auto
  (naming the models whose `supportsAutoMode` is set, and left out when
  none is), and never ask (`bypassPermissions`) only when Claude was
  launched allowing it (`--dangerously-skip-permissions`,
  `--allow-dangerously-skip-permissions`, or that permission mode).
- **Inputs.** Prompt, withdraw, send now (a message at default priority,
  which joins the running turn at its next tool boundary), interrupt (a
  control request), clear (sent as `/clear`), permission (a value the
  catalogue offers, sent as `set_permission_mode`; any other is refused)
  and model (control requests), effort (`apply_flag_settings` with `effortLevel`), and
  answer. The effort Claude runs at comes from `get_settings`, asked once
  Claude answers `initialize` and again after a model change: its
  `applied.effort` is what Claude applied from every settings source and
  the session's own change.
- **Built-in commands.** A command Claude runs itself (`/context`, `/cost`
  and the like) answers as the `<synthetic>` model with text only. That
  answer is the command's output, drawn as the slash command's item, not
  a reply; the command's replay names it when it arrives later. Output of
  a local command that no command typed through amux asked for (Claude's
  note on a setting amux changed) draws nothing.
- **Turns.** A turn starts with a submission or a `message_start`, and ends on
  the `result` line: completed, interrupted, or failed (with an error item
  for a non-success subtype), with its duration on the turn item.
  While a message amux handed Claude is unanswered, a result naming none
  of amux's messages (`user_message_uuids`, `user_message_uuid`), or naming
  none and not of a person's origin, ends work of Claude's own (a peer's
  message, a task's notification) and changes nothing of the person's turn,
  its asks or its calls; one naming amux's message ends the turn whatever
  began it. The result dismisses the main thread's asks still open. An ask a
  subagent still running in the background raised (one naming its task's
  `agent_id`, or whose call hangs under its call) stays open, as do that
  subagent's calls, which finish after the turn; its asks close when its
  task's notification says it ended.
- **Boundaries.** Claude repeats its `init` at every turn; only a process
  start, a reset or a changed session id is a boundary. A
  `conversation_reset` (a `/clear`, leaving plan mode with its context
  cleared, a fresh session, onboarding) draws a cleared boundary at once,
  but its `new_conversation_id` is an id Claude makes up before it makes the
  session, so nothing takes it: the next `init` names the new session, which
  the boundary then shows, and which the agent saves (`ProviderSession`)
  for the next incarnation to resume. A session id that changes with no
  reset before it is a fork.
- **Tasks.** A subagent or a shell in the background is reported as task
  events naming the call that started it. They are progress on that call:
  a subagent's steps on its Agent row, and the call stays running, across
  the end of the turn that launched it, until the task's notification ends
  it with the task's answer. A task whose call this interpreter never showed
  is an item of its own.
- **Claude's words.** Claude says several of its fields may carry terminal
  escapes. Every field made of its words (the status word, a fallback's
  and a refusal's reason, a command's output, an ask's reason, description
  and title, a denial's note, a tool server's message) goes through one
  cleaner, which drops CSI and OSC sequences in either form, the other
  escapes and every control character but a newline and a tab. Terminal
  Claude's `!command` and command output go through the same one.
- **Subagents' frames.** A frame naming a `parent_tool_use_id` is a
  subagent's, and is checked for that first: it starts no turn, sets no
  model or context, and draws no text, thinking, error or interruption of
  the main thread's; its tool calls are drawn under the call that started
  it, and its stream events are skipped. A permission ask naming an
  `agent_id` hangs under that subagent's call too. So a subagent working
  in the background holds no turn open, and the person's next prompt goes
  in at once.
- **Usage.** A `rate_limit_event` states one window's status (its
  `rateLimitType`) and every window's use (`unifiedWindows`). The named
  window takes the status; every other keeps the last status stated for it,
  so a person can see the weekly limit reached while the five-hour one is
  fine, until the window resets: a report whose reset for a window is later
  than the one held drops the status stated for it, back to unknown. A
  report can name a window without its use or reset (a window outside
  `unifiedWindows` carries both only when Claude has them): a window seen
  before keeps the use and reset it was last given, and a window first named
  that way says its use is unknown (`used_unknown`) rather than empty.
  Windows are five-hour or weekly, and a weekly window that belongs to
  one model carries the model as Claude's own labels name it
  (`seven_day_opus` Opus, `seven_day_sonnet` Sonnet,
  `seven_day_overage_included` Fable); a window amux does not recognise
  carries Claude's name for it. The overall state is the report's own: with
  overage a window can be spent while the agent still works. Terminal Claude
  reports no usage.
- **Agent messages** go as a user message on stdin, an immediate hand-off to
  Claude's own queue. The agent-message item is written at acceptance, and
  the message is consumed when Claude reports taking that UUID.

## Codex

`codex` reads the JSON-RPC messages the app server writes, each decoded as a
`codex_protocol::ServerMessage`: notifications about the thread, its turns and
their items, requests that ask the person something, and responses to the
requests the interpreter sent, read as the response type of the request they
answer.

- **Requests.** The agent process performs the handshake; the interpreter
  reads the thread from its answer or from `thread/started`, and from then on
  writes every request with ids of its own (`amux-<n>`), so each
  acknowledgement is matched to what it acknowledges.
- **Permissions and modes.** Codex has two settings of its own, when to
  ask and how tightly commands are sandboxed, plus who answers approvals
  (the person, or its `auto_review` model). The catalogue names four
  combinations as permissions: read only (on-request, read-only), default
  (on-request, workspace-write), auto (default with the reviewer answering)
  and full access (never, danger-full-access). Its modes are Codex's
  collaboration modes, default and plan. A permission or mode input names a
  catalogue value, and the next `turn/start` carries the settings it names
  (a mode with the model and effort in force); any other value is refused.
  The snapshot reports the named permission, absent when the settings match
  none (custom), the mode, and the raw settings, read from the handshake's
  thread answer and from every `thread/settings/updated`. A mode chosen when
  the agent was created is set by its first turn.
- **The thread's name.** When the handshake's thread answer arrives, in
  every incarnation, the interpreter names the thread with the agent's name
  from the spec (`thread/name/set`, ids `amux-name-<n>`), and again on the
  `rename` frame the daemon sends when the agent is renamed, so Codex's own
  app shows the name amux does.
- **A fresh thread on disk.** Codex writes a thread to disk at its first
  turn, and its own app resumes from what is on disk (it asks for the
  thread without its turns and pages them), so until then the app cannot
  attach; naming does not write it. When the handshake answers with a
  thread that has no turns, the interpreter reads it with its turns
  (`thread/read` with `includeTurns`, id `amux-persist`), which has Codex
  write it, and ignores the answer and the `deprecationNotice` beside it.
- **Usage.** `account/rateLimits/updated` gives up to two windows, each
  described only by its length: 300 minutes is the five-hour limit, 10080
  the weekly one, and any other length is carried as it is. A window is
  blocked when fully used and near its limit from 80%; Codex saying a limit
  was reached, without saying which, blocks the whole. Codex's credits text
  is kept beside the windows.
- **Offers.** Once the thread is known, the interpreter asks `model/list`
  (following `nextCursor` to the last page, leaving hidden models out) and
  `skills/list`, once per server, and asks `skills/list` again on
  `skills/changed`; the catalogue carries the models with their reasoning
  efforts, the slash commands amux runs as its commands, and the skills
  apart from them. A refused or missing answer leaves its list empty.
- **Slash commands.** Codex's server has no commands: each command Codex's
  own view offers is client code calling a server method, so amux runs the
  ones it offers itself, from a table in the interpreter (`COMMANDS`):

  | Command | What amux sends |
  |---|---|
  | `/compact` | `thread/compact/start`, which runs as a turn of its own |
  | `/clear` | `thread/start` (ids `amux-clear-<n>`) in the agent's folder with the model and permission in force; the thread it answers becomes the agent's conversation, marked by a cleared boundary, named, and recorded for the next incarnation to resume. The agent then unsubscribes from the old thread and the child threads it spawned (`thread/unsubscribe`), which Codex otherwise keeps loaded with their tool servers. The new thread's first turn carries the mode and effort the agent ran with |
  | `/review [instructions]` | `review/start` on the thread: the uncommitted changes, or the instructions given. The review runs as a turn |
  | `/init` | A turn with the prompt Codex's own `/init` sends (as Codex 0.161 words it), which writes an `AGENTS.md` unless one exists |
  | `/plan [prompt]` | Plan mode from the next turn, as choosing the mode does; the words after it, if any, are that turn's prompt |

  The prompt's item shows what was typed. Any other `/word` (Codex's `/new`
  among them, which means another conversation, so another agent) is refused
  as unsupported rather than sent to the model as words; a skill's name after
  a slash is refused with the mention to type instead. A command's name is a
  word with no further slash, so a prompt that starts with a path
  (`/Users/me/src/lib.rs has a bug`) is words and goes to the model.
- **Skills** are mentioned, as in Codex's own app: `$name` anywhere in a
  message or steer, the name a run of letters, digits, `_`, `-` and `:`. The
  text goes as typed, followed by a `skill` input item (`name` and the
  `path` of its `SKILL.md` from `skills/list`) for each skill it mentions,
  once per name. Codex resolves a skill item by its path, so of two skills
  that share a name (a user's and a repository's), the first `skills/list`
  answers is the one selected; text alone would select neither.
- **Asks** are the server's requests:

  | Request | Ask |
  |---|---|
  | `item/commandExecution/requestApproval` | Command approval, with the decisions Codex offers. Codex runs a model's command through the user's shell and asks about the wrapped form (`/bin/zsh -lc "…"`); the ask, like the command's step, shows the script inside, as Codex's own view does |
  | `item/fileChange/requestApproval` | File-change approval |
  | `item/permissions/requestApproval` | Access grant |
  | `item/tool/requestUserInput` | Question |
  | `mcpServer/elicitation/request` | A tool server's form or link, or, when Codex marks it as a tool-call approval, approval of the running tool server call. An approval for amux's own tool server is answered approved at once (for the session when Codex offers it) and opens no ask: amux's own tools never ask, on any kind |

  `item/tool/call` is answered at once that this client hosts no dynamic
  tools. Any other request is answered with a method-not-found error and
  drawn as an unrecognized item.
- **Turns.** `turn/started` begins one and records its id; `turn/completed`
  ends it with its status (completed, interrupted or failed, with an error
  item when the turn carries one) and dismisses every ask still open. A
  command still running when a turn completes or is interrupted carries on
  as a background terminal (Codex's interrupt ends the turn, not its unified
  exec commands; one Codex did end reports its own end), so it stays running
  and the interpreter asks Codex for its background terminals. A
  prompt starts a turn with `turn/start`, and its item is written then, keyed
  by its input id; Codex's own reflection of it is left out. Send now steers
  the running turn with `turn/steer`. Both carry the input id (in hex) as
  `clientUserMessageId`, which Codex echoes as the user message's
  `clientId`, so the echo is matched to what amux sent by id; a user message
  without an id is matched by order (the oldest prompt awaiting its
  reflection, then the oldest steer). A steer Codex refuses, or one still
  unanswered when the turn ends, goes back to waiting in the queue. An
  interrupt is `turn/interrupt`, held until `turn/started` names the turn
  when it has not yet; a held interrupt is dropped when that `turn/start`
  fails or Codex exits, so it never lands on a later turn.
- **Model, effort, approval.** These inputs are accepted at once and ride on
  the next `turn/start` as overrides.
- **Other clients.** The agent is one client of its Codex server, and
  Codex's own app can be another; nothing is locked, and the interpreter
  believes what Codex reports whoever caused it. A user message whose id is
  not amux's is another client's: the first in a turn is drawn as a prompt,
  later ones as steers. Their turns, interrupts and settings changes arrive
  as the same notifications amux's own do and change the snapshot the same
  way. An approval Codex resolves (`serverRequest/resolved`) without amux's
  answer was answered elsewhere: its decision is marked so and reads
  allowed or denied by how the command then ends; one resolved because amux
  interrupted the turn is dismissed. The recordings `two_clients_prompt`,
  `two_clients_approval` and `two_clients_steer` hold real sessions of two
  clients on one server; each line carries the client it belongs to, and
  the interpreter reads only amux's.
- **Child threads' asks.** Codex sends a child thread's approval requests to
  every client of the server. Each ask keeps the thread that asked: a
  child's points at no row of the agent's turn (its work is drawn by the
  child's own subagent row), a child's file-change ask shows the changes the
  child's own items carried, and the agent's turn end leaves a child's asks
  open. `serverRequest/resolved` closes an ask whichever thread it names. A
  tool server's approval belongs to that server's running call (of the tool
  Codex names, from codex-cli 0.163), else to the newest running call. A
  child's request amux cannot answer is refused without a row.
- **Streaming.** Agent-message, plan and reasoning deltas and command output
  are appends to their open items.
- **Other notices.** A web search that opens a page or looks within one
  carries the page's address, as the step's text too, since Codex says
  nothing else about it. Codex's warnings are error items of the kind Codex
  sent them as, in Codex's words, as its own app shows them: `configWarning`
  (configuration Codex found and did not apply), `deprecationNotice`,
  `warning` (among them the one that says a request was routed to another
  model; `model/rerouted` itself changes nothing, since a reroute covers one
  request), `guardianWarning` (except an approved review's, which repeats
  its verdict row), `model/verification`'s cybersecurity check (in Codex's
  fixed words) and `autoApprovalReview/strictReviewRequired`.
  `model/safetyBuffering/updated` is a safety-check item, hidden, which makes
  the turn's activity "Running safety checks" while the check runs. A
  review's `enteredReviewMode` and `exitedReviewMode` items are command-output
  lines ("Code review started: …", "Code review finished"). A
  goal's progress (`thread/goal/updated`) draws nothing, like its clearing,
  and neither does a turn's moderation verdict (`turn/moderationMetadata`)
  or which turn Codex's own app last showed (`thread/readState/changed`).
  Hook runs (`hook/started`, `hook/completed`) and hook-prompt items draw
  nothing: a hook's effect reaches the conversation itself (a blocked call's
  output names the hook), as for Claude. Nor do sub-agent activity, sleep
  and function-call-output items. The notifications amux never draws
  (`DROPPED_NOTIFICATIONS`: hooks, tool-call progress, patch updates, app
  and environment changes, and the rest) are opted out of at `initialize`
  (`optOutNotificationMethods`) and dropped should Codex send them anyway.
- **Agent messages** go to Codex with `thread/inject_items`. Codex reports
  nothing about an injected item, so the interpreter writes the agent-message
  item itself, at acceptance. A probe of codex-cli 0.157.0 settled when the
  message counts as consumed (`CODEX_INJECT_CONSUMPTION`): a running turn
  answers items injected into it before it ends, so a message injected
  mid-turn is consumed at the inject's acknowledgement; an inject into an idle
  thread is answered by an empty turn the interpreter starts for it, and is
  consumed at that turn's acknowledgement. A message that arrives before the
  thread is running is held until it is; if a prompt is waiting for the
  thread as well (a spawn's task), the held messages are injected and ride
  that prompt's turn instead of an empty one, so the model reads its task
  with them. Both arms have goldens of their own.

## Provider transport crates

The messages themselves are in the protocol crates, described with these in
[the provider crates](PROVIDER_CRATES.md).

| Crate | What it holds | Who uses it |
|---|---|---|
| [`claude`](../crates/claude/src/lib.rs) | Hosting Claude Code: hook forwarding over the hook socket (`hooks`), launch settings and the environment scrub (`launch`), the messaging socket client (`messaging`), transcript tailing (`transcript`), session files (`history`) and version probing (`version`) | The agent process (hooks, launch, messaging, version, session files), `amux internal hook claude`, and `claude-specs` |
| [`codex`](../crates/codex/src/lib.rs) | Hosting the Codex app server: the listening socket (stdio on Windows), the process group, and whole messages in and out as bytes | The agent process, for its Codex server and the catalogue probe, and `codex-specs`, whose own client drives Codex to record scenarios |
| [`pty-host`](../crates/pty-host/src/lib.rs) | Provider-neutral pseudo-terminal hosting: spawn in a process group, one output stream, input, resize, process-group signals, graceful termination | The agent process (terminal Claude and Codex views) and `claude-specs` |

## Recordings and re-recording

The interpreters are tested against real provider traffic, recorded by the
spec crates and sanitized so it replays offline:

| Crate | Recordings |
|---|---|
| [`claude-specs`](../crates/claude-specs/src/lib.rs) | `fixtures/sdk/<spec>/` for headless Claude, `fixtures/pty/<spec>/` for terminal Claude, two transcript captures in `fixtures/claude-pty/` that cannot be regenerated in the tree ([their README](../crates/claude-specs/fixtures/claude-pty/README.md)), and the live run's captures in `fixtures/live/{sdk,pty}/<scenario>/`, driven by amux's own interpreter ([their README](../crates/claude-specs/fixtures/live/README.md)) |
| [`codex-specs`](../crates/codex-specs/src/lib.rs) | `fixtures/runtime/<spec>/`, with the capture conditions in [`PROVENANCE.md`](../crates/codex-specs/fixtures/runtime/PROVENANCE.md), and the live run's captures in `fixtures/live/<scenario>/` ([their README](../crates/codex-specs/fixtures/live/README.md)) |

A recording is a directory holding `manifest.json` (the provider, version and
model it was recorded with, its content hashes, the fields it observed, and
its verifications), `io.jsonl` (every line in both directions, with its time)
and `spawn.jsonl`. Captures are sanitized with the shared redaction rules:
machine paths, credentials and personal identifiers become placeholders, while
provider ids, timestamps, payloads and ordering are kept.

An interpreter fixture names its recording by a relative path and a format
(`claude_pty_io`, `transcript_rows`, `claude_sdk_io` or `codex_io`), so the
same bytes feed the recorded replays in the spec crates, the provider fakes'
conformance tests and the interpreter goldens.

### Re-recording

The probes run the real provider under your own login, so they are never
part of an ordinary test run.

```sh
# Claude: list the specs and what each was recorded with.
cargo run -p claude-specs --bin claude-probe -- list [--sdk|--pty]
# Record one or more specs again, terminal or headless.
cargo run -p claude-specs --bin claude-probe -- record --pty <spec>...
cargo run -p claude-specs --bin claude-probe -- record --sdk <spec>...

# Codex, the same two commands.
cargo run -p codex-specs --bin codex-probe -- list
cargo run -p codex-specs --bin codex-probe -- record <spec>...
```

`claude-probe` runs the `claude` on your `PATH`, or the one named by
`CLAUDE_REAL_PATH`; `codex-probe` runs `codex` and records with the model its
specs pin (`CAPTURE_MODEL` in `crates/codex-specs/src/specs.rs`). A Claude
capture, terminal or headless, reads only project and local settings and only
the tool servers its spec names (`--setting-sources project,local
--strict-mcp-config`, unless the spec names its own sources), so your own
plugins, skills, agents, permission rules, default mode and connected accounts
stay out of the recording. A terminal capture's transcript drops the rows that copy each API request and scrubs the
attachments that carry your instructions, environment, attribution and what
you have installed.

After re-recording, run the replays and review the diffs:

```sh
just test-crate claude-specs
just suite codex-recorded-adapter spec_replay::
INTERPRET_UPDATE_GOLDENS=1 just test-crate interpret   # then review the golden diff
```

## Goldens

A fixture under `crates/interpret/fixtures/<kind>/` is a JSON file: what it
shows (`about`), a spec, an optional recording, authored events (facts,
inputs, ticks, checkpoints, with expected replies, phases, queues and asks),
and what may be left over at the end. Recorded events run first; authored
events that must come before them go in a `prelude`. The rendered emission is
the golden beside it, `<name>.golden`, written only when
`INTERPRET_UPDATE_GOLDENS=1` is set. The format is documented at the top of
[`golden.rs`](../crates/interpret/src/golden.rs).

Every run also checks the invariants every journal obeys
(`interpret::check_invariants`): the first frame is a starting snapshot with
the unknown body and no items; a key appears once per step and never changes
arm; no item reopens after its final revision; appends go only to the newest
open item; every open ask points at an item that exists; the queue drains
and every stream ends complete, unless the fixture says otherwise. And it
checks the checkpoint property described [above](#checkpoints).

```sh
just suite interpreter-emissions
```

## The coverage test

[`crates/interpret/tests/coverage.rs`](../crates/interpret/tests/coverage.rs)
ties the interpreters to the chat vocabulary's catalogue of what a chat can
show. The catalogue is encoded in
[`vocabulary.toml`](../crates/interpret/tests/vocabulary.toml) beside it: one
entry per catalogue row, with each kind's availability mark (`full`,
`partial`, `none` or `na`), the carriers that hold the row, and the pattern a
golden line must match. The test enforces that:

- the file holds all 31 catalogue rows, in order;
- a row a kind can show (`full` or `partial`) names at least one carrier, as
  `item:<arm>[.<field>…]` in the kind's item body, `snapshot:<field>` in its
  snapshot body or `ask:<arm>` in its ask, and each carrier is a real field,
  checked against the wire schema's descriptors;
- such a row has a golden pattern, and some line of some golden under
  `fixtures/<kind>/` contains every string in it and none of the strings
  prefixed with `!`;
- a row a kind cannot show (`none` or `na`) names no carriers and no goldens.

A change to what a kind can show is therefore a change to `vocabulary.toml`,
to the kind's bodies and to a golden, together. Run it on its own with
`just suite interpreter-emissions coverage::`.

## Adding or changing an interpreter

- Keep every rule about a provider's facts in its module, and state the
  inference rules in the module's documentation.
- Read and write provider messages through the protocol crate. When the
  interpreter needs something the crate does not type yet, add it there
  first, with the recording corpus still decoding strictly.
- Put everything a restart must continue from in `State`; the checkpoint
  property will catch what is missing.
- Give every item a key derived from the provider's own ids, stable across
  re-emission and across incarnations.
- When a snapshot body gains a field, give it an unknown in `unknown.rs`.
- Record the behaviour you depend on with the spec crate's probe, point a
  fixture at the recording, and author fixtures only for what no recording
  can show.
- Update `vocabulary.toml` and a golden when the change affects what a chat
  can show, and the client's per-kind layer when a body changes (see
  [the client](CLIENT.md)).
