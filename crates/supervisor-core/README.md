# supervisor-core

The supervisor's update decisions as one function of its state and an input.
`Supervisor::step` takes what the release feed listed, what a start did and
how the daemon exited, and says which release to fetch, which binary to
start, when to stop or kill the daemon, which binary to set aside, and when
to go or roll back. Processes, downloads and renames are the driver's.

What belongs inside: anything whose interleaving with the core's state can
matter is in the core; anything that only produces a result is a job the
driver runs and reports back as an input; transport and codec are the
driver's, so the core sees decoded messages and never a byte of a stream.
Nothing inside a step locks, spawns, awaits or reads the clock. The
invariants cheap enough to check at a step's end are checked inside `step`
in production, and a violation is a dump: the step appends `Output::Dump`
with the violation as its reason rather than carrying on. These are the
[architecture page's principles](../../docs/ARCHITECTURE.md#the-mental-model)
applied to the supervisor.
