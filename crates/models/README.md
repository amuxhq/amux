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
at 1.3 GB. `MODELS_SEARCH=breadth` (or `Bounds::breadth_first`) explores breadth
first, to print the shortest path to a failure depth first found; a bounded
depth always does, since depth first would skip a state first met past the
bound. `walk(model, bounds, seed)` follows
one schedule chosen by the seed: the same seed walks the same schedule on every
run, so a failing walk is reproduced by its seed alone.

## Faults

What the world may do to a machine between its own actions is one
vocabulary, `Fault`, shared by every model's environment and by the
simulator: a message in flight lost, delayed or reordered, a host crashing,
rebooting uncleanly or losing power, and the clock moving on. Each model
offers the faults its slice of the world can suffer and says how each plays
out on it; nothing else is simulated.

## Parked properties

When the checker finds a property failing and what to do about it is a
decision still waiting, the property is parked rather than weakened: it
keeps its sentence and predicate, and `.parked(Parked { since,
counterexample, reason })` records when, the path that broke it and the
decision it waits on. The checker reports a parked property with its reason
instead of exploring it, so `just test` stays green meanwhile, and
`just parked-check` lists every parked property and fails while any
remains, so nothing lands with one.

## Naming a property elsewhere

`cargo run -p models --bin properties` lists every property of every model
as JSON lines (machine, kind, sentence), each once. A `model` contract in
`tests/contracts.toml` names a property by its machine and sentence, and
`just contracts-check` fails when that list does not hold it, so rewording a
sentence means updating its contract.

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
each on a smaller world so that it fits its budget. In the first, one of
the link's packets arrives, on a path the machine sends on or on a byte
stream it has not heard on, and a paired daemon's forwarding link comes up
and goes, while the network stays put, the relay link stays up and the
local address keeps answering. In the second, an answer arrives for a path
with no probe of it outstanding (late, duplicated, or from before the
network changed; never on a path whose probe the machine still waits on,
since the driver passes only the outstanding probe's answer), the relay
refuses for no route, the rate limit or shutting down, the peer stops
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
relay link keeps being heard from; the local network may go. A link idles
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
and its result are each a delivery waiting in a lane. Its initial state is
the topology started with its agents created. Its actions are the
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

## Cost, bounds and budget

A model explores real stores, so a state costs a store transaction or two
rather than a few instructions, and its bounds are sized so a run fits the
CI runner's time. Every model's test prints its measured cost as `states`,
`seconds` and `states_per_second`. Every test asserts its budget on the
state count, which the bounds decide and the machine does not: the count at
the CI bounds with two percent over, since for some models it moves with the
threads (the family model's by about 1.5%). A run past its count says the bounds have
outgrown their place; a run that hangs is the test harness's bound to
catch, not a verdict the model gives.

| Model | CI bounds | States | 12 threads | 4 threads | Four-core runner | Budget | Nightly bounds | Nightly, 12 threads |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| Replication | 2 keys, cap 1, revisions up to 4, 2 generations, 2 in flight, 1 loss in a row, the opening replayed once | 157,789 | not measured | 50 s | none yet at these bounds (about 233 s projected); at 81,268 states, before the resumed link: 87, 102, 72, 103, 50 and 101 s | 480 s | 3 keys, cap 2, revisions up to 5, 3 in flight | 1,062 s (6,424,651 states, load 45 falling to 16) |
| Single daemon, and its peer | 4 frames, 2 checkpoints, 1 crash, power loss or reboot, 1 subscription, 1 lag; the peer: 2 frames, 1 checkpoint, 1 crash, power loss or reboot, 1 resumed link, 1 replay, its pin removed | 207,594 and 50,633 | 51 s and 16 s | 74 s and 21 s | 81, 161, 161, 77, 113 and 135 s for the journal; none yet for the peer (about 100 s projected) | 540 s for both | 2 crashes, power losses or reboots, 1 refused commit; the peer around the CI bounds' journal and subscriber | 557 s (2,018,569 states), load about 30; the peer past 4,000 s (not finished at 12 threads, load about 55) |
| Family and deliveries | 1 turn end, 1 link down, 1 crash, 1 wait | 219,380 | 23 s | 47 s | 178 and 128 s (about 191 s projected); at 396,826 states with the parent's spontaneous exit: 161, 299, 222, 313, 310 and 155 s (the first at 391,552 states, before the first listing predated the parent) | 420 s | the parent's and the child's spontaneous exits, 2 links down, 2 waits | 757 s |
| Presence and routing | 4 links or claims up, 3 down, 3 failed dials or refusals for no route, 2 plan changes, each heard by this daemon after the relay enforces it, 2 refusals for the plan in a row | 50,510 | 0.2 s | 0.2 s | none yet | 30 s | 5 up, 4 down, 3 plan changes | 0.4 s (216,602 states) |
| Agent lifecycle, one-shot child and parentless agent | 3 daemon connections, 3 inputs, 2 messages, 2 asks, 1 unasked provider exit, 2 stops or kills | 1,296,718 and 71,250 | 9 s | 18 s | 45, 28, 44, 29, 41 and 45 s for both | 120 s each | 4 connections, 3 messages, 2 unasked exits | 81 s |
| Supervisor update, both platforms' swaps | 2 releases, 2 supervisor crashes, 2 daemon crashes, 1 failed start, 5 timers firing, 1 `amux update` | 5,819,060 and 4,850,638 | 7 s | 8 s | 17, 27, 25, 27, 27 and 28 s for both | 60 s each | 3 releases, 2 failed starts, 6 timers firing | 124 s |
| One open chat | 2 writes, 1 lag, 1 reset, 2 pages over a three-item opening, a tail of 2, pages of 1 | 1,217,819 | 10 s | 24 s | 51, 109, 116, 105, 105 and 107 s | 240 s | 3 writes | 343 s |
| Path machine, in three worlds | 2 of the 3 addresses learned, 1 lost probe, 1 network change, 1 plan change, 2 ticks, 1 relay link drop, 1 silence of the local address; the same with no network change, relay link drop or silence, and 1 of the peer's packets arriving, a byte stream and 1 paired daemon's forwarding link; the first with no relay link drop or silence, and 1 answer with no probe outstanding, 1 refusal for another reason than the plan, 1 address forgotten and an IPv6 address | 12,929,251, 11,118,688 and 12,701,530 | 15 s, 14 s and 17 s | 30 s, 28 s and 34 s | none yet | 300 s, 300 s and 360 s | the first with all 3 addresses, 2 plan changes and 3 ticks; the second with a network change; the third with a relay link drop | the first not re-measured since it was 90,308,562 states in 405 s; the second 60,146,859 states in 89 s and the third 59,820,884 in 106 s (load about 13, about 19 GB each) |
| Relay's forwarding, as the relay and as a daemon | 3 links connecting, 3 packets asked about, 1 plan change, 1 expiry; the daemon's links dialled by either end, and 1 restart of a host's process | 694,420 and 719,032 | 4 s and 4 s | 3 s and 3 s | none yet | 60 s each | the relay with 4 links; the daemon with 4 links | 41 s; the daemon's 18,625,784 states in 43 s (load about 13) |
| Link socket, in six worlds | the laptop starting again once on its port or another, 1 probe from each host, 1 send on each link; the local network going; then on the laptop's port, 1 loss; 1 duplicate; the local network and then the relay link going; 1 tick past the unnamed senders' time | 3,531,706, 51, 248,321, 104,781, 38,389 and 87,607 | 7 s for the first, under 1 s for each other | 15 s for the first, under 2 s for each other | none yet | 60 s for all six | the relaunch on its port or another with 1 loss and 1 send on each link | 230 s (93,186,069 states, load about 12) |
| Composed net: a reset while presence flips, lost messages, a person's calls through a crash | 1 sever and 1 unclean reboot; 1 reordering, 1 lost message and 1 advance; 1 kill, 1 prompt and 1 stop | 33,025, 9,559 and 216,320 | 27 s | 51 s | 151, 105, 204, 207, 139 and 180 s for all three | 420 s for all three | every fault but a kill, with 1 reordering; the calls through a crash with 1 lost message | 184 s |

Measured on 2026-10-18 and 2026-10-19 on the enrolled Mac (Mac14,6, 12
cores), the family model's on 2026-10-20, the relay's forwarding on
2026-10-21 and the path machine's and the relay daemon's on 2026-10-22 (the
relay's first with the Mac busy, load about 20, the path machine's at load
about 5 to 13;
presence's again that day at load about 15, and presence, the single
daemon's peer and replication again that night at load about 8, and the
nightly bounds of presence, replication and the single daemon the next
morning at load 16 to 56; the link socket's on 2026-10-29 at load about 20), with the
development profile,
which optimises SQLite, the store and the models; `MODELS_THREADS=4` gives
the four-thread figure. The checker runs on every core the machine has; the
verdict and the state count do not depend on the number of threads (the
family model's and replication's counts aside, below), only which of several
counterexamples is printed.

The four-core runner column is each model's test binary's time at its CI
bounds on CI's four-core Linux runner, measured by six CI runs
of nearly the same tree, in October 2026; a binary holding two machines is timed
for both. The runner's speed varies between runs by up to two and a half
times: presence took 288 s in one run and 120 s in the next, the composed
net 105 s in one and 204 s in the next.
The family model's first two figures are from two earlier runs;
its six runs at the bounds that included the parent's spontaneous exit
spread twofold, and two passed 300 s, so the parent's spontaneous exit
moved to the nightly bounds; the cut bounds took 0.61 of the time on the
enrolled Mac, back to back, and the budget is sized to the slowest run
scaled by that; the first two CI runs at the cut bounds took 178 s and 128 s. Its state count varies by under 1% from run to run
(218,332 to 219,506 at these bounds), so one of its values is not yet
printed the same way for the same state. Replication's count varies by a
few dozen states on several threads (157,539 to 157,789 at these bounds) and
not on one: a state keeps the cores' checkpoints the first path to reach it
left, and which path is first depends on the threads. Presence's earlier figure
is at bounds that allowed two refusals for the plan in a row: twice it passed
600 s, so the CI bounds allow one and the second moved to the nightly bounds.
Once the relay refused the host's packets and the plan reached this daemon
after the relay, presence at two failures projected past the cap before any
runner timed it, so the CI bounds allow one failure and the second is the
nightly bounds' (which allow three). Presence's nightly bounds keep the CI
bounds' links up and down: with a fourth up and a third down they explored
30,391,426 states in 1,170 s on the Mac's twelve threads at load about 50,
which projected past the nightly job's then 100 minutes on the runner, so the fourth
up and the third down were cut. The nightly job's runner is projected from a
twelve-thread Mac time by the slowest ratio of four threads to twelve in the
table (2.4) and the runner's 4.7 on four: presence's 140 s comes to about
1,600 s.
The time budgets in the table size the bounds, not the tests.

Each model runs in CI as a job of its own on that runner, and its bounds are
sized to it: **a model's time budget is at least twice its measured time on
the four-core runner at its CI bounds, rounded up to the next full minute,
and no budget is above 600 s.** A model with no runner figure yet is sized from its
four-thread Mac time times 4.7, the slowest runner-to-Mac ratio measured (the
runner took 2.1 to 4.7 times the Mac's four-thread time; presence, at 339.9 s
against 73 s on four of the Mac's threads at the same bounds, was the
slowest), doubled and rounded up the same way, until a CI run measures it. A model whose doubled runner time would pass
600 s has its CI bounds cut, and the bounds it loses move to its nightly
test. The nightly bounds run daily in their own workflow, each model in a job
of its own, and locally through `just models-nightly`.

A walk is reproducible: one seed walks one schedule on every run, and
another seed walks another (`one_seed_walks_one_replication_schedule`).

Run the models with `just test-crate models`, one with
`just test-crate models -- --test <target>`, and the nightly bounds with
`just models-nightly`.
