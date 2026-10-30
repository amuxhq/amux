# daemon-core

The daemon as one function of its state and an input. `Daemon::step` takes a
decoded `Input` (a tick, a link or channel event, a peer's or client's call,
an agent's nudge, a job's result) and appends the `Output`s the daemon wants
done; a driver around it runs the sockets, timers, tonic and disk. The core owns the profile's store outright, and every call,
timer and job the driver holds for it is named by a `CallId`, `TimerId` or
`JobId`. `checkpoint` and `restore` take the core's state out and back in
beside the store it was taken with.

Pairing and trust are the core's too. The paired hosts are rows of the
profile's store, read when the core is made and changed only by core steps,
one at a time, each committed in the step that makes it: pairing, unpairing,
a key replacement and a pairing's rollback are synced to the drive before
they are answered (the sync's time is left out of the step budget), the
addresses and facts a host reports of itself are ordinary commits. The
shell's handshakes read the copy the driver publishes. The pairing window (its secret, attempts, lockout and expiry), the
SPAKE2 exchange, key confirmation and the sealed identities run as steps over
`PairingMessage` inputs, with each conversation's deadline and each pending
pairing's token as timers; the driver carries only the pairing streams. An
SSH pairing commits through the same trust writes. Randomness arrives as
`Input::Entropy` and is stretched by counting, so a run is a function of its
inputs.

Reaching paired hosts is the core's as well (`reach`). It holds one attempt
per host at a time, from its `Output::Dial` until its `Input::AttemptEnded`,
each on every path it knows the host by: where discovery found it and where
it last answered, its SSH destination, the paired hosts and the relay that
claim it. An attempt has ten seconds; one that ends without a link is
followed by a pause (one second doubling to thirty) that a new path or a call
wanting the host cuts short, and a host whose link dropped within a second
waits out the rest of that second before the next; a host that refused
this one as no longer trusted is not dialled until something says it may have
taken that back. The same addresses go to the link's path machine
(`Output::PeerAddresses`), so a link up on one path moves to another that
answers. These deadlines and pauses, and how long a link lived, are reckoned
in link time, the second clock `Input::Tick` carries, which runs on when a
test holds policy time still.

What belongs inside: anything whose interleaving with the core's state can
matter is in the core; anything that only produces a result is a job the
driver runs and reports back as an input; transport and codec are the
driver's, so the core sees decoded messages and never a byte of a stream.
Nothing inside a step locks, spawns, awaits or reads the clock, except to
time the step itself. The invariants cheap enough to check at a step's end
are checked inside `step` in production, and a violation is a dump: the
step appends `Output::Dump` with the violation as its reason rather than
carrying on. These are the
[architecture page's principles](../../docs/ARCHITECTURE.md#the-mental-model)
applied to the daemon.

The invariants checked so far, each at the boundary a step crossed and in
constant time per row it touched:

- no answer carries a revision of this host's agent that the store has not
  committed;
- an agent's journal is reclaimed only below the cursor of the newest
  checkpoint on the drive;
- the generation this host's streams open under only ever rises;
- nothing is committed while a checkpoint moves the WAL into the main file.

A step that takes longer than its budget (`StartOptions::budget`, 100 ms by
default) is logged and appends `Output::Dump` too, one per step that ran
over. A checker, simulator or replay runs the core with
`StepBudget::UNLIMITED`, so its outputs depend on its inputs alone.
