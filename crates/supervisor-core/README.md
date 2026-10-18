# supervisor-core

The supervisor's update decisions as one function of its state and an input.
`Supervisor::step` takes what the release feed listed, what a fetch staged,
what a daemon did (prepared, activated, exited, never started), the requests
on the control socket and its timers, and says which daemon to start, tell
`go`, stop or kill, which release to fetch, which files beside the binary to
move, and when to hand over to the installed binary or exit. Processes, the
activation pipe, downloads and renames are the driver's, in
`crates/node/src/supervisor`.

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
