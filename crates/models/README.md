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

Run the models with `just test-crate models`.
