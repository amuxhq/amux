# models

The protocol's promises, written once as properties and explored over the real
code.

A model is a `Machine`: a slice of real state, the actions its environment may
take, a transition that calls the real code, and a list of properties. Each
property reads as a sentence a person would argue over and is a predicate over
the state:

- **always** holds in every explored state;
- **sometimes** holds in at least one, so an `always` cannot pass on a machine
  that does nothing;
- **eventually** holds somewhere on every path that ends because no action is
  enabled. The checker has no fairness, so a model writes liveness in with a
  bound on consecutive faults.

`check(model, bounds)` explores every state breadth first within the bounds and
prints the shortest path to a failing state. `walk(model, bounds, seed)` follows
one schedule chosen by the seed: the same seed walks the same schedule on every
run, so a failing walk is reproduced by its seed alone.

## Faults

What the world may do to a machine between its own actions is one
vocabulary, `Fault`, shared by every model's environment and by the
simulator: a message in flight lost, delayed or reordered, a host crashing,
rebooting uncleanly or losing power, and the clock moving on. Each model
offers the faults its slice of the world can suffer and says how each plays
out on it; nothing else is simulated.

## Parked properties

When the checker finds a property failing and what to do about it is a
decision still waiting, the property is parked rather than weakened: it
keeps its sentence and predicate, and `.parked(Parked { since,
counterexample, reason })` records when, the path that broke it and the
decision it waits on. The checker reports a parked property with its reason
instead of exploring it, so `just test` stays green meanwhile, and
`just parked-check` lists every parked property and fails while any
remains, so nothing lands with one.

## Replication

`Replication` follows one agent from its origin to one replica, both real
daemon cores over real in-memory stores, over a stream that delivers in
order and may die at any message. The replica's core follows the origin's
inventory and keeps a source on the agent: it decides when to open a
stream and what to ask for, absorbs what arrives, and asks the origin's
core for a page when a client fetches below its block. The origin commits
keys, trims its oldest row and reboots uncleanly, losing its newest commit
and minting those revisions again under the next generation. Two states
are one when their stores, the link and the replica's source stand alike;
the cores' checkpoints ride along, since what else differs between them is
the numbering of their calls and the length of a backoff. The properties
are in `src/replication.rs`, each a sentence over the two stores.

## Single daemon under crash

`SingleDaemon` runs the real daemon core over a real in-memory store and one
agent's journal laid out as files, and holds the core as the checkpoint it
takes of itself. The agent writes frames and nudges; a client subscribes
and may fall behind its queue; the checkpoint timer fires, the WAL job
moves the WAL into the store's main file or finds it busy while inputs go on
arriving and the core holds those that would write, and the checkpoint job
writes, finishes or fails; the page cache writes the journal back; the
drive may refuse a commit; and the daemon crashes, loses power or reboots
cleanly at any point between, then starts again under the generation the
installation decides. What the drive holds is kept apart from what the
process sees: a power loss takes the store back to its last passive WAL
checkpoint and the journal back to its last writeback.

Every step is also replayed the way a dump will be: the core restored from
the newest checkpoint the driver's input ring holds, over the store it was
taken beside, stepped through every input since against the journal as each
step found it, must come out as the live core, store and journal. The
properties are in `src/single_daemon.rs`.

## Presence and routing

`PresenceRouting` follows what one host reads as on a real daemon core
whose links to the account's relay and to one peer are already up. The
host's own links come up and go, the peer and the relay start and stop
claiming to reach it, each attempt the core asks for opens a channel, is
refused, times out, is refused by the relay for the account's plan or
fails because its route went, a call opens a channel of its own on the
first route (its direct link may go under it, and the call then tries the
route the core names next), a call finds no channel within its patience
(the core may close the host's own links for it), the plan is bought and
lapses, the retry timer fires, and at any point the world may stop
changing. The relay
refuses for the plan for as long as the plan lapses; the model bounds how
many such refusals come in a row so that a path retrying only the relay
ends and its liveness is judged. The properties are in `src/presence.rs`.

## Family and deliveries

`FamilyDeliveries` runs two hosts' real daemon cores, each over its own
in-memory store and its agents' journals laid out as files: a parent agent
on one host and, once a client spawns it, its child on the other, which
follows the parent's host's inventory. A spawn the child's host refuses
makes no child and may be tried again. The world spawns, stops and deletes
the parent, lets either process exit, ends the child's turn (writing a
report for its parent into the child's outbox), lets the child's listing
catch up, takes a link down and up, unpairs the child's host and crashes
either host; it may stop changing at any point. The drivers answer jobs at
once, fire a timer armed for now at once and a later one only as the
model's own action, within a bound on such waits, and carry each call and
its answer between the hosts as the model's actions, so a report may be
written to the parent's process, its verdict lost and the report handed
again. Two states are one when the cores' fingerprints, the stores, the
journals and the armed timers match: `Daemon::fingerprint` is the core's
state with the calls, jobs and sessions it numbered ranked among those in
use and its pass counters and backoffs left out. What must happen
eventually is judged where the world rests with no timer still armed; a
path the bound on waits cut short is not judged. The properties are in
`src/family.rs`.

## Agent lifecycle

`AgentLifecycle` runs the real agent core around the test kind's
interpreter, held between steps as its value. A fake provider records a
prompt it is handed and runs a turn on it, takes an injected message at
once, and ends its turn, opens an ask or exits when the model chooses; a
stop cancels its turn, ask and all. The daemon dials in, a new connection
may replace an old one before it closes, connections close, and the daemon
hands a person's input or an agent message, stops or kills the agent, and
resumes it with a parent's message it refused while exiting, again after
every incarnation that exits before its journal records the message taken,
as the daemon's message lane does. The resume replays the facts ring the
last incarnation recorded, as the driver does. Time moves only when the
grace or drain timer fires. Two configurations are checked: a one-shot
child started on its spawn prompt, and an agent without a parent. The
properties are in `src/lifecycle.rs`.

## Supervisor update

`SupervisorUpdate` runs the real supervisor core, held between steps as
its value, against what is true on the machine: the binary at the install
path, prev beside it, a staged download, the rejected-build record, the
daemon processes and what the release feed publishes. The driver performs
the core's outputs in order, answers a look at the feed and a download at
once, starts a daemon from whatever binary is at the path and kills one at
once; when the core hands over, the installed binary starts as a
supervisor that inherits the serving daemon. The feed publishes newer
releases, each good or bad (a daemon from a bad build never prepares); a
daemon prepares, receives `go`, cannot be started, exits when asked or
crashes; the start, stop and restart timers fire; `amux update` asks for a
look; and the supervisor crashes, also before any file output of a step,
so every prefix of a swap or a rollback is explored, and starts again from
the binary at the path while its daemons exit with their pipe. Both swap
orders are checked: the binary renamed over while the daemon runs, and the
daemon stopped first. The properties are in `src/supervisor.rs`.

## The composed net

`ComposedNet` takes a topology (hosts, the links between them and the
agents each starts with) and runs it whole: every host's real daemon core
over its own in-memory store and its agents' journals, every agent's real
agent core around the test kind's interpreter with a scripted fake
provider, and the drivers' glue between them, so a call, its answer, a job
and its result are each a delivery waiting in a lane. Its initial state is
the topology started with its agents created. Its actions are the
environment's and a person's: a lane's next delivery (or a later one, within
a bound on reorderings), a call's message lost, a link severed and restored
once both ends have heard it go, a daemon killed and started again or
rebooted uncleanly under its next generation, the clock moved on to the
next timer, a prompt sent to the agent and a stop asked of its host. Its
properties are the promises of every machine above that a net of two hosts
and one agent can break, judged over the same stores (no core asks for a
dump, replication's four promises over the block, hosts on a working link
read each other online, an accepted input keeps its row), beside the ones
that hold only between machines: a reset arriving while presence flips
still leaves the replica current under the newest generation, and the
listing a far host's family acts on never reads an agent exited before
the agent's own host does. Two nets are one when the cores' fingerprints,
the stores, the journals, the timers and what waits in each lane match,
with the calls and jobs a core numbered ranked as its fingerprint ranks
them; the ranking is not exact, so on several threads the count may differ
by a few dozen states between runs while the verdict does not. The
checked topology is two linked hosts and one agent whose one turn writes
three items, explored under three sets of faults. Walked one seeded
delivery at a time, the same model is the testnet's simulated mode. The
properties are in `src/net/mod.rs`.

## Cost, bounds and budget

A model explores real stores, so a state costs a store transaction or two
rather than a few instructions, and its bounds are sized to a time budget.
Every model's test prints its measured cost as `states`, `seconds` and
`states_per_second`, and fails when the exhaustive run outlasts its budget
under `just test`.

| Model | CI bounds | States | 12 threads | 4 threads | Four-core runner | Budget | Nightly bounds | Nightly, 12 threads |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| Replication | 3 keys, cap 2, revisions up to 4, 2 generations, 2 in flight, 1 loss in a row | 207,443 | 37 s | 66 s | 274 s | 600 s | revisions up to 5, 3 in flight | 333 s |
| Single daemon | 4 frames, 2 checkpoints, 1 crash, power loss or reboot, 1 subscription, 1 lag | 207,594 | 51 s | 68 s | not yet measured (199 s before the checkpoint's WAL job) | 600 s | 2 crashes, power losses or reboots, 1 refused commit | 527 s |
| Family and deliveries | 1 turn end, the parent's spontaneous exit, 1 link down, 1 crash, 1 wait | 391,552 | 39 s | 71 s | 161 s | 360 s | the child's spontaneous exit too, 2 links down, 2 waits | 740 s |
| Presence and routing | 3 links or claims up, 2 down, 2 refused or timed-out attempts or calls finding no channel, 1 call opening its own channel, 2 plan changes, 2 refusals for the plan in a row | 1,134,868 | 31 s | 68 s | not yet measured (134 s at 859,652 states) | 600 s | 4 up, 3 down, 3 failures | 431 s |
| Agent lifecycle, one-shot child and parentless agent | 3 daemon connections, 3 inputs, 2 messages, 2 asks, 1 unasked provider exit, 2 stops or kills | 1,275,996 and 71,250 | 8 s | 17 s | 44 s for both | 120 s each | 4 connections, 3 messages, 2 unasked exits | 81 s |
| Supervisor update, both swap orders | 2 releases, 2 supervisor crashes, 2 daemon crashes, 1 failed start, 5 timers firing, 1 `amux update` | 4,189,038 and 4,449,354 | 5 s | 11 s | not yet measured | 120 s each | 3 releases, 2 failed starts, 6 timers firing | 77 s |
| Composed net: a reset while presence flips, lost messages, a person's calls through a crash | 1 sever and 1 unclean reboot; 1 reordering, 1 lost message and 1 advance; 1 kill, 1 prompt and 1 stop | 33,025, 9,559 and 194,762 | 23 s | 45 s | not yet measured | 420 s for all three | every fault but a kill, with 1 reordering; the calls through a crash with 1 lost message | 184 s |

Measured on 2026-10-18 and 2026-10-19 on the enrolled Mac (Mac14,6, 12
cores), with the development profile, which optimises SQLite, the store and
the models; `MODELS_THREADS=4` gives the four-thread figure. The checker
runs on every core the machine has; the verdict and the state count do not
depend on the number of threads, only which of several equally short
counterexamples is printed.

The four-core runner column is each model's time at its CI bounds on CI's
four-core Linux runner, measured by CI run
[39185561814](https://github.com/jordanwebster/amux/actions/runs/39185561814);
the lifecycle figure is its test binary, both models together.

Each model runs in CI as a job of its own on that runner, and its budget is
sized to it: **a model's budget is at least twice its measured time on the
four-core runner at its CI bounds, rounded up to the next full minute, and no
budget is above 600 s.** A model with no runner figure yet is sized from its
four-thread Mac time times 4.2, the slowest runner-to-Mac ratio measured (the
runner took 2.1 to 4.2 times the Mac's four-thread time; replication, the
model heaviest on the store, was the slowest), doubled and rounded up the same
way, until a CI run measures it. A model whose doubled runner time would pass
600 s has its CI bounds cut, and the bounds it loses move to its nightly
test. The nightly bounds run daily in their own workflow, each model in a job
of its own, and locally through `just models-nightly`.

A walk is reproducible: one seed walks one schedule on every run, and
another seed walks another (`one_seed_walks_one_replication_schedule`).

Run the models with `just test-crate models`, one with
`just test-crate models -- --test <target>`, and the nightly bounds with
`just models-nightly`.
