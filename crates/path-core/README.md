# path-core

Which path one link's packets take, as one function of its state and an
input. A link is one QUIC connection; beneath it the packets can go straight
to one of the peer's addresses, through a forwarding link (the cloud relay's
or a paired daemon's), or over a byte stream such as SSH. `Paths::step` takes
what the socket layer saw (candidates learned or gone, probes answered or
timed out, packets arriving, forwarding links up, down or refusing, the
network changing, the account's plan, the clock) and answers with what to do:
probe a path, carry the link on one, send on every candidate, forget an
address, or wake it at a time. There is one machine per link, and one per
remembered host that has no link yet.

The rules it keeps:

- A path carries the link only after a probe on it was answered; until one
  has, the link's packets go on every candidate at once.
- A path that skips the forwarders is preferred, and the link moves only to a
  better kind of path, never off a working one within the hold-down.
- While the link is direct the relay path is kept ready by probing it each
  interval, so falling back costs no validation.
- An address only the relay's observation gave is reached by punching, which
  is paid: a free account never probes one, and a lapse drops it.
- A network change makes every path stale and probes them all at once.

Like the other cores, nothing inside a step locks, spawns, awaits or reads
the clock; the invariants are checked at the end of every step, and a
violation is a dump: the step appends `Output::Dump` with the violation as
its reason. These are the
[architecture page's principles](../../docs/ARCHITECTURE.md#the-mental-model)
applied to choosing a path.
