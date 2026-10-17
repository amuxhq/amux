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

## Replication

`Replication` follows one agent from its origin to one replica, both real
in-memory stores, over a stream that delivers in order and may die at any
message. The origin commits keys, trims its oldest row and reboots
uncleanly, losing its newest commit and minting those revisions again under
the next generation; the replica opens streams, absorbs what they carry and
pages below its block. The properties are in `src/replication.rs`, each a
sentence over the two stores.

## Cost, bounds and budget

A model explores real stores, so a state costs a store transaction or two
rather than a few instructions, and its bounds are sized to a time budget.
Every model's test prints its measured cost as `states`, `seconds` and
`states_per_second`, and fails when the exhaustive run outlasts its budget
under `just test`.

| Model | CI bounds | States | Seconds | states_per_second | Budget | Nightly bounds |
| --- | --- | --- | --- | --- | --- | --- |
| Replication | 3 keys, cap 2, revisions up to 4, 2 generations, 2 in flight, 1 loss in a row | 194,522 | 18 (94 on one thread) | 11,000 (2,070 on one thread) | 120 s | revisions up to 5, 3 in flight: 2,038,361 states in 186 s |

Measured on 2026-10-18 on the enrolled Mac (Mac14,6, 12 cores), with the
development profile, which optimises SQLite, the store and the models. The
checker runs on every core; the verdict and the state count do not depend
on the number of threads, only which of several equally short
counterexamples is printed. The budget leaves room for a smaller CI runner
or a busy machine.

A walk is reproducible: one seed walks one schedule on every run, and
another seed walks another (`one_seed_walks_one_replication_schedule`).

Run the models with `just test-crate models`.
