# daemon-core

The daemon as one function of its state and an input. `Daemon::step` takes a
decoded `Input` (a tick, a link or channel event, a peer's or client's call,
an agent's nudge, a job's result) and appends the `Output`s the daemon wants
done; a driver around it runs the sockets, timers, tonic and disk. The core owns the profile's store outright, and every call,
timer and job the driver holds for it is named by a `CallId`, `TimerId` or
`JobId`. `checkpoint` and `restore` take the core's state out and back in
beside the store it was taken with.

What belongs inside: anything whose interleaving with the core's state can
matter is in the core; anything that only produces a result is a job the
driver runs and reports back as an input; transport and codec are the
driver's, so the core sees decoded messages and never a byte of a stream.
Nothing inside a step locks, spawns, awaits or reads the clock. The
invariants cheap enough to check at a step's end are checked inside `step`
in production, and a violation is a dump: the step appends `Output::Dump`
with the violation as its reason rather than carrying on. These are the
[architecture page's principles](../../docs/ARCHITECTURE.md#the-mental-model)
applied to the daemon.
