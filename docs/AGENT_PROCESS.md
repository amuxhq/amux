# The agent process

*For developers changing the agent process or adding a provider.*

Every agent runs in a process of its own: `amux internal agent <dir>`, a hidden
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
| [`crates/agent`](../crates/agent/src/lib.rs) | The process: `run` and `main` in `lib.rs`; the driver that runs the agent core over the provider child, the sockets and the journal in `host.rs`; provider children in `provider.rs`; typing into terminal Claude in `terminal.rs`; reading terminal Claude's first screen in `ready.rs`; the facts ring in `crates/core-ring`; raw attach on `pty.sock` in `attach.rs`; the tool server in `tools.rs`; the agent's end of the socket its tool server asks on in `spawns.rs`; the agent's part of a dump in `dump.rs`; directory helpers in `dir.rs` |
| [`crates/agent-core`](../crates/agent-core/src/lib.rs) | The lifecycle as a state machine around the interpreter: one step per input (a provider fact, a daemon dialling in or leaving, a control frame, a timer, a failed write, a job's result), asking for journal steps, provider writes, frames, timers, jobs and the exit; how the next incarnation starts from the facts ring |
| [`crates/agent-dir`](../crates/agent-dir/src/lib.rs) | What the agent and the daemon must agree on: the file and socket names, the directory lock, the frame format on `ctl.sock` and `pty.sock` (`ctl.rs`), the socket its tool server asks the agent on, `private/agent.sock` (`agent_sock.rs`), local sockets (`local_socket.rs`) and the injectable clock (`clock.rs`) |
| [`crates/journal`](../crates/journal/src/lib.rs) | The segmented, append-only journal writer and reader |
| [`crates/pty-host`](../crates/pty-host/src/lib.rs) | Provider-neutral pseudo-terminal hosting: spawn in a process group, output stream, input, resize, signals |
| [`crates/interpret`](../crates/interpret/src/lib.rs) | The per-kind interpreters the process runs |
| [`crates/amux/src/main.rs`](../crates/amux/src/main.rs) | The hidden subcommands a harness runs from the install path: `amux internal agent <dir>`, `amux internal mcp <dir>` (the tool server) and `amux internal hook claude` (terminal Claude's hook command) |

The daemon's side of the relationship is the agent registry: what to
start, stop, kill, delete, resume or rename, and when, is decided in the
daemon core's
[`crates/daemon-core/src/families.rs`](../crates/daemon-core/src/families.rs),
and the processes themselves (spawning, dialling, the control connection,
the startup sweep) are kept by
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
  tools.sock             the daemon listens; its clients are the agent's tool server
                         and, for a spawn, the agent process itself
  journal/0000000000 …   steps, in segments named by the offset of their first byte
  pty/0000000000 …       terminal Claude's raw terminal bytes, named the same way
  blobs/<sha256>         bytes items refer to, and what the agent offers as an
                         encoded Catalogue its snapshot names, each named by its hash
  agent.log              the agent process's stderr
  private/               the agent's own state; the daemon never reads it
    agent.sock           the agent listens; its tool server asks it here, for a child
    hooks.sock           terminal Claude's hook command connects here
    messaging.sock       terminal Claude's messaging socket (Claude binds it)
    codex.sock           Codex's app server listens here; the agent is one client
    facts/               the facts ring and its checkpoints
    transcript-cursor    how far Claude's transcript has been read, and which file
    subagent-cursors/    the same for each of terminal Claude's subagents' files, by its id
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

The daemon writes `spec.<n>` and starts `amux internal agent <dir>` detached from
itself: in its own process group, with stdin and stdout closed and stderr
appended to `agent.log`, so neither the daemon's terminal nor its death
reaches the agent. The agent holds `agent.log` and `private/provider.log`
to 1 MiB each, checking every 30 seconds and moving a log past its cap to
`<name>.1`; a report carries the minute of `agent.log` before it, at INFO and above, and never
`provider.log`, which is the provider's own output.

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
input id, or, when a message resumes an exited agent, that message.
Only the daemon writes it; what a client sends on create or resume can only be
a prompt.

## Lifecycle

The lifecycle is decided by the agent core,
[`agent_core::Agent`](../crates/agent-core/src/lib.rs): one step per input,
with no I/O of its own. The driver in [`host.rs`](../crates/agent/src/host.rs)
is a single loop over six sources (provider events, daemon connections,
control frames, keys and resizes from attached terminals, finished jobs, and
the next timer the core armed); it steps the core with each, at the clock's
time, and carries out what the step asks in order. Keys and sizes from an
attached terminal and raw terminal output never meet the core: they are not
the agent's state. The process is in one of three modes:

| Mode | Meaning |
|---|---|
| `Running` | Ordinary life. Inputs go to the interpreter. |
| `Draining` | On its way out once the turn in progress ends; carries why (the daemon was lost, or a stop, which has already cancelled the turn). Input is answered `REFUSAL_DRAINING`. |
| `Exiting` | The final boundary is written and the provider has been asked to finish. Input is answered `REFUSAL_EXITING`. |

These modes are the process's lifecycle. They are not the phase a client
shows (starting, working, needs you, idle), which the interpreter derives
and carries in its snapshots.

![Self-destruct is the Draining path: EOF on ctl.sock starts a grace timer (a parameter, in minutes); if no daemon reconnects, the agent writes a "daemon lost" item, finishes its turn, and exits. The finished turn is in the journal, so a later resume loses nothing. An orphaned agent mid-turn lives as long as the turn, never longer.](figures/agent-states.svg)

### One decision, one way out

After every input the core's step calls `settle`, the only place that decides
whether this incarnation ends, and every decision to end goes through one
action, `exit(cause)`:

1. feed the interpreter `Exiting` with an `IncarnationEnd { cause, detail }`,
   which closes every open ask and writes the final `Exited` boundary with
   the cause (an `ExitCause`) and its own words, in a step whose
   `incarnation_end` carries the same;
2. take a census of every process below the provider, then ask it to
   finish: end of input and SIGTERM for headless Claude, the end of its
   connection (on which the agent sends SIGTERM) for Codex, SIGTERM to the
   process group for terminal Claude. Every provider cleans up the commands
   it runs on SIGTERM, and headless Claude does not exit on the end of its
   input alone while a background shell runs;
3. enter `Exiting` with a ten-second deadline, after which everything below
   the provider and its process group are killed.

The provider's own exit then ends the loop, after the agent kills whatever
of the census still runs. Claude's and Codex's tool shells each lead a
session of their own, so a kill of the provider's process group misses
them, and by the time the provider has exited its orphans belong to init:
the census, taken while the provider runs, is what finds them. Each process
is remembered with its start time and killed only if that same process is
still running ([`tree.rs`](../crates/agent/src/tree.rs)). Two paths skip `exit`: the
provider exiting by itself (the interpreter records it as the final boundary,
with the exit code or "killed by a signal"), and a kill, which kills the
provider's process group at once and writes nothing.

| While | What happens | Result |
|---|---|---|
| `Running`, no daemon connected | The grace timer runs out | The interpreter writes a daemon-lost boundary; `Draining` (daemon lost) |
| `Running` | `Stop` arrives | The interpreter cancels the turn the provider's own way, so an open ask closes as dismissed and its call as cancelled; `Draining` (stop) |
| `Draining` | The turn ends, or no turn is running | A turn end is marked `last`; `exit` with `DAEMON_LOST` or `STOPPED`; a one-shot agent whose turn ends quiescent while it drains for a lost daemon exits `FINISHED`, as its parent hears it |
| `Draining` after the daemon was lost | An ask is open (phase needs you) | A drain deadline is armed; when it passes, `exit` with `ORPHANED` |
| `Running`, the agent has a parent | A turn ends with nothing running, nothing queued, no accepted agent message still unconsumed, no typed prompt the provider has yet to record, no child it asked for still unsettled, the last report of every child incarnation it made or was told is live already taken, and no provider subagent of its own running | The turn end is marked `last`; `exit` with `FINISHED` (the one-shot rule) |
| `Running`, the agent has no parent and its spec carries the suspension delay | It has been idle for the delay without a break (see below) | `exit` with `SUSPENDED` |
| `Running`, the agent has a parent and sits idle | A child it asked for is settled as never made, and nothing else is waited for: a later daemon's `children` frame leaves out a spawn its predecessor never answered, or a refusal arrives after a turn cut short has ended | `exit` with `FINISHED`; no turn end was marked `last`, so the daemon reports the exit to the parent |
| Any | The interpreter asks to exit (`Effect::Exit`) | `exit` with `ENDED_UNEXPECTEDLY` and the interpreter's words |
| Any | A write to the directory fails (in practice, a full disk): a journal frame, a blob, or the provider's own state in `private/` that the next incarnation resumes from (Codex's thread id, the transcript position) | `exit` with `DIRECTORY_FAILED` and the failed write as the detail; if even that cannot be written, the provider is killed. The cause is then not in the journal either, so the row says `ENDED_UNEXPECTEDLY` (or `ENDED_WHILE_AWAY`) |
| Any | The provider exits | The loop ends; `PROVIDER_EXITED` with the exit code or signal |
| Any | `Stop { kill }` arrives | Everything below the provider, and its process group, is killed; `KILLED` |
| Start | The provider cannot be started | `COULD_NOT_START`, with why |
| `Exiting` | Ten seconds pass | Everything below the provider, and its process group, is killed |

Why the incarnation ended travels in the journal, not on `ctl.sock`: the
step that writes the final boundary also carries `incarnation_end`, and the
agent nudges its daemon after it as after any step. The daemon copies the
cause onto the agent's row in the commit that ingests that step, and reads
the journal to its end before it marks the agent exited, whether it watched
the process end or finds it gone when it starts, so the row gets the agent's
own cause even when no daemon was connected at the time, or when a daemon
ingested the step and stopped before marking. A journal that stops
without one (the process was killed, or crashed, or could not write its
directory) gets the daemon's own cause. An incarnation that ended with no
final boundary at all gets one with `ENDED_UNEXPECTEDLY` when the next
incarnation starts; that step carries no `incarnation_end`, since the row
already says how the earlier incarnation ended.

A request for its record is answered in every mode, since a draining agent is often the
one someone wants to look at.

Nothing a person does in a chat stops the process. A chat's interrupt is the
interrupt input, which cancels the turn and leaves the agent live and idle.
Process stops come from the daemon: a close (`SetFiling`, `amux settle`, an
agent's stop tool), the family rule, and the first step of a delete each send
`Stop` on the control socket, and the agent cancels its turn the provider's own way and exits. There is no "stop when
done": the agent's own drain, finishing its turn, happens only when its daemon
is lost, and no message asks for it. If a stopped agent overruns the daemon's
stop deadline (30 seconds), the stop becomes a kill: the daemon sends
`Stop { kill: true }` on the control socket, and if the agent has not exited
two seconds later, kills the agent process's own process group from outside.
The kill is the daemon's own escalation and no client can ask for it.
The process it kills is the one on the other end of the control socket, as
the operating system names it (`SO_PEERCRED` on Linux, `LOCAL_PEEREPID` on
macOS, the pipe's server process on Windows), so an agent adopted from an
earlier daemon is killed as surely as one this daemon started.

A turn end after which the incarnation exits is marked `last` in the
journal (the interpreter cannot know it; the process marks it): a one-shot
turn end that leaves it quiescent, one the interpreter asks to exit after,
and any while it drains or exits. Its parent's report of that turn is the
incarnation's last. An incarnation that exits without one, its turn open or
ended idle (a subagent stopped while it waits for its own child), gets a
report from its daemon that it exited instead, so every child incarnation ends with
exactly one last report, which names the child incarnation it is from. A
waiting subagent matches reports to incarnations, so a report that comes
before the daemon names its incarnation (a resumed child's, say) is kept for
it. It counts on that, not on how many
agents are live below it: one child's report can start a turn that spawns
the next before the daemon says the first has gone, and a child with its own
child reports mid-way and again at its end.

Which children there are to wait for, the agent learns from two sides. The
children it asked for it knows itself: its tool server's `start_subagent` goes
through the process ([the tool server](#the-tool-server)), which tells the
core of the child before the request reaches the daemon and of the daemon's
answer before the tool server has it. A turn's end and a daemon's frame
arrive on different sockets, in either order, so a subagent that learned of
its own spawn only from the daemon could end its turn first and exit under a
child it had just started. The children that exist the daemon knows, and
says in its answer to the create and in the `children` frame. A child the
daemon made owes its last report; a refused one is dropped; one whose
daemon went away before answering stays asked for until a `children` frame
names it, or one from a daemon that connected since leaves it out. A frame
that leaves a child out never drops it otherwise, since the daemon may have
written the frame before it made the child.
[Agent tools](AGENT_TOOLS.md#families) has the rule in full, and why a
child created for the agent from outside it is not covered.

A provider subagent (Claude's Agent tool, a Codex child thread) runs inside
the agent's own provider and hands its answer back through it: Claude starts
a turn of the parent's on the subagent's task notification, and Codex returns
it within the parent's running turn. A one-shot agent whose turn ends while
one runs would kill it and report the launch as its answer, so it stays. The
process knows which run from the steps it journals itself, each subagent's
part of a step carrying its start or its end, so the rule is the same for
every provider. When the provider answers with a turn of its own, that turn
has started by the step that carries the subagent's end, and the turn's end
decides, as after a child's report. When the last subagent's end leaves the
agent idle with nothing queued or awaited and no turn of the provider's to
follow, the agent finishes there, as when a spawn it waited on made no child.
A turn after one subagent ended while another still runs reports up and is not
the last.

The one-shot rule is what having a parent means: there is no flag. An input
that arrives while such an agent is exiting is answered `REFUSAL_EXITING`,
and the daemon then treats it as exited and resumes it if the sender is its
parent. See [agent tools](AGENT_TOOLS.md) for families.

An agent without a parent ends itself only under the `suspension`
experiment, whose daemon writes a delay into its spec
(`AgentSpec.experimental["suspension/1"]`, from `[experiments.suspension]
after`, 30 minutes unless set). The delay counts from the start of the
agent's **idle stretch**. Idle is `Running` with no turn, nothing queued or
pending, every child's last report taken, no provider subagent running
(Claude's or Codex's own, which would die with the provider), no terminal
client attached (the driver feeds the attached count as an input), and
nothing the interpreter says the provider holds (`Interpreter::holds`:
background jobs, a turn the provider owes for a job that ended between
turns, a scheduled wake-up, or a rule or directory amux granted for the
session, none of which survive a kill and resume; a permission mode set for
the session, as approving a plan does, is not one, since the snapshot
records it and the resume starts in it). The stretch starts the moment the
agent is idle, which is the end of its last turn (a person's, another
agent's, a wake-up's) or the moment one of those holds lets go, and again
at a turn that begins and ends between two steps. Anything that makes the
agent not idle ends the stretch, with one exception: the interpreter's own
idle housekeeping.

That housekeeping is an [idle compaction](INTERPRETERS.md#idle-compaction),
which an interpreter runs at most once a stretch, at a time of its own. It
is nobody's turn, though the agent reads as working while it runs, and it
neither ends the stretch nor starts one. The interpreter reports where it
stands (`Interpreter::housekeeping`): due at a time, running, or settled
(done, or nothing worth doing: a small conversation, a cache gone cold or
not one that outlives the wait). The core arms a suspension timer only
while the agent is idle and the housekeeping settled, for the later of the
stretch's start plus the delay and now, and cancels it the moment either is
not so. So a suspension comes at the later of the delay and the
housekeeping's end, whichever constant is the larger, and a compaction
never restarts the delay. With the 30-minute default:

| Conversation | Compacts | Suspends |
|---|---|---|
| Small, or not on a cache worth keeping | never | at 30 minutes |
| Large, Codex | at 25 minutes | at 30 minutes |
| Large, headless Claude on the hour cache | at 55 minutes | as the compaction ends |
| Large, under a delay longer than the compaction point | at its time | at the delay |

The core checks after every step that the timer runs only while the agent
is idle (`agent.suspension_while_busy`), and that it never suspends, and its
timer never runs, while the housekeeping is due or running
(`agent.suspension_before_housekeeping`). A hold that lets go starts a new
stretch, so the delay counts from its end: someone at the terminal, or a
job or subagent at work, is the agent in use, which amux's own housekeeping
is not. A suspended agent has no process to arm a wake timer, so it is
never woken to compact, which is why it waits for a compaction that is
due rather than leaving the conversation to go cold.

When the timer fires the exit is the ordinary graceful one with cause
`SUSPENDED`: there is no turn to cut short and nothing running in the
background. A message arriving as it exits is refused `REFUSAL_EXITING`, as
for a one-shot agent. A provider subagent has no process or spec of its
own, so it never suspends; its parent suspends only once none runs.

### A suspended agent on its host

The daemon leaves a suspended agent's row **live**, phase idle, with
`SUSPENDED` on it, so a paired host or client of any build sees an idle agent:
none stops its children, and none holds its reports as owed to an exited
parent. Only the agent's own host knows it has no process, and handles it so
whether or not the experiment is on, since switching it off stops only new
suspensions:

- the start's sweep leaves such a row as it is;
- any message resumes it, with the message as the new incarnation's first
  input: the person's, another agent's, or a child's report, which never
  resumes an exited agent. A report the agent refuses as exiting waits for
  the exit and begins again against the row;
- a person's input wakes it the same way, whichever client sent it and
  whether it was made on its own host or carried from another: the host finds no process
  to relay the input to, and asks the core for a resume that goes ahead
  only if the row is still suspended when its turn comes, after any exit it
  waits out. The row reads suspended from the commit of the step that ends
  the incarnation, while the process is still finishing and may already
  have stopped reading input, so a process whose row carries its exit's
  cause is one the resume waits out rather than refusing as running. The
  row is what the person read, so the wake decides from the same fact,
  and a daemon restarted in that window finds the cause there too. A
  prompt is the new incarnation's first input and is answered as taken;
  any other input goes to the new process once it connects. Input typed
  to an agent stopped in the meantime starts nothing and is refused as
  exited. Input the agent refuses as exiting waits for the exit when the
  agent is leaving of its own accord, as it does when it suspends, and
  then wakes it if the exit left it suspended or is refused as exited;
  input to an agent the daemon is stopping is answered as exiting at once,
  not after the stop's deadline;
- stopping or closing it marks it exited with cause `STOPPED` (closed, for a
  close), and its children follow as for any exit; deleting it removes it;
- it is no awake member of its workspace while it has no process, and a
  resume takes the workspace's claim before it launches;
- `amux attach` wakes it, with no input, before it dials the terminal: it
  asks the resume only of a suspended agent (`if_suspended`), since the row
  it goes by may be out of date, and an agent stopped since stays stopped.

A child's report is for one incarnation of its parent. Under the experiment
the parent's host keeps, in `exp_suspension_1_runs`, the incarnation each of
its agents' current run began at: a resume of a suspended agent continues
the run, any other resume starts a new one, and a report for the run's first
incarnation or a later one counts. Without an entry only the current
incarnation's report counts. The child's host drops a report itself only
for a parent row that is gone or exited under another incarnation, or a host
no longer paired; a live parent under another incarnation may have been
suspended rather than stopped, so the report goes to its host, which answers
that the incarnation is gone when its run is.

### A restart of the machine

Nothing is written at shutdown, so a clean shutdown, a power cut and a crash
are the same; the next start decides, from each agent's journal, under the
experiment. When its sweep finds an agent's process gone, its journal giving
no cause, and the machine booted since the daemon before (the boot id in
`<data_dir>/generation`), it decides as below. A shutdown can end the
provider before the agent process, and then the journal does give a cause,
the provider's own end: a signal, which amux never sends a provider, since
its own stops and kills end with causes of their own (`PROVIDER_EXITED`,
"killed by a signal", or the exit status 128 and the signal's number a
provider that catches it gives: 129, 130, 137 or 143 for SIGHUP, SIGINT,
SIGKILL and SIGTERM, as headless Claude exits 143); or a clean exit
(`QUIT`), as a provider that exits cleanly when signalled gives (Codex's
app server does, even mid-turn). Each counts as no cause here. A quit is
recorded as one when the machine did not restart. Only terminal Claude can
be quit by a person, in its terminal (Codex's terminal is a view that
leaves its app server running), so its quit in a turn is the person's and
is never continued. The interpreter's last
snapshot after a provider's own end reads idle unless it cut work off, a
turn or a provider subagent still running (which ends with it), so the
phase tells the two apart:

- an agent the person closed was on its way out, and is marked exited as
  stopped;
- an idle one, or one waiting on its children, is left suspended;
- either way, a provider subagent its journal left running ends now, with
  the cause it would have taken had the agent been marked exited (ended
  while away, unless the journal said how the provider ended): it ran
  inside the process the restart killed;
- one in its turn, or with a provider subagent still running (its row
  still live: the journal never ended it), is suspended and resumed at
  once with *"The machine
  restarted while you were working; carry on where you left off."*, which
  continues its run (a wake, so a stop the person asked for while the
  journal was being read holds); the provider finds the cut-off turn in its own
  transcript, and a question it was asking is asked again by the retried
  tool call.

A process found gone on the same boot is a fault, marked exited as before.
So that a shutdown never publishes an exit a paired host would stop
children for, an end whose journal gives no cause, or only the provider's
own end as above, is recorded only after a grace of three seconds
(`CAUSELESS_GRACE_MS`), and once the daemon is asked
to stop (a signal, a client, its supervisor handing over) it records no
exit at all.

### Why suspension is shaped this way

Suspension must be invisible: on any client and any paired host, builds
before it included, a suspended agent is an idle one that any message
reaches. Each choice above follows from that, and these were rejected:

- **Suspending agents with a parent.** They already end themselves by the
  one-shot rule once their work is done, and a message resumes them, so the
  processes that linger are those of agents without a parent.
- **The daemon deciding,** from quiet and busy reports the agent sends, with
  a timer of its own and a stop that checks again before it acts. The agent
  core already knows every condition, its children included, and an input
  that meets the exit is already handled by the one-shot path: refused as
  exiting, then woken.
- **Letting an idle compaction count as activity,** which cancelled the
  timer and re-armed it from the compaction's end. Whether a conversation
  was compacted before it went cold then depended on which constant was
  larger: under the default delay a large headless Claude conversation
  suspended at 30 minutes, before its compaction at 55, and was read
  uncached on the next prompt.
- **The core knowing each provider's compaction point,** to arm the timer
  past it. The interpreter alone knows whether a compaction is worth
  running (the conversation's size, which cache it is on, whether that has
  expired), so it reports due, running or settled, and the core waits on
  that.
- **Recording a suspension as an exit,** a row that reads exited with a new
  cause newer clients draw as idle. Every older host and client would see an
  exited agent, stop its children and drop the reports owed to it, and a
  great deal of code branches on whether a row is live.
- **Gating the suspended-row handling on the switch.** Rows already
  suspended would be stranded when a person switched the experiment off,
  so only new suspensions stop.
- **Publishing the run's first incarnation on the row,** so a child's host
  could judge a report's staleness itself. It needs a wire field and a
  replica table to tell other hosts what only the parent's host needs to
  know. **Keeping the incarnation number across a suspension** fails too:
  every spec file, journal boundary and envelope dedupe is per incarnation.
- **Asking the person to continue an agent a restart cut off in its turn.**
  A person who restarts a machine almost always wants the work to carry
  on, a step repeated by an automatic continuation is no riskier than one
  repeated after a typed "continue", and unattended work survives only this
  way. It applies only after a reboot: on the same boot, a command the dead
  process started may still run, and continuing the turn could run it
  twice.
- **Marking the continuation with a new kind of sender, or a reason on the
  resume's boundary.** amux has only agent and person senders, and older
  clients would get one they could not place. A fixed marker in the
  prompt's input id needs no protocol field and no change to the
  interpreters, which already keep the input id on a prompt's item.
- **Writing anything at shutdown.** The operating system signals agents and
  the daemon in no promised order, and a power cut writes nothing, so the
  next start decides from each journal, and the grace keeps a daemon that
  is itself about to be signalled from publishing an exit first.

## The control socket

`ctl.sock` carries `CtlFrame` messages
([`agent_process.proto`](../proto/amux/v1/agent_process.proto)), each a varint
length followed by the encoded message, the same framing the journal uses for
steps. A frame over 64 MiB is treated as a broken peer. The surface has no
version integer, and a frame a reader does not know is ignored.

| Frame | Direction | Meaning |
|---|---|---|
| `hello` | agent → daemon | Sent at once on every connection: the agent id |
| `nudge` | agent → daemon | The journal grew. Sent after every step that wrote bytes; an empty step writes nothing and sends nothing |
| `input` | daemon → agent | A person's `Input` (a prompt, an answer, an interrupt, a setting, …), its body as the client encoded it |
| `child_input` | daemon → agent | `ChildInput { agent_id, input_id, of }`: a person's act on one of the provider subagents this agent's provider runs, either an `Input` of the subagent's kind answering one of its asks, or a `StopJob { key }` naming a job in the subagent's own snapshot (its own job, keyed by the provider's id for it, stops it). The daemon sends the job stop as the shared `StopJob` for every kind, so it never builds a kind's input. The core hands it to the interpreter as `Event::ChildInput`, through the facts ring like any input, and the interpreter answers the provider for the subagent; the `reply` is keyed by the input id, and a draining or exiting agent refuses it as it refuses its own input. The daemon refuses every other input to a subagent itself |
| `agent_message` | daemon → agent | An `Envelope` with the sender the daemon set; the recipient's interpreter picks the carrier |
| `dump_request` | daemon → agent | `AgentDumpRequest { dump_id }`: asks for the agent's record for a report, answered in any mode |
| `rename` | daemon → agent | `Rename { name }`: a Codex agent's thread takes the agent's new name, so Codex's own app shows what amux does; other kinds have nothing to rename |
| `stop` | daemon → agent | `Stop { kill }`: without `kill`, cancel the turn the provider's own way and exit; with it, kill everything below the provider and its process group at once (the daemon's escalation past the stop deadline) |
| `read_git` | daemon → agent | `ReadGit {}`: read the folder's [git facts](#git-facts) again, as at a turn end; a draining or exiting agent ignores it |
| `children` | daemon → agent | `Children { children }`: the agents directly below this one that are live, on every host its daemon knows of, each with its incarnation, sent whenever the list changes and when the daemon connects: always to an agent it found running (even with none, which settles a spawn the last daemon took and never answered), and to one it has just started only if it has any; a one-shot agent waits for each incarnation's last report |
| `dump` | agent → daemon | `DumpPart` holding the agent's record, answering a dump request |
| `dump_asked` | agent → daemon | `DumpAsked { reason, invariant, record }`: the agent's core asked for a report (the invariant it broke, or none for a step over its budget), with its record built then. The daemon writes the one report, with that record for this agent, after a limit of its own per kind; a step over its budget is sent with no record, and the daemon counts it and writes no report; one asked with no daemon connected waits, the newest of each kind, and goes right after the next `hello` |
| `reply` | agent → daemon | `Verdict { id, verdict }`: the interpreter's verdict on an input, keyed by its input id, which the daemon relays as the `SendInput` reply; or on an agent message, keyed by its envelope id, which settles the message's delivery |

Within one step the order is fixed: blobs the step refers to are written
first, then the step is appended to the journal, then the daemon is nudged,
then the effects run (replies are sent, bytes go to the provider). A reply
therefore never reaches the daemon before the step that explains it is on
disk. Before each event, when the clock has moved since the last one, the
core feeds the interpreter a `Tick`, so the interpreter never reads a clock
itself. When the interpreter asks to hear the clock at a time no event may
bring (`Interpreter::wake_at`, for an idle compaction), the core arms its
wake timer for it and feeds the `Tick` when it fires, while the agent runs.

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
any transcript rows a follower has not read yet, the agent's own and then
its subagents'.

A provider's background jobs do not outlive the agent's exit: every provider
ends them on SIGTERM, and the census kills any it left. A provider process killed on its
own leaves them running, orphaned, and terminal Claude's `/exit` lets the
person move the session and its jobs into Claude's own background, where
they keep running after the pane closes. Either way the agent's list of
jobs is emptied at the exit ([background jobs](INTERPRETERS.md#background-jobs)).

| Kind | Child | Facts | Input |
|---|---|---|---|
| `claude_pty` | `claude` in a pseudo-terminal | Hook payloads on `private/hooks.sock`, transcript rows, and the agent's own launch, trust-dialog, ready and watched facts | Decisions written back on the hooks Claude waits on, and keystrokes for what has no hook |
| `claude_sdk` | `claude --print` over stream-JSON on stdin and stdout | Each stdout line | Stream-JSON lines on stdin |
| `codex` | `codex app-server` listening on `private/codex.sock` (stdio on Windows), the agent one of its clients | Each JSON-RPC message the server sends the agent | JSON-RPC messages from the agent's client |

Terminal Claude is hosted on Unix only. On Windows an agent of kind
`claude_pty` ends before spawning anything, with the cause "could not start
the provider: terminal Claude is not hosted on Windows in this build;
headless Claude and Codex are"; the other two kinds run there as here.
[Windows, as a stated cost](ARCHITECTURE.md#windows-as-a-stated-cost) gives
the reasons.

### Terminal Claude

Before Claude starts, the agent runs `claude --version` and reports the
version as its launch fact, ahead of anything Claude reports; the boundary
item records it. Claude older than 2.1.251, the first with every
hook the agent registers, is refused: the agent ends before spawning it,
with the cause "could not start: the provider: terminal Claude needs
Claude Code 2.1.251 or later; this machine has …", and `amux attach --in
native` to it answers with that cause. An older Claude would start
and never report the events the interpreter follows. A version the agent
cannot read starts anyway, since a wrapper script or an unfamiliar banner
is likelier than an old Claude.

Claude is started through `pty_host::spawn` with the spec's arguments plus:

- `--session-id <id>` for the first incarnation, `--resume <id>` for later
  ones. The id is a UUID the agent makes up and keeps in
  `private/provider-session`. A session Claude never began (it exited at its
  folder-trust dialog, say) has no transcript and cannot be resumed, so it is
  started again under the same id. One whose transcript holds no user or
  assistant row (Claude stopped as it wrote its first message, leaving only
  metadata) can neither be resumed nor started again under that id, so a new
  id replaces it. The check looks where Claude does: a resume across the
  projects Claude searches, a new session in this folder's project alone.
- `--settings <json>`: the user's own settings with the agent's merged over
  them. The agent's part registers `<install path> internal hook claude`, as a
  program and its arguments with no shell between, as the hook command for
  `SessionStart`, `SessionEnd`, `UserPromptSubmit`, `PermissionRequest`,
  `PermissionDenied`, `PreToolUse`, `PostToolUse`, `PostToolUseFailure`,
  `Stop`, `SubagentStart`, `Elicitation`, `MessageDisplay` and
  `PostModelSwitch`, the two that wait for a decision with a timeout of
  seven days, accepts messages from other sessions at once, and allows amux's own tools
  (all but `attach_file`, which asks like any read) and reading the agent's `blobs/` folder (where the files a person attaches
  are) without asking. Headless Claude gets the same settings.
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

The agent also sends a watched fact when the first terminal client shows
Claude's screen and again when the last one stops: a client shows it from
its hello on `pty.sock` until it leaves, or until it says it no longer
does, as the terminal client does when it goes to its fleet with the
connection kept (`PtyFrame.shown`). Claude draws a tool server's form or
link only once its `Elicitation` hook returns, so while someone looks at
Claude's own terminal the interpreter lets that hook go and leaves the
dialog to Claude. A new agent process counts its clients afresh.

Nothing else is read off the screen. The permission Claude runs in is the
one amux cycled it to with Shift+Tab, confirmed by the permission Claude
records next; the permission approving a plan switches to follows from the
launch arguments and the catalogue (see [the interpreters](INTERPRETERS.md)).

Keystrokes are typed by one task, in order, with the pauses a paste needs
(see [keys](INTERPRETERS.md#keys)); what an attached terminal types goes
through the same task, so it never interleaves with a prompt half-way.

### Headless Claude

The agent runs the spec's command with the spec's arguments plus
`--print --input-format stream-json --output-format stream-json --verbose
--include-partial-messages --permission-prompt-tool stdio
--replay-user-messages`, `--session-id <id>` or `--resume <id>` (kept in
`private/provider-session` as for terminal Claude, and replaced the same
way), `--settings <json>` and `--mcp-config`. It also appends one sentence to
Claude's system prompt, joined to any `--append-system-prompt` the spec's
arguments carry: write a Bash call's `description` before its `command`.
Claude streams a call's input in the order the model writes its fields, and
by its own tool definition the description comes last, after a command that
can take the model many seconds to write; asked, it writes the description
first, so the call's row can open on it.
A reset gives Claude a new session, which the interpreter names once Claude's
next `init` reports it; the agent saves it and names it on later user
messages. Claude leads a process group of its own and finishes its turn
after its input closes, so on Unix it is tethered as a Codex server on a
socket is (see [Codex](#codex)): an agent that dies, however it dies, takes
it with it. Claude reports nothing until
asked, so the agent writes an `initialize` control request at once, built
from `claude_protocol::stream` types like every line written to Claude; its
answer is what says Claude takes input, and it lists the models and commands
the agent offers.

The interpreter writes everything after that: user messages carry a UUID
derived from the input or envelope id, which Claude echoes on its replay of
the message, and images a person attached go as base64 image blocks beside
the element that names their blob. Closing the child is closing its stdin
and sending it SIGTERM. Its `initialize` declares `perTaskStopAffordance`:
amux's interrupt ends the turn and leaves background work running on every
kind, and Claude spares background subagents on an interrupt, as it spares
background shells, only for a host that says it can stop each task itself.
amux can: a person stops one job with `stop_task` (see
[stopping one job](INTERPRETERS.md#background-jobs)).

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
API on), `initialized`, then `thread/start` (id `agent-thread`), or for a
later incarnation `thread/resume` (id `agent-resume`) with the thread id from
`private/provider-session` and `excludeTurns`, as Codex's own app resumes, so
a long thread's history never has to fit one message; both carry the spec's
working directory and model. Messages from the server's socket may be up to
128 MiB, past the websocket library's 16 MiB frame default, since one item can
carry a turn's screenshots. A resume Codex refuses because the thread is
archived is answered by the interpreter with `thread/unarchive` and a second
resume; one refused because the thread's history is gone ("no rollout found")
draws a notice and starts a new thread (`amux-fresh-thread`), which becomes
the agent's conversation. A killed agent's server, told to finish
by its tether, can take seconds to exit and holds the thread until it does,
refusing anyone else's resume ("thread … already has an active writer"); the
agent asks again every quarter of a second for up to 30 seconds before it
hands the refusal on, and a refusal handed on ends the incarnation with
Codex's words rather than leave it starting. The thread id a `thread/start`
answers with is written to `private/provider-session`, as is the thread a
`/clear` starts (the interpreter's `thread/start`, ids `amux-clear-<n>`) and
the one that replaces a thread with no history: from then on that thread is
the agent's conversation. A resume keeps the thread it resumed. Straight after, the interpreter names the thread
with the agent's name (`thread/name/set`), and names it again when the agent
is renamed: the daemon tells a running agent with a `rename` frame, and a
resumed agent's spec carries its current name, so Codex's own app shows the
name amux does. An agent nobody named has its thread left unnamed until its
title comes, and the thread then takes the title ([INTERPRETERS](INTERPRETERS.md)). A thread that has not run a turn has nothing on disk, and
Codex's own app, which resumes from what is on disk, cannot attach to it;
naming does not change that. When a started thread has no turns the
interpreter also reads it with its turns (`thread/read`), which has Codex
write it, so the app can attach before the first turn; a resumed thread is
already on disk, and its answer has no turns because the resume left them
out. Every message the server sends the agent is a fact; the
interpreter writes every request after the handshake with ids of its own
(`amux-<n>`). A turn whose prompt carries attachments has them appended to
its input, each as the element text the model reads (naming it and where it
is), and an image then also as a local image at its blob's path, so the
model can tell which attached image is which.

Closing the agent's connection ends a stdio server. On a socket, the end of
the agent's connection (closed by the agent, or dropped by the server) is
when the agent sends the server SIGTERM; Codex then finishes any running
turn and exits. Nothing tells a socket server that the agent died, so the
agent also starts a tether beside it: a shell in the server's process group
reading a pipe only the agent holds. When the agent dies, however it dies,
the pipe closes and the shell sends the server's group SIGTERM, and, if the
server is still running five seconds later (one with an approval open
ignores SIGTERM, and would hold its thread from the next incarnation for
good), SIGKILL. After the server exits on its own, the agent lets the tether
go, which ends whatever the server left in its group.

## Git facts

The snapshot carries the agent folder's branch, base branch and change
totals, which the fleet row shows. The agent process reads them with the
[`git-facts`](../crates/git-facts/src/lib.rs) crate at start, again each
time a turn ends, and whenever the daemon sends `read_git`, off its event
loop and one read at a time, and feeds each result to the interpreter as an
event; the snapshot changes only when the facts do.

Several agents can work in one checkout, and the totals compare the whole
folder with HEAD, so one agent's commit, or a person's, changes what every
other agent's row counts. The daemon therefore sends `read_git` to every
other agent of its host with a live process whose folder is the same as, or
inside or around, the folder of an agent whose turn just ended; judging the
checkout by path costs no git, and a folder it wrongly includes costs only
a read that finds nothing new. Only the agent's own reads write its row's
totals: `GetChanges` compares the folder for whoever asks and stores
nothing, so an agent without a live process, such as a stopped or suspended
one, keeps the totals of its last read. A folder outside a repository, or a
machine without git, has none.
The base is the branch the agent's worktree was made from when its host
made one (the spec's `base_branch`), else the repository's default branch:
what `origin/HEAD` names, else the configured default, else `main` or
`master`. On the base branch itself the counts on the branch run from where
it left its upstream, so commits not yet pushed count; with no upstream they
are the uncommitted work.

## The facts ring

`private/facts/` records every event the interpreter was fed, in order, so
its state can be rebuilt and a dump can show what the provider actually said.
It is segmented like the journal, and each segment has a checkpoint beside it,
`<offset>.checkpoint`: the interpreter's whole state before the segment's
first entry.

- An entry is one JSON line: a fact with its channel and its payload (as text
  when it is UTF-8, as hex otherwise; terminal Claude's subagent transcript
  rows on a channel of their own, `subagent_transcript`, apart from its main
  transcript's), a person's input, a person's input to one of its provider
  subagents (`child_input`, the encoded `ChildInput`) or an agent message
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

The agent's part of a report is its record: these segments run again
through the kind's interpreter from their checkpoints, keeping what it
concluded and none of what was said (`dump.rs`). The segments themselves
never leave the machine. See [the agent's record](DEBUGGING.md#the-agents-record).

## Hooks and the transcript

Terminal Claude has no protocol a program can read. What the agent knows
about it comes from its hooks, its transcript file and its terminal.

A hook reaches the agent like this:

1. Claude runs the hook command, `<install path> internal hook claude`, for
   each registered hook event, with the payload on stdin.
2. The command reads `CLAUDE_HOOK_SOCKET`, connects to `private/hooks.sock`,
   writes the payload and closes its side for writing, then prints whatever
   the agent writes back before closing, which Claude reads as the hook's
   decision. When Claude put its
   messaging socket's path and token in the hook's environment
   (`CLAUDE_CODE_MESSAGING_SOCKET`, `CLAUDE_CODE_MESSAGING_TOKEN`), the
   command wraps the payload in an envelope that carries them
   (`claude::hooks::forward_from_env`).
3. The command always exits 0, even when it could not deliver: Claude reads a
   hook's exit code as a verdict, and 2 would block the action.
4. The agent reads one connection at a time, in the order Claude ran its
   hooks. Anything Claude runs can reach the socket, so a payload is held to
   what Claude writes, and dropped otherwise: its `session_id` is a UUID,
   and a `transcript_path` it names is that session's `<id>.jsonl` in
   Claude's project folder for the working folder or a sibling git
   worktree. Messaging credentials go to the provider, which uses them to
   hand agent messages to Claude; the payload Claude wrote becomes a hook
   fact, which the interpreter decodes with `claude_protocol::hooks`. Once a
   session has started, the interpreter ignores any hook but a
   `SessionStart` that names another session.
5. A `PermissionRequest` or `Elicitation` connection is held open: Claude
   waits on it for amux's decision. The agent numbers it and adds the
   number to the payload as `amux_reply`; the interpreter's answer
   (`Effect::HookAnswer`) is written on the connection with that number,
   which then closes. An empty answer closes it with no decision, so Claude
   carries on with its own dialog. At most 64 are held at once; a payload
   past that is dropped. Every other connection is closed as soon
   as it is read, so Claude waits on it no longer than the delivery takes.
   If the agent goes away, the connection closes, and Claude falls back to
   its own dialog as it does for an empty answer.

The transcript is found through the hooks. `SessionStart` names the
transcript file; when the path changes (a start, a resume, `/clear`, a
compaction) the interpreter asks the agent to follow it. A follower holds the
file open and reads only what was written since it last looked, when the
platform reports a change (kqueue on macOS, inotify on Linux, to the file
once it exists and to the nearest folder above it until then) and at least
once a second in case no report comes. It passes on whole lines only, keeps
the start of a line still being written until its newline arrives, and
records its position in `private/transcript-cursor` (the offset and the
file's path) after every row. A file gone, shorter than what was read or
replaced by another is followed no further. The files share one watcher, so
a session's hundred subagents cost no more watchers than one; `tail.rs`
holds the reader and its watch.
A later incarnation following the same file continues from the saved
position, so a resumed session's rows are not read twice.

Each subagent's conversation is a file of its own beside the transcript,
`<session>/subagents/agent-<id>.jsonl`, with `agent-<id>.meta.json` naming
the call that launched it. When Claude's `SubagentStart` hook names one, the
interpreter asks the agent to follow its file (`FollowSubagent`). A subagent
has one follower, as a continued one writes on in the same file. Its follower sends
the meta first (rows wait up to half a second for it), then the user,
assistant and system rows, on the `subagent_transcript` channel; Claude's
attachment and bookkeeping rows are not sent. Each keeps its own position in
`private/subagent-cursors/<id>`. A follower reads its file as the
transcript's does, and one follows each subagent, finished ones included,
until Claude exits. A drained one lets its watch, its open file and its
timers go and waits only to be asked again: Claude fires `SubagentStart`
for a subagent its model continues before the subagent writes. A file it can no
longer read, or one now shorter than what it read, is reported once and its
follower ends. The interpreter asks again at each `SubagentStart` for a
subagent, as Claude fires one when its model continues a subagent: that
wakes a follower to read at once, or
starts anew, from the saved position, one that ended. Claude
can tell the agent a subagent finished before writing its last rows, so the
interpreter asks for the file to be drained (`DrainSubagent`): the follower
reads it to its end and reports it drained: at once, or for a completed
subagent as soon as it reads the final reply the interpreter names, giving
up on it after two seconds.

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
`amux internal mcp <dir>`, a separate process that the provider launches from the
install path, like any MCP server it is configured with; it is not the agent
process. The agent only names it in the provider's launch (`--mcp-config`
for Claude, `--config mcp_servers.amux.…` for Codex), and only when the
spec's configuration has an install path. It speaks newline-delimited
JSON-RPC over stdio. Its `initialize` answer carries short instructions
saying what each tool is for: Claude defers MCP tools behind its tool
search and shows the model only their names, but always shows a server's
instructions, so they are worded the way a search for the tools would be.

Where an organization manages Claude's MCP servers (a `managed-mcp.json` in
the managed-settings folder: `/Library/Application Support/ClaudeCode`,
`/etc/claude-code` or `C:\Program Files\ClaudeCode`, or the folder
`CLAUDE_CODE_MANAGED_SETTINGS_PATH` names), Claude refuses a server added on
its command line and does not start. The agent then launches Claude, either
kind, without amux's tool server and tells its interpreter so on the agent
channel; the agent's tool-server health lists `amux` as failed, saying the
organization's policy turned amux's tools off. The agent says so at each
launch, so an incarnation launched once the policy is gone reads amux's
server as Claude reports it. The host's catalogue ask
leaves out `--strict-mcp-config` there for the same reason.

Four fleet tools (agents, hosts, send, stop) dial the daemon at
`<dir>/tools.sock`, a socket the daemon listens on for this agent alone, so
the daemon knows who is calling from the connection and nothing in a request
claims an id. A daemon that adopts a live agent after a restart binds the
socket again. A call made while the daemon is restarting retries for five
seconds; with no daemon it answers "amux daemon isn't running". The other two
never reach the daemon: status answers ok and the interpreter reads the call
from the provider's own record, and attach writes the file into `blobs/` and
returns the element the model puts in its reply. The tool server never writes
the journal; the agent process is its only writer. [Agent tools](AGENT_TOOLS.md)
describes each tool.

The fifth fleet tool, spawn, is the one call the tool server makes of the
agent process, on `<dir>/private/agent.sock`, and the one call the agent
process makes of the daemon. The socket is for whatever a tool server needs
of its agent process: each connection carries one `AgentAsk` and its
`AgentAnswer`, framed as control frames are, and each says which kind it
is, so a later build can add an ask. A process that reads an ask it does
not know refuses it in words. A spawn's ask is the `CreateAgentRequest` the
child's host takes, with the child's agent id the tool server chose, in the
`spawn` arm, and that host's id in `host_id` (empty for the agent's
own host): the tool server resolves a host the model named to its id before
it asks, so a name never reaches the process. The answer is the host's
`CreateAgentResponse` or the words of a refusal. For
each spawn the process
([`spawns.rs`](../crates/agent/src/spawns.rs)) tells its core the child was
asked for and waits until the core has it; makes the create on `tools.sock`,
addressed to that host by its id, so the daemon carries it there when it is
another, redialling for five seconds and making the call itself at most once; tells its core how the daemon answered and waits
again; and only then answers the tool server. The provider cannot go on
with its turn, let alone end it, before the core knows what became of the
spawn. A request that arrives once the core is gone, the process exiting, is
dropped unmade, and the tool server reports that its agent process went
away. A tool server that finds no such socket says its agent process is not
taking spawn requests and asks nobody else.

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
   boundary with the cause `DAEMON_LOST`, and it exits.
4. A pending ask is mid-turn, not an end. An agent draining after the daemon
   was lost with an ask open waits for the drain deadline (the
   `agent.drain_secs` setting, five minutes by default) and exits `ORPHANED`
   ("orphaned while waiting for you"). A resume asks again, since providers
   drop an unanswered tool call when they resume.

The finished turn is in the journal. Whichever daemon comes next ingests the
rest of it, shows the agent as exited, and a resume (from any client) writes
the next `spec.<n>` and starts the directory again with the same id. It reads
the agent's own cause from the journal's last step, so the row says
`DAEMON_LOST` or `ORPHANED` whether the daemon came back during the drain or
after the process was gone. A process found gone whose journal says nothing
of why is recorded `ENDED_WHILE_AWAY`.

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
| Idle before an agent without a parent suspends, counted from its idle stretch's start and never before its idle compaction settles | 30 minutes, while the `suspension` experiment is on | `[experiments.suspension] after`, copied into the spec's `experimental["suspension/1"]`; never without that entry |
| Facts ring size, across its two segments | 4 MiB | `agent.facts_ring_mib`, copied into `facts_ring_bytes` |
| A step of the agent's core, before it asks for a dump | 10 ms | the spec's `step_budget_micros` (`Launch.agent_step_budget_micros`, not a configuration key); `StepBudget::AGENT` in `agent-core` if the spec leaves it zero |
| Journal segment size | 1 MiB | `JOURNAL_SEGMENT_BYTES` in `lib.rs` |
| Terminal log | 4 segments of 256 KiB | constants in `host.rs` |
| Provider asked to finish, then killed | 10 s | `PROVIDER_STOP_MS` in `agent-core` |
| Output collected after a child exits | 300 ms | `TRAILING_OUTPUT` in `provider.rs` |
| A followed transcript is looked at with no change reported | 1 s | `FALLBACK_POLL` in `tail.rs` |
| A subagent's rows wait for its meta; a drain waits at most for its final reply | 500 ms; 2 s | `META_WAIT`, `DRAIN_WAIT` in `claude/subagents.rs` |
| Terminal Claude's first screen: quiet, cap | 100 ms, 500 ms | `FIRST_SCREEN_QUIET`, `FIRST_SCREEN_WAIT` in `provider.rs` |
| Codex view asked to end, then killed | 2 s | `VIEW_STOP` in `attach.rs` |
| Tool server retry window | 5 s | `RETRY_WINDOW` in `tools.rs` |

Every deadline reads the injected `agent_dir::Clock`, so tests drive them by
hand with `ManualClock`. [Parameters](PARAMETERS.md) lists every tunable in
the system.

## Testing

| Suite | Run it with | What it holds |
|---|---|---|
| Provider hosting | `just suite agent-provider-hosting` | Each kind's child is launched, completes its handshake and a turn, continues its own session in a later incarnation, and takes agent messages through its own channel; a Codex thread is named after the agent at start and on rename |
| Lifecycle | `just suite agent-process-lifecycle` | Grace, drain, abort, kill, a failed journal write and a provider exit, with every deadline driven by the test's clock; the snapshot's git facts at start and after a turn |
| Raw attach | `just suite agent-terminal-attach` | Two clients share terminal Claude's one terminal; each Codex client gets its own view |
| Codex attach | `just suite desktop-cli-system codex_attach::` | Codex's own app joins a fresh agent's server; its prompt shows in amux's chat, and an approval is answered from amux, then from the app |
| Codex attach, live | `just live codex attach` | The same against the real Codex, from amux's client and Codex's app in two terminals: a timed capture of each (`app.cast`, `amux.cast`, raw bytes), a text frame of both per step and a verdict, in `target/live/codex-attach/` |
| Dump | `just suite agent-dump-redaction` | The dump part carries no planted secret |
| Tool server | `just suite agent-tool-server` | Tool calls reach a scripted daemon on `tools.sock`, a spawn by way of the agent process's own socket; they retry across an update window and come back as the items the interpreter draws; a subagent stays for a child it spawned that its daemon has not named |

These run the real process code against the scripted fake providers in
[`crates/provider-fakes`](../crates/provider-fakes/src/lib.rs) and recorded
replays in `crates/agent/tests/replay`. [Testing](TESTING.md) explains the
suites, lanes and the live provider lane.
