# relay-core

The relay's admission and forwarding decisions as one function of its state
and an input. `Relay::step` takes link events and, for each pair of hosts the
driver has not been told about, the header of a forwarded packet; it judges
the account's claims, tier and adjacency, and answers with a pair to forward
between or a refusal for the sender's control link, repeated at most every
`REFUSAL_REPEAT_MS` per destination. A pair is installed only between two
links up on one account, both paid or both pinned, and a link going down,
lapsing to the free plan or its token expiring ends every pair through it and
tells both ends. The driver checks token signatures before admission and
forwards from its table of pairs without asking again. A daemon forwarding
between its own paired hosts runs one too, as a relay with one account whose
links are all pinned. It also keeps the adjacency discipline every host's
links follow: which of a crossed dial's two links stays, and what each link is
told of its neighbors. Its step budget is tighter than the daemon's because
admission must never queue.

What belongs inside: anything whose interleaving with the core's state can
matter is in the core; anything that only produces a result is a job the
driver runs and reports back as an input; transport and codec are the
driver's, so the core sees decoded messages and never a byte of a packet.
Nothing inside a step locks, spawns, awaits or reads the clock. The
invariants cheap enough to check at a step's end are checked inside `step`
in production, and a violation is a dump: the step appends `Output::Dump`
with the violation as its reason rather than carrying on. These are the
[architecture page's principles](../../docs/ARCHITECTURE.md#the-mental-model)
applied to the relay.
