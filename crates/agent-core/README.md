# agent-core

The agent process around its interpreter as one function of its state and
an input. `Agent::step` takes a provider fact, a daemon connecting or going
away, a control frame, a person's input, an agent message, a timer or a
job's result, and appends the `Output`s the agent process wants done:
journal appends, provider writes, control-socket frames, timers, and the
exit. The incarnation, the grace and drain timers, the queue, the pending
set and the open ask live here, around the pure interpreter from
`interpret`; the driver runs the provider child, the sockets and fsync.

What belongs inside: anything whose interleaving with the core's state can
matter is in the core; anything that only produces a result is a job the
driver runs and reports back as an input; transport and codec are the
driver's, so the core sees decoded messages and never a byte of a stream.
Nothing inside a step locks, spawns, awaits or reads the clock. The
invariants cheap enough to check at a step's end are checked inside `step`
in production, and a violation is a dump: the step appends `Output::Dump`
with the violation as its reason rather than carrying on. These are the
[architecture page's principles](../../docs/ARCHITECTURE.md#the-mental-model)
applied to the agent process.
