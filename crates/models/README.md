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
fails because its route went, the plan is bought and lapses, the retry
timer fires, and at any point the world may stop changing. The relay
refuses for the plan for as long as the plan lapses; the model bounds how
many such refusals come in a row so that a path retrying only the relay
ends and its liveness is judged. The properties are in `src/presence.rs`.

## Cost, bounds and budget

A model explores real stores, so a state costs a store transaction or two
rather than a few instructions, and its bounds are sized to a time budget.
Every model's test prints its measured cost as `states`, `seconds` and
`states_per_second`, and fails when the exhaustive run outlasts its budget
under `just test`.

| Model | CI bounds | States | Seconds | states_per_second | Budget | Nightly bounds |
| --- | --- | --- | --- | --- | --- | --- |
| Replication | 3 keys, cap 2, revisions up to 4, 2 generations, 2 in flight, 1 loss in a row | 207,443 | 37 | 5,700 | 120 s | revisions up to 5, 3 in flight |
| Single daemon | 4 frames, 2 checkpoints, 1 crash, power loss or reboot, 1 subscription, 1 lag, 1 refused commit | 179,245 | 34 (125 on one thread) | 5,300 (1,430 on one thread) | 120 s | 2 crashes, power losses or reboots: 910,364 states in 148 s |
| Presence and routing | 3 links or claims up, 2 down, 2 refused or timed-out attempts, 2 plan changes, 2 refusals for the plan in a row | 118,374 | 2 (12 on one thread) | 62,000 (9,900 on one thread) | 60 s | 5 up, 4 down, 3 failed attempts: 4,367,842 states in 91 s |

Measured on 2026-10-18 on the enrolled Mac (Mac14,6, 12 cores), with the
development profile, which optimises SQLite, the store and the models. The
checker runs on every core; the verdict and the state count do not depend
on the number of threads, only which of several equally short
counterexamples is printed. The budget leaves room for a smaller CI runner
or a busy machine.

A walk is reproducible: one seed walks one schedule on every run, and
another seed walks another (`one_seed_walks_one_replication_schedule`).

Run the models with `just test-crate models`.
