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

`check(model, bounds)` explores every state within the bounds and prints a path
to a failing state. It explores depth first, holding only each state's
fingerprint and the path it is on; breadth first holds every frontier state with
its path, and at nightly bounds the client model passed 53 GB that way on the
enrolled Mac without finishing, where depth first finished in 175 s and peaked
at 1.3 GB. A bounded depth explores breadth first, since depth first would
skip a state first met past the bound, and so prints the shortest path to a
failure. `walk(model, bounds, seed)` follows
one schedule chosen by the seed: the same seed walks the same schedule on every
run, so a failing walk is reproduced by its seed alone.

## Replication

`Replication` follows one agent from its origin to one replica, both real
daemon cores over real in-memory stores, over a stream that delivers in
order and may die at any message. The replica's core follows the origin's
inventory and keeps a source on the agent: it decides when to open a
stream and what to ask for, absorbs what arrives, and asks the origin's
core for a page when a client fetches below its block; the origin answers
the page at once, and the answer reaches the replica later, after whatever
the stream delivers meanwhile, or not at all when the origin reboots
first. The origin commits
keys, trims its oldest row and reboots uncleanly, losing its newest commit
and minting those revisions again under the next generation. Two states
are one when their stores, the link and the replica's source stand alike;
the cores' checkpoints ride along, since what else differs between them is
the numbering of their calls and the length of a backoff. The replica's
link to the origin is resumed, so the opening it asks for after the origin
reboots rides it as early data, which the origin's driver serves only once
its core has taken the link; and that opening may be replayed on a
connection of its own, whose answers go to whoever replayed it. The
properties are in `src/replication.rs`, each a sentence over the two stores.

## Single daemon under crash

`SingleDaemon` runs the real daemon core over a real in-memory store and one
agent's journal laid out as files, and holds the core as the checkpoint it
takes of itself. The agent writes frames and nudges; a client subscribes
and may fall behind its queue; the checkpoint timer fires, the WAL job
moves the WAL into the store's main file or finds it busy while inputs go on
arriving and the core holds those that would write, and the checkpoint job
writes, finishes or fails; the page cache writes the journal back; the
drive may refuse a commit; and the daemon crashes, loses power or reboots
cleanly at any point between, then starts again under the generation the
installation decides. What the drive holds is kept apart from what the
process sees: a power loss takes the store back to its last passive WAL
checkpoint and the journal back to its last writeback.

Every step is also replayed the way a dump will be: the core restored from
the newest checkpoint the driver's input ring holds, over the store it was
taken beside, stepped through every input since against the journal as each
step found it, must come out as the live core, store and journal.

A paired peer comes back on resumed links, each carrying an early read of
the agent that the driver serves only once the core has taken the link;
anyone holding those packets may replay them on a connection of their own;
and someone may ask this host to remove the peer's pin, which the core
writes through the driver before the driver closes the peer's link. The
peer has bounds of its own, small around the journal, explored beside the
journal's in the same test. The properties are in `src/single_daemon.rs`.

## Presence and routing

`PresenceRouting` follows what one host reads as on a real daemon core at
launch: its links to the account's relay and to one peer are up, and
neither has said anything of the host, which this daemon remembers and
dials at once on every path. So the host's own links may come up before
any claim of it, alternately on its own connection and over SSH; they go
(a call failing on one closes it), the peer and the relay start and stop
claiming to reach it, the relay refuses its packets for no route while its
claim lingers, and the shell dials the host through the first host
claiming it whenever the core says to, again after each failed try: a
dial through the peer brings up a link of the host's own forwarded end to
end, and so does one through the relay while the plan pays for
connections between hosts, which the relay refuses for the plan
otherwise. The plan is bought and lapses (the relay enforces the change
at once and this daemon hears of it later), and at any point the world
may stop changing. The model bounds how many refusals for the plan come
in a row so that a path dialling only the relay ends and its liveness is
judged. The properties are in `src/presence.rs`.

## Family and deliveries

`FamilyDeliveries` runs two hosts' real daemon cores, each over its own
in-memory store and its agents' journals laid out as files: a parent agent
on one host and, once a client spawns it, its child on the other, which
follows the parent's host's inventory. The child's host starts following
before the parent is made, so its first listing predates the parent and a
child spawned before that listing catches up is one it must not judge. A
spawn the child's host refuses makes no child and may be tried again. The world spawns, stops and deletes
the parent, lets either process exit, ends the child's turn (writing a
report for its parent into the child's outbox), lets the child's listing
catch up, takes a link down and up, unpairs the child's host and crashes
either host; it may stop changing at any point. The drivers answer jobs at
once, fire a timer armed for now at once and a later one only as the
model's own action, within a bound on such waits, and carry each call and
its answer between the hosts as the model's actions, so a report may be
written to the parent's process, its verdict lost and the report handed
again. Two states are one when the cores' fingerprints, the stores, the
journals and the armed timers match: `Daemon::fingerprint` is the core's
state with the calls, jobs and sessions it numbered ranked among those in
use and its pass counters and backoffs left out. What must happen
eventually is judged where the world rests with no timer still armed; a
path the bound on waits cut short is not judged. The properties are in
`src/family.rs`.

## Agent lifecycle

`AgentLifecycle` runs the real agent core around the test kind's
interpreter, held between steps as its value. A fake provider records a
prompt it is handed and runs a turn on it, takes an injected message at
once, and ends its turn, opens an ask or exits when the model chooses; a
stop cancels its turn, ask and all. The daemon dials in, a new connection
may replace an old one before it closes, connections close, and the daemon
hands a person's input or an agent message, stops or kills the agent, and
resumes it with a parent's message it refused while exiting, again after
every incarnation that exits before its journal records the message taken,
as the daemon's message lane does. The resume replays the facts ring the
last incarnation recorded, as the driver does. Time moves only when the
grace or drain timer fires. Two configurations are checked: a one-shot
child started on its spawn prompt, and an agent without a parent. The
properties are in `src/lifecycle.rs`.

## Supervisor update

`SupervisorUpdate` runs the real supervisor core, held between steps as
its value, against what is true on the machine: the binary at the install
path, prev beside it, a staged download, the rejected-build record, the
daemon processes and what the release feed publishes. The driver performs
the core's outputs in order, answers a look at the feed and a download at
once, starts a daemon from whatever binary is at the path and kills one at
once; when the core hands over, the installed binary starts as a
supervisor that inherits the serving daemon. The feed publishes newer
releases, each good or bad (a daemon from a bad build never prepares); a
daemon prepares, receives `go`, cannot be started, exits when asked or
crashes; the start, stop and restart timers fire; `amux update` asks for a
look; and the supervisor crashes, also before any file output of a step
and between the two moves of an install or a rollback, so every prefix of a
swap or a rollback is explored, and starts again from the binary at the
path while its daemons exit with their pipe. A crash or power loss is the
same here: the daemons die with the supervisor. Both platforms' swaps are
checked. Where a running binary can be renamed over, the daemon keeps
running, an install links prev to the binary and renames the staged one
over the path, and a supervisor that starts between the two finds prev is
its own binary and removes it. Where it cannot (Windows), the daemon stops
first, an install renames the binary to prev and the staged one into the
empty path, and a rollback over the supervisor's own running image renames
that aside before putting prev back. Between a Windows install's or
rollback's two renames the path is empty, and a supervisor that dies there
is not started again: the property that a binary can always start names
that window as its exception. The properties are in `src/supervisor.rs`.

## One open chat

`ClientModel` runs the real `ui_state::SessionState`, the session both
clients reduce a chat's stream into, fed what its driver forwards. Around
it, one agent's daemon writes new items, appends, whole revisions and
snapshots; the stream carries each write in order behind an opening (the
generation, the snapshot, a tail of the newest items and CaughtUp); the
stream falls behind and is reopened at once with a fresh tail; the
origin's generation changes (the newest item gone, every other revised,
and the stream carrying what a replica publishes for the new block: a
Reset naming its generation, the snapshot, a fresh tail and CaughtUp); the
client asks for the page below its oldest
row and the answer lands later, as the daemon read it when asked; and the
person sends one prompt, which reaches the daemon or not, is answered
accepted or lost, and may be discarded while unconfirmed. An append whose
base the client does not hold is answered with Get before the next
message, as the driver does. Beside the client the state keeps a client
that received every write uninterrupted, which a catch-up plus the live
stream must read as at rest, and the session as its stream last opened
with every message since, which is what a client's dump records and must
replay to the live session. Two states are one when they print alike, so
the session keeps its indexes in ordered maps. The daemon's items are
prose and the prompt, with real bodies, so the views draw each as its
text. The properties are in `src/client.rs`. Seeded walks of this model
also hold the phone's chat deltas to the session's window
(`crates/app-runtime/src/schedules.rs`).

## The path machine

`PathModel` runs the real `path_core::Paths` for one link to a peer the
world can reach three ways: at its address on the local network, which
works while this host is on that network; at the address the relay saw it
at, by punching, which works until this host's network changes; and through
the relay, which forwards while its link is up and the plan is paid. A
fourth address, one that worked on a network this host has left, never
answers. The machine learns the addresses late and one at a time; each probe
it sends is answered by a path that works or is lost, and a probe with no
answer times out as the clock reaches it; while the link is on the local
address, that address may go silent for long enough that three probes on it
in a row go unanswered, nothing else changing meanwhile, and the world rests
once it answers again; this host's network changes; the
relay link goes down and comes back; the plan lapses or is bought again, and
the relay refuses the lapsed plan's packets once it sees them; the link
comes up once the first flight reaches the peer on a path that works and
idles out when none does; and the clock moves to whatever wake the machine
asked for. Router mappings expiring play out as lost probes on the punched
path. At any point the world may stop changing; the clock still moves a
bounded number of times so the machine settles, and its liveness is judged
there.

Two more worlds add what the peer and the relay say beside the probes,
each on a smaller world so that it stays quick to explore. In the first, one of
the link's packets arrives, on a path the machine sends on or on a byte
stream it has not heard on, and a paired daemon's forwarding link comes up
and goes, while the network stays put, the relay link stays up and the
local address keeps answering. In the second, an answer arrives for a path
with no probe of it outstanding (late, duplicated, or from before the
network changed; never on a path whose probe the machine still waits on,
since the driver passes only the outstanding probe's answer), the relay
refuses for no route or the rate limit, the peer stops
listing an address, and the peer lists an IPv6 address as near as its IPv4
one, while the relay link stays up and the local address keeps answering.
The properties read the world's own record of what the machine asked for
against what the world knows of each path, and are in `src/path.rs`.

## The relay's forwarding

`RelayModel` runs the real `relay_core::Relay` for three hosts reaching the
relay: a desk and a laptop on one account and a phone on another. A host's
link connects and waits for its token to check out, or gives up first;
packets arriving while it waits are held, and once it is admitted the core
is asked about each. A host may connect again while its first link is still
up, as one does after its network changes. Admitted links send packets for
any host whenever the driver has no pair that carries them; an account's
plan lapses or is bought again, and each link learns of it when it presents
its next token; a token expires before a fresh one comes. Each link's
handshake lists the neighbors its host was told of, and a host forgets them
once it holds no link here. At any point the world may stop changing, and
the lapse's promise is judged there. The same machine with every link
admitted by a pinned key, all on one account, is a daemon forwarding
between its paired hosts; its links are the daemon's own network links to
them, dialled by either end, so both ends may dial at once, and a host's
process may restart, leaving the links of its earlier life to be found
dead. The properties read the driver's table of pairs,
built only from what the core told it, against the world's record of each
link, and are in `src/relay.rs`.

## The link socket

`SocketModel` runs the real `socket_core::Peers` of two hosts, a laptop
and a desk, each signed in to the relay and linked to the other directly,
with a cut-down QUIC above them: a dial's first flight, its answer, and
data on a link that is up, each carrying its connection's number so a
packet from a dead run's connection reaches a process that drops it. The
world delivers each datagram to wherever its address leads: the other
host, the relay (which forwards to the host behind the header while that
host's relay link is up and drops it otherwise), or nowhere. The laptop's
process may start afresh, on its port or another, leaving the desk's end
of the old link alive and stray packets of it in flight; it dials the desk
directly, on every path at once, or through the relay once the relay has
admitted its link; its relay link may first dial an address that never
answers, and may go and come back. Either host's path machine may probe
the address it knows the other at and move the link to a path that
answered. A datagram may be lost, which QUIC answers by sending the first
flight once more, or by giving up and dialling again; one may arrive
twice; the clock may pass the time unnamed senders are kept, while each
relay link keeps being heard from or while the laptop hears nothing from
the relay and its link stays up, as a phone suspended for minutes finds;
the local network may go. A link idles
out at either end once the other end is dead or nothing it sends can
arrive. Each fault is explored in a world of its own beside the relaunch,
and the world rests once no action is left, where the liveness promise is
judged. The properties are in `src/socket.rs`; each of the socket's fixes
for the relaunch failures of October 2026, undone one at a time, makes
one of them fail.

## The composed net

`ComposedNet` takes a topology (hosts, the links between them and the
agents each starts with) and runs it whole: every host's real daemon core
over its own in-memory store and its agents' journals, every agent's real
agent core around the test kind's interpreter with a scripted fake
provider, and the drivers' glue between them, so a call, its answer, a job
and its result are each a delivery waiting in a lane. Links come up only
on the cores' own dials: each host trusts its peers at an address of its
own in the net, a dial is answered with the link while the link is whole
and both ends run, and is silence otherwise until the link can carry it or
the core's deadline drops it. A severed link is heard gone only after a
second of link time, as a dead link's keepalives go unanswered, so its ends
redial at once rather than pausing as after a link that dropped as it came
up. Its initial state is the topology started with its agents created. Its actions are the
environment's and a person's: a lane's next delivery (or a later one, within
a bound on reorderings), a call's message lost, a link severed and restored
once both ends have heard it go, a daemon killed and started again or
rebooted uncleanly under its next generation, the clock moved on to the
next timer, a prompt sent to the agent and a stop asked of its host.

Its properties are the promises of the machines above that a net of two
hosts and one agent can break, each judged by the same predicate its own
model judges it by, over the same stores and cores:

- no core asks for a dump, and every store keeps every invariant its core
  checks after a write;
- the single daemon's commit before broadcast, a segment reclaimed only
  below the cursor, and the generation bumping after an unclean reboot and
  only after one;
- replication's four promises over the block, and a replica whose origin
  and link fall quiet becoming current;
- the lifecycle's exit only at a turn end or a drain deadline, and the exit
  cause on the journal's last step;
- presence from links: a host reads online exactly while a link of its
  own is up, and hosts on a working link read each other online once the
  world rests;
- an accepted prompt keeping its row on its host.

The environment judges the promises about a step as it makes the step, so
the simulated mode keeps them too. Beside them are the promises that hold
only between machines: a reset arriving while presence flips still leaves
the replica current under the newest generation, and a replica reads an
agent exited only once the agent's own host does, the order a far host's
family listing relies on (the net's one agent has no parent, so no family
acts on it here).

The rest are left to their own models, because this net cannot break or
observe them:

- the single daemon's "the open is one cut": no client subscribes in the
  net; its one subscription is the replica's, which replication's promises
  over the block judge;
- the single daemon's "a checkpoint plus the inputs since reproduces the
  state": the net keeps no ring of the inputs since a checkpoint, and
  replaying one at every state would double what a state costs;
- replication's "the store never refuses a write": the net's stores never
  refuse one;
- the lifecycle's "a resume continues where the last incarnation was": the
  net keeps no facts ring, since nothing in it reads one back;
- the lifecycle's "a one-shot child runs a queued follow-up before exiting"
  and "a message after the exit decision resumes": both need a parent and
  its one-shot child;
- presence's "a claimed host is dialled": each host reaches the other by
  one route, their link, with no relay or peer to claim another;
- every promise of the family model, which needs a parent and a child;
- every promise of the supervisor, which is outside the net.

Two nets are one when the cores' fingerprints, the stores, the journals,
the timers and what waits in each lane match, with the calls and jobs a
core numbered ranked as its fingerprint ranks them; the ranking is not
exact, so on several threads the count may differ by a few dozen states
between runs while the verdict does not. The checked topology is two
linked hosts and one agent whose one turn writes three items, explored
under three sets of faults. Walked one seeded delivery at a time, the same
model is the testnet's simulated mode. The properties are in
`src/net/mod.rs`, and the judging of a step in `src/net/env.rs`.

## Bounds

A model explores real stores, so a state costs a store transaction or two
rather than a few instructions. Each model runs in CI as a job of its own on
a four-core runner, under that job's time guard, and its CI bounds are sized
to fit it; bounds that would not fit move to the model's nightly test, which
runs daily in its own workflow and locally through `just models-nightly`.
Every run prints how many states it explored and how long it took. The
verdict does not depend on the number of threads, only which of several
counterexamples is printed.

A walk is reproducible: one seed walks one schedule on every run, and
another seed walks another (the switch's `one_seed_walks_one_schedule`).

Run the models with `just test-crate models`, one with
`just suite protocol-models <model>::` (each model is a module of the
crate's integration test binary), and the nightly bounds with
`just models-nightly`.
