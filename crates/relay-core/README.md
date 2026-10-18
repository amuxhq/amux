# relay-core

The relay's admission and splice decisions as one function of its state and
an input. `Relay::step` takes link events and the decoded preface of each
stream a host opens, judges the account's claims, tier and adjacency, and
answers with a refusal or a splice; the driver checks token signatures
before admission and copies a spliced stream's bytes on tokio's workers. A
daemon forwarding between its own links runs one too, as a relay with one
account. It also keeps the adjacency discipline every host's links follow:
which of a crossed dial's two links stays, and what each link is told of its
neighbors. Its step budget is tighter than the daemon's because admission must
never queue.

What belongs inside: anything whose interleaving with the core's state can
matter is in the core; anything that only produces a result is a job the
driver runs and reports back as an input; transport and codec are the
driver's, so the core sees decoded messages and never a byte of a stream.
Nothing inside a step locks, spawns, awaits or reads the clock. The
invariants cheap enough to check at a step's end are checked inside `step`
in production, and a violation is a dump: the step appends `Output::Dump`
with the violation as its reason rather than carrying on. These are the
[architecture page's principles](../../docs/ARCHITECTURE.md#the-mental-model)
applied to the relay.
