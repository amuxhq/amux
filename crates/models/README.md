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
and may fall behind its queue; the checkpoint timer fires and the
checkpoint job writes, finishes or fails; the page cache writes the journal
back; the drive may refuse a commit; and the daemon crashes, loses power or
reboots cleanly, then starts again under the generation the installation
decides. What the drive holds is kept apart from what the process sees: a
power loss takes the store back to its last passive WAL checkpoint and the
journal back to its last writeback.

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

## Cost, bounds and budget

A model explores real stores, so a state costs a store transaction or two
rather than a few instructions, and its bounds are sized to a time budget.
Every model's test prints its measured cost as `states`, `seconds` and
`states_per_second`, and fails when the exhaustive run outlasts its budget
under `just test`.

| Model | CI bounds | States | 12 threads | 4 threads | Budget | Nightly bounds | Nightly, 12 threads |
| --- | --- | --- | --- | --- | --- | --- | --- |
| Replication | 3 keys, cap 2, revisions up to 4, 2 generations, 2 in flight, 1 loss in a row | 207,443 | 37 s | 66 s | 240 s | revisions up to 5, 3 in flight | NIGHTLY_REPLICATION |
| Single daemon | 4 frames, 2 checkpoints, 1 crash, power loss or reboot, 1 subscription, 1 lag, 1 refused commit | 254,451 | 61 s | 96 s | 300 s | 2 crashes, power losses or reboots | NIGHTLY_SINGLE |
| Family and deliveries | 1 turn end, the parent's spontaneous exit, 1 link down, 1 crash, 1 wait | 391,552 | 39 s | 71 s | 240 s | the child's spontaneous exit too, 2 links down, 2 waits | NIGHTLY_FAMILY |
| Presence and routing | 3 links or claims up, 2 down, 2 refused or timed-out attempts or calls finding no channel, 1 call opening its own channel, 2 plan changes, 2 refusals for the plan in a row | 859,652 | 24 s | 52 s | 180 s | 5 up, 4 down, 3 failures, 2 calls | NIGHTLY_PRESENCE |

Measured on 2026-10-18 and 2026-10-19 on the enrolled Mac (Mac14,6, 12
cores), with the development profile, which optimises SQLite, the store and
the models; `MODELS_THREADS=4` gives the four-thread figure. The checker
runs on every core the machine has; the verdict and the state count do not
depend on the number of threads, only which of several equally short
counterexamples is printed.

Each model runs in CI as a job of its own on a four-core Linux runner, and
its budget is sized to that runner: **three times the model's four-thread
time on the Mac fits inside its budget, and no budget is above 600 s.** One
CI run measured the ratio: the family model took 136 s on the Mac's 12
threads and 918 s on a four-core Windows runner, and was stopped past 760 s
on a four-core Linux one, about 2 to 2.5 times the Mac's time per thread.
A new model is sized by the same rule; one that does not fit has its CI
bounds cut, and the bounds it loses move to its nightly test. The nightly
bounds run daily in their own workflow, each model in a job of its own, and
locally through `just models-nightly`.

A walk is reproducible: one seed walks one schedule on every run, and
another seed walks another (`one_seed_walks_one_replication_schedule`).

Run the models with `just test-crate models`, one with
`just test-crate models -- --test <target>`, and the nightly bounds with
`just models-nightly`.
