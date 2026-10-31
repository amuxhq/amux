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
claim it. An attempt has ten seconds. A path that turns it away (a
machine with another key, an endpoint refusing connections) is left out and
the rest are dialled again at once within the same ten seconds. At a
pairing the joiner dials and the offerer waits up to ten seconds for that
link, a call on the offerer meanwhile waiting with it, so the two dials do
not cross; a host that closed its link saying goodbye as it stopped is left
the same ten seconds to dial back as it starts again, so its first dial and
the catch-up it carries are not turned away by a dial from here; a host a
peer announces as just arrived, reached by nothing before, is likewise left
the same ten seconds to dial, an attempt out to it dropped, since the
peer's list of who was there named this host to it, while a host named in
a peer's list is dialled from here; an attempt
that ends without a link otherwise is followed by a pause (one second doubling to thirty) that a new path or a call
wanting the host cuts short, and a host whose link dropped within a second
waits out the rest of that second before the next; a host that refused
this one as no longer trusted is not dialled until something says it may have
taken that back. The same addresses go to the link's path machine
(`Output::PeerAddresses`), so a link up on one path moves to another that
answers. These deadlines and pauses, and how long a link lived, are reckoned
in link time, the second clock `Input::Tick` carries, which runs on when a
test holds policy time still.

The relay link is reached the same way (`reach::relay`): one attempt at a
time while the profile is signed in (`Input::RelayWanted`), from
`Output::DialRelay` until `Input::RelayEnded`. The driver fetches the token
and names the relay host (`Input::RelayPrepared`); the core chooses the
carriers then: QUIC with TLS over TCP after a 300 ms head start, or TLS alone
to a host found to drop UDP within the hour (`Input::RelayUdp`, in policy
time). After a lost link or a failed attempt it pauses a second doubling to
five minutes, jittered from the entropy it is given, starting afresh after a
clean close or a link that lived thirty seconds; a refused kept token is
replaced at once, and a refusal of the credentials or the version stops it
until the profile signs in again. The device's network changing
(`Input::NetworkChanged`) ends a pause and starts the count afresh; an
attempt out or a link up is left to QUIC, which moves a live connection to
the new network.

A host to pair with is dialled on the plan `Daemon::pairing_dial` gives:
every address at once, those discovery found the host at before those the
request named, an earlier one preferred over a later answer while within its
two-second short try, each with five seconds in all; then the forwarders that
claim the host, paired hosts before the relay.

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
