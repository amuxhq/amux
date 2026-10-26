# Performance

*For developers measuring amux, changing a budget, or recording a baseline.*

Performance is a test of a specified workload on a reference machine. Each metric has an absolute budget and, for
some, a drift limit against a reviewed baseline. Because the numbers depend on the machine, performance runs in
the qualification lane on enrolled machines, never in an ordinary push. Claims that do not depend on the machine
(rows and bytes retained, message counts, cache misses) belong in ordinary tests; elapsed time and memory belong
here. How this lane sits beside the others is on [Testing](TESTING.md).

The desktop harness is the `qualification` crate (`crates/qualification/src/perf`); the phone has a suite of
its own, `just ios perf`, which drives the optimised app on the enrolled Mac's simulator (see
[The phone](#the-phone)).

## Working a budget

A budget is the requirement, and meeting it ends the work: a number that passes is not improved further, and a
number that misses is not argued down. Every change starts from a measurement that names the cause, from the
suite's own marks, the links' logs or a profile, never from a guess at where the time goes; a change made without
one is reverted if the number does not move. The first question of each fix is what work to remove, not what
machinery to add: a round trip, a handshake, a view built before anyone looks at it. A fix states its design in a
sentence, lands with a test or with the suite's number before and after, and is measured as what somebody holding
the phone sees, a frame or a reconciled fleet, not as a function's cost. When a profile points at nothing of ours
to remove, and the next change would only add machinery to work around the platform, the work stops there and the
budget is set again from the profile, with the reasoning recorded beside its number.

## Running it

```sh
just perf                        # every workload
just perf -- --only flood        # the flood alone
just perf --baseline             # record this machine's baseline after every budget passes
just perf -- --only flood --baseline
```

`just perf` builds the `perf` binary in release with the `bundled,perf` features and runs it; the binary refuses to
run from a debug build. The binary accepts nothing but `--baseline` and `--only flood`.

`just perf-profile 'System Trace' 15s` records the flood under Instruments on this Mac and stops it 15 s in, for
reading what the daemon's core thread does and waits on; `just perf-profile 'Time Profiler'` samples a whole run.
The trace and the run's report land in `target/perf-profile/`, and `xctrace export --xpath` turns a trace's
`context-switch`, `syscall` and `time-profile` tables into XML. Two things about Instruments shape the recipe: a
System Trace attributes to the process only the threads still alive when the recording stops, so a recording of
the flood phase must end inside it, and the record of the capacity phase (which ends the run) comes from a whole
run; and only the process `xctrace` launched itself is symbolicated, so the recipe launches `perf` directly rather
than through a shell. A run killed by the time limit leaves its agent processes behind; the recipe kills them.

For the whole run a child process keeps one core busy. An otherwise idle Apple Silicon machine lets its cores drop
into a low-power state between bursts and measures several times slower with wider spread; one busy core is the
repeatable reference state baselines are recorded in (`one busy core (cluster warmer)` in the report and the
baseline). It changes no workload, statistic or budget.

Wait for unrelated builds, simulator runs and other harnesses to finish before a qualifying run: the machine's load
is part of what is measured.

## The flood

The flood is the desktop workload (`crates/qualification/src/perf/flood.rs`). Twenty agents on one host (`desk`)
stream at full rate, and a second runtime (`phone`), linked to it and holding nothing yet, opens the fleet and one
chat. Full rate is the fastest a real provider streams, not the fastest a fake can write: every recording in the
Claude and Codex corpora peaks at 47 provider frames in one second, so each agent sends a message every 20 ms,
about a thousand frames a second on the host. The hosts run with a replica tail and catch-up cap K of 200, and
every agent's history is well past K before the viewer connects.

One run measures, in order:

| Metric | Statistic | Budget | Gate |
| --- | --- | --- | --- |
| `flood fleet caught up`: the viewer's fleet reaching CaughtUp | worst | 1,000 ms | budget |
| `flood chat caught up`: one chat's tail of K rows and snapshot while the rest catch up beside it | worst | 2,000 ms | budget |
| `flood ingest lag`: a journal write to its commit in the store | p99 | 250 ms | budget |
| `flood agent process memory`: each agent process's footprint | peak | 48 MiB | budget and drift |
| `flood catch-up under K`: the viewer cut off for K/2 rows, then restored, to CaughtUp | worst | 2,000 ms | budget |
| `flood catch-up over K`: the same for 5K rows, answered with a reset and a tail | worst | 2,000 ms | budget |
| `flood backlog growth with the daemon killed`: journal growth while the host's daemon is dead | worst | 1,200 MiB/min | budget |
| `flood backlog drain after restart`: every journal read to its end after the restart | worst | 5,000 ms | budget |
| `flood ingest cost per frame`: ingest draining a held backlog alone, per committed frame | median | 100 µs | budget and drift |

The last metric comes from a second phase: the agents write unthrottled while the host's store is held, so ingest
commits nothing until the journals hold a fixed backlog; then the writers pause and ingest drains the backlog alone,
priced over seven equal shares of the frames it commits. That isolates ingest's own cost from how busy writers share
the cores with it.

Memory is the platform's own measure: physical footprint on macOS, resident set size on Linux. The two are not
interchangeable and the report names which it used.

### The smoke

The same workload with three agents, a small K and short phases runs in the ordinary test lane so it keeps
working between qualifying runs. It checks that every metric is measured, not the budgets, which hold only for
release builds on an enrolled machine.

```sh
just test-crate qualification -- --test flood
cargo test -p qualification --features perf flood
```

The same test file holds the served flood to the measured one: `journeys/topologies/flood.json` must equal the
topology the perf lane builds, so `testnet serve journeys/topologies/flood.json` (see [TestNet](TESTNET.md)) hands a
real client the flood the lane measures. `FLOOD_TOPOLOGY_UPDATE=1` rewrites the file after the workload changes.

## Budgets and baselines

Budgets are constants beside the workload, in the `budgets` module of `crates/qualification/src/perf/flood.rs`,
each with the reasoning behind its number. The values they rest on (K, the tail N, the facts ring and journal
segment sizes) are on [Parameters](PARAMETERS.md). Changing a budget is a code change reviewed like any other.

A metric either is held to its budget alone ("ceiling only") or also to drift from the recorded baseline median.
Most flood timings are one observation, or sub-millisecond medians that vary several-fold between runs on the same
machine, so drift on them would fail runs for noise; their budget is what they must meet. Agent memory and ingest
cost per frame are stable enough to drift-check. Drift limits come from the unit: 15% for times and percentages,
10% for bytes, MiB and ratios.

Baselines are committed per enrolled machine:

```text
perf/baselines/
  desktop/<hw.model>.json      e.g. desktop/Mac14,6.json
```

A desktop baseline records its schema version, machine model, profile (`release`), features (`bundled,perf`),
reference state, and the median of every metric, `null` for ceiling-only ones. The enrolled machines are listed in
`crates/qualification/src/perf/report.rs`: at present one, `pinned-mac`, model `Mac14,6`. A run on a machine not in
the list is refused, and so is a baseline recorded on another model, profile, feature set or reference state, or
one whose metrics differ from the run's. With no baseline on disk, drift is reported as unavailable and the run is
judged on budgets alone.

`--baseline` records the whole workload it ran. It still enforces every absolute budget, and a run outside a budget
cannot become a baseline: a miss is a defect to explain, never a new value to adopt. Review the complete report
before committing the file.

A baseline is only comparable with a run that measures the same way. Whenever a metric's measurement changes,
re-record that metric's baseline in the same commit as the change. `--baseline` rewrites every median, so when only
one measurement changed, keep the other metrics' committed values and commit only the changed one. The ingest cost
per frame moved from 21.0 to 23.8 µs when its measurement changed to a fixed backlog priced over shares (b972f117),
and the new figure was adopted without building the old commit under the new measurement to tell the two apart: the
difference is under 3 ms of daemon time per thousand frames, which no one feels, and the drift check guards the
figure from here. A regression hiding inside that move would have to be found by profiling, not by the baseline.

A baseline, or a run that qualifies a change, counts only when the machine is shown to be in the reference state,
never assumed. Show it in the same session, with the machine state (`uptime`, `ps -Ao pcpu,comm -r | head`,
`df -h`) recorded beside every run:

- **Control.** Build the commit that recorded the current baseline (`git archive <sha>` into a scratch tree with its
  own target directory) and run its `perf --only flood` three times before and three times after the runs that
  count. Both medians must reproduce its recorded value within 5%. One reading is not a control: runs of one build
  differ by several percent.
- **Quiet.** Before each run, no process holds more than a fifth of a core for a minute. After every flood run the
  file-events daemon works through the run's writes for minutes; wait it out.
- **Free space.** Ample free space on the volume the run writes to (the temporary directory's). Ingest cost is file
  writes, and a nearly full APFS volume inflates it: with 5 GiB free on a 926 GiB disk the baseline commit read 8
  to 25% above its own recorded value; with 176 GiB free it reproduced it.
- **Agreement.** Run the build being judged at least three times; the recorded run must fall within 5% of the
  median of that session's runs of the same build. A reading outside that is noise, not a baseline.

## Reading a report

The report names the machine and OS, profile and features and the reference state, then one line per metric:

```text
metric | median | measured | budget | baseline | drift | verdict
flood ingest cost per frame | 21.002 µs | median 21.002 µs | 100.000 µs | 21.002 µs | +0.0% | PASS
  workload=… · seed=0 · identities=… · samples=7 · warm-up=… · start=… · end=…
```

`measured` is the metric's named statistic over its samples; `baseline` and `drift` read `ceiling only` for metrics
without a drift gate. The run fails if any verdict is `FAIL`.

## The relay curve

`just relay-curve` measures how the production relay scales with the hosts holding links to it
(`crates/qualification/src/perf/relay.rs`). At 10, 100 and 1,000 concurrent connections it starts a fresh relay on
loopback, links that many hosts to it in pairs, one pair to an account, then links each pair end to end through the
relay, one host dialling the other through its own link to the relay as a daemon does, and measures three things:
link admissions per second, the level's hosts over the time from the first dial to the last link the relay accepted,
sixty-four hosts dialling at once; stream-open latency, every host opening twenty streams to its partner on their
link one after another, all hosts at once, each timed from the open to the partner's one-byte answer; and aggregate
throughput, every host writing into one stream on that link as fast as the link takes the bytes, read over a
three-second window after a second's warm-up. Each level runs five rounds, each waiting a quarter second after the relay has let the last round's links go,
since their teardown otherwise delays the next round's dials: admissions and throughput are the median round's, and
the latency percentiles are taken over every round's opens together, so the 99th percentile at ten connections rests
on a thousand opens.

```sh
just relay-curve              # measure and print
just relay-curve --baseline   # record perf/relay/<hw.model>/baseline.json
just relay-curve --compare    # judge against it; non-zero outside the noise bands
just relay-curve --udp        # the same over QUIC on UDP; its baseline is perf/relay/<hw.model>/udp/baseline.json
```

By default the hosts link over the relay's TLS-over-TCP listener, the one a deployment runs for hosts whose UDP does
not get through. With `--udp` they link over QUIC on UDP, as a deployment's hosts normally do: each host has a socket
and relay tickets of its own and answers the relay's Retry, and the relay's limit of ten new QUIC handshakes a minute
from one address is lifted for the benchmark alone, since every host here dials from the same loopback address. The
links between partners are QUIC whose packets the relay forwards over those legs. The hosts are bare: a link to the
relay, a link to the partner and raw streams on it, serving no calls, so the work measured is the relay's forwarding,
the partners' QUIC and the carrier's, in the same process with the relay on a runtime of its own. The run records the machine model, OS, cores,
reference state (the flood's busy core) and the load average before and after; the binary refuses a debug build
and a machine that is not enrolled.

The comparison is one-sided: a run passes unless a figure is worse than its baseline by more than its band, and a
faster run is reported as such. The bands are constants beside the workload, set from three consecutive runs of one
build on the enrolled Mac: throughput moved by up to 4% and the median open by up to 2%, so their bands are 15% and
25%; the 99th percentile moved by up to 76% and at a thousand connections falls in one of two modes, so it fails only
past double. Admissions moved by up to 10% at every level over three runs with the Mac busy elsewhere, so their band
is 25%, like the median open's; more admissions per second is better. A baseline from another model, profile, carrier
or reference state, or one recorded before admissions were measured, is refused.

Before the relay forwarded links' packets it carried the hosts' streams itself, and the baseline on the enrolled Mac
was recorded then: about 190 MiB/s from 10 connections to 100 and 160 at a thousand, with the median open at 3, 29
and 294 ms. Forwarded, an open is one round trip between the partners rather than a stream the relay sets up, so the
median open fell to 0.8, 7 and 69 ms. Throughput first fell to 46, 77 and 39 MiB/s, because every 1,200-byte packet
was its own write, a TLS record and a system call on each TCP leg, and the queues behind those writes overflowed on
QUIC's bursts; with each byte-stream path gathering its frames into 64 KiB writes it rose to about 450, 270 and
275 MiB/s, above the carried baseline at every level, with admissions and opens unchanged. A change to the relay shows
when it adds round trips, packets or system calls to a link, or serialises work across links.

Over QUIC on UDP the same benchmark, applied to the starting commit as well, read 32, 29 and 21 MiB/s at 10, 100 and
1,000 connections with the relay carrying streams, admitting 1,491, 1,934 and 2,001 links a second, the median open
at 2.6, 26 and 280 ms and the 99th percentile at a thousand 1.1 s. Forwarded, it carries 42, 41 and 39 MiB/s, admits
1,854, 2,643 and 3,207 links a second, and the median open falls to 1.6, 11 and 111 ms, the 99th percentile at a
thousand to 139 ms. Throughput over UDP stays well under TCP's at every level: each forwarded 1,200-byte datagram
crosses the relay's one UDP socket in and out with a system call of its own, and macOS offers no segmentation
offload, where each TCP leg is a stream of its own written in 64 KiB batches.

## The phone

`just ios perf` measures the iPhone app itself, on this Mac's pinned simulator, against a served network built for
it: three machines on the phone's local network, signed in to one account at a served relay, forty agents dealt
across them, and one conversation a thousand rows long that streams on cue (`crates/qualification/src/perf/phone.rs`
generates the topology; `testnet journey perf` runs the suite). The app is the `Measured` build: compiled the way
the shipped build is, with the driving door kept in, because an unoptimised Swift build measures the compiler
rather than the app. Every packet between the phone and a machine crosses a gate the run can delay (the served
net's `LanGate` and `LanFaults` verbs), so reaching the fleet is measured over a household network's latency as
well as over none; and once more with the phone away from home, signed in to the relay with every direct dial lost,
which is the path mobile data takes. Away from home every way to the relay is gated: the phone dials a gate in front
of the relay's QUIC carrier (`RelayGate`), each machine already reaches the relay through a gate of its own, and the
net's `NetFaults` verb aimed at `relay` delays them all at once, so a relayed round trip pays the latency on both
legs. The account service is not gated, since a launch reuses the phone's relay token rather than fetching one.

Every number is taken by the app: the launch marks (`Signposts` in `AmuxCore`, emitted in every build) and the
door's `measure` verb, which watches the display, the main thread and the footprint for a stretch while the run
streams into the chat on screen. The script only arranges the workload and judges. Five samples per metric, the
median against the budget; the run stops and says so if a mark it needs never appears or a stream never reached
the phone, rather than reporting a number about an idle screen.

| Metric | Group | What is measured | Budget | Worst | Tolerance |
| --- | --- | --- | --- | --- | --- |
| `cold first frame` | cold | Kernel process start to the first presented frame carrying the remembered fleet's rows, the app terminated between the five launches, the machines up | 850 ms | 900 ms | 15% |
| `cold store read` | cold | Inside each of those launches, the store's own share: the embedded node's start (opening every profile's store, the listener, the identity) plus the profile on screen opening its fleet from the store and catching up with it, before any host is reached; the main thread's wait between the two is not counted | 50 ms | | 15% |
| `cold fleet render` | cold | From the fleet on screen having caught up with its store to the first presented frame carrying its rows: building the home from what the store held, and the rest of the shell's first frame around it | 250 ms | | 15% |
| `reconciliation at 0 ms` | reconciliation | From that point to the first presented frame after every trusted machine's agents were current with the machine, over loopback | 250 ms | | 15% |
| `reconciliation at 100 ms` | reconciliation | The same, with every gate holding each packet 100 ms | 336 ms | | 15% |
| `reconciliation through the relay at 100 ms` | reconciliation | The same, the phone away from home: signed in to the machines' account at the served relay, every direct dial lost, and every way to the relay, the phone's and each machine's, holding each packet 100 ms | 566 ms | | 15% |
| `opening hitch time` | opening | Missed frame time per second from just before a tap on the working agent's row on home until 2.5 s later: the chat's push, its transcript's first build and its activity line starting. The push is the render server's; what this counts is the main thread's stall building the page, during which nothing it animates moves | 120 ms/s | | ceiling only |
| `opening main-thread CPU` | opening | The main thread's share of one core over the same 2.5 s | 20 % | | 15% |
| `streaming hitch time` | streaming | Missed frame time per second while fifty rows a second arrive for twenty seconds into the conversation on screen, resting at its tail | 5 ms/s | | ceiling only |
| `streaming main-thread CPU` | streaming | The main thread's share of one core over the same stream | 60 % | | 15% |
| `streaming footprint` | streaming | The process's footprint at the end of the stream | 120 MB | | 10% |
| `idle transcript commits` | idle | Rows the chat on screen took over five seconds with nothing arriving, after a two-second settle | 0 count | 0 | 0% |
| `idle display ticks` | idle | Display refreshes the app asked for over the same five seconds | 0 count | 0 | 0% |
| `idle main-thread CPU` | idle | The main thread's share of one core over the same five seconds, the agent on screen working with nothing arriving: what a never-ending animation costs, which neither count sees | 8 % | | ceiling only |
| `idle home main-thread CPU` | idle | The same on home, after the launch has reconciled and before the conversation opens, the fleet's agents working | 8 % | | ceiling only |

The cold-start budgets are a simulator's: an empty SwiftUI app linking the frameworks this one links draws its first
frame at about 414 ms on the pinned simulator, so the gate is the floor plus room for the app's own work, and the
400 ms a phone is asked for stays on the physical-phone checklist in [the iPhone app](IOS.md). The simulator reports
60 Hz and composites through the Mac's display, so hitch time is display-link missed-frame accounting, a proxy for a
device's hitch metric. The store read was first measured at about 270 ms on the pinned Mac, of which the node's own
start was 25 ms and the fleet's open one; the rest was the main thread, busy building the shell, taking its time to
hear that the node was up, which the two intervals now leave out. The fleet-render budget is what the main thread's
work between the store read and the first frame should take: the home built from the store's rows, and the rest of
the shell's first frame, which waits for the same thread.

Where a cold launch goes, from the marks (the run prints them per launch): about 300 ms before
`main` is reached, loading the images; about 100 ms of UIKit building the scene before the app's composition is
asked for, which an empty app pays too; the composition itself in 2 ms; then the main thread building the shell's
first frame, which is where the app's own time is. The shell builds the tab somebody is looking at, and a tab is
built when first reached for and kept from then; the home's rows are built as they come near the screen. Those two
took the shell from about 430 ms to about 230 ms on the pinned Mac, of which the home with forty remembered rows is
about 80. What is left is a navigation stack, a tab bar and a home being built by SwiftUI for the first time, with
no one item a profile points at. An Instruments profile of the launch on the pinned Mac puts the main thread's
heavy leaves in the loader (270 ms: symbol comparisons, load-command walks, and 85 ms of dyld_sim re-pointing the
shared cache's exports at the host, which a device's loader does not do) and in the Swift runtime's conformance and
metadata work, with no function of ours among them; the budget on the simulator was 650 ms median and 700 worst,
set from that profile. The home's polish put it under the system's chrome (a tab bar, a navigation bar with a large
title and buttons, a search field, filter chips and callout cards) and redrew its rows, which took the first frame
from about 640 ms to between 740 and 785 across runs on the pinned Mac. Taking those four pieces of chrome out
together brings it back to about 680: roughly 60 ms is the system's chrome, about 30 of it before the fleet opens
while the main thread builds the tab bar, and 40 is the redrawn list. No one piece is larger than the 40 ms by which
two runs of the same build differ. The budget is now 850 ms median and 900 worst, and the fleet render's 250: that
cost, with room for the 40 to 60 ms by which runs of one build move on this Mac. A phone has budgets of its own
(below).

Where reconciliation goes, over a gate that holds each packet 100 ms (a 200 ms round trip): one round trip. A
launch resumes each host's link with the TLS ticket and address-validation token its last process kept, so no Retry
and no full handshake intervene, and its first flight carries the link's Hello, the inventory subscription and every
remembered session's subscription as early data, on every remembered path and through the relay at once; the host
answers with the catch-up beside its handshake. The phone's link log times each host's catch-up 213 to 219 ms after
the link's first packet, and the reconciliation mark reads a 162 ms median, since the first flight leaves while the
store is still being read. Before, a launch took four round trips, about 760 ms (797 ms in the run this work started
from): two for the QUIC handshake, since a listener answers an address it has not seen with a Retry; one for the
link's Hello and HelloAck; and one for the inventory. A stream to a paired host is plain, with no handshake inside and
no wait to be accepted, and one Session channel per host carries every agent's subscription. The test gate itself
passes datagrams in the order they came: held each on its own timer, two sent in the same millisecond could swap
places, and a packet overtaking the handshake it followed was dropped and counted lost, costing a round trip a
household network never does.

Through the relay, with the gate holding both the phone's leg and each machine's (a relayed round trip, 400 ms,
crosses both), the pinned Mac measured a 355 ms median against 1,608 ms before, each machine's catch-up logged 420
to 427 ms after the first packet. Both launch budgets are the ungated reconciliation (91.8 ms) plus one gated round
trip, with 15% over that: 336 ms at home and 566 ms through the relay.

A launch leaves its successor a ticket only once the host sends one, when the phone's handshake finishes: two round
trips into a resumed link, one after the catch-up. The run keeps each launch open until the next is due, ten
seconds apart, as a person looking at their fleet does; quit as soon as the fleet reconciled, every other launch
found no ticket and paid a full handshake (839 ms through the relay).

`--only cold|reconciliation|opening|streaming|idle` takes one group, for working on it; `--baseline` needs a whole run, and
judges it on the budgets alone, since the baseline it replaces is no reference for its own successor.
`--trace <path>.trace` records an Instruments trace through the first sample of one group (`--during`, streaming
unless named; `--template`, Time Profiler unless named), for finding where the main thread's time goes. On the
simulator it is a recording of the whole Mac (xctrace cannot attach to a simulator process here), with the app's pid
in the run's notes to filter by; every table the trace lists is exported beside it as XML, so read the app's process
alone.
`--describe` says which enrolled machine this is and whether its baseline exists without building or launching
anything. Baselines live beside the desktop's:

```text
perf/baselines/phone/<machine>.json      e.g. phone/pinned-mac.json
```

with the same drift rules: the median may grow past the recorded one by the tolerance in the table and no more;
a metric with no baseline is judged on its budget alone and the report says so. The machines:

| Machine | Model |
| --- | --- |
| `pinned-mac` | `Mac14,6` |

A Mac not in the table is refused, not skipped: the suite's own tests hold this page's tables to
`crates/testnet/src/journey/perf.rs`, so enrolling a machine or changing a budget is a change here and there in one commit. The report lands in
`target/ios/perf/report.md` with `verdict.json` and `samples.json` beside it; the served net's own record is under
`target/ios/perf/journey/`. `just ios verify` runs the suite last, as `just ios measured`; the hosted runners are not
enrolled, so it is not in `just ios captures`.

The flood's viewer (above) still reads the fleet and one chat through the same subscriptions the app reads, from a
runtime holding nothing, so its budgets say what the phone waits for on the host's side, without a simulator in the
number; the phone suite says what the app does with it.

### On a plugged-in phone

`just ios perf --device` runs the same suite on an iPhone plugged in over USB: one phone, paired with this Mac, in
Developer Mode, unlocked, with Auto-Lock off for the run, and the signing file `apps/apple/Signing.local.xcconfig`
naming the team the build is signed under. The recipe builds the bridge's phone slice and the `Measured` app for it;
`devicectl` installs, launches and stops the app. The phone and the Mac meet over the phone's USB link, a
point-to-point IPv6 network CoreDevice brings up for a paired phone: the app's door and its LAN socket bind the
phone's address there, and the served net takes `--client-ip` for the Mac's, where the hosts' gates, the relay's gate
and a second listener for the account service listen (a Mac cannot reach its own address on that link, so the
hosts keep loopback). Nothing listens on Wi-Fi. A run installs over the app already there and launches it with
`-amux-fresh-start`, which forgets the app's files, caches and settings as an uninstall would but keeps the local
network permission, so the system never asks for it with nobody there to answer. Before measuring, the door's
`condition` verb reports the thermal state and Low Power Mode; a phone that is seriously hot or saving power is
refused, and a fair thermal state is noted in the report.

The numbers are the app's own, as on the simulator, but the display runs at the phone's rate: the suite's frame
counter asks for 120 Hz, as the app does, so hitch time counts ProMotion frames. The report sets the cold first frame
against the 400 ms a phone is asked for at launch, as a target rather than a budget. A phone's baseline is kept per
model and iOS major version, since an update changes the loader and the frameworks under the app:

```text
perf/baselines/phone/<phone>-ios<major>.json      e.g. phone/iphone-14-pro-ios27.json
```

`just ios profile GROUP [TEMPLATE]` records one group's first sample on the phone under Instruments (Time Profiler
unless named; Power Profiler, Hitches, SwiftUI and System Trace are as useful) into `target/ios/traces/`. On a phone
xctrace attaches to the app, so the recording carries its symbols; a launch is recorded by having Instruments launch
the app itself, the one way a launch's trace is symbolicated, which moves the launch's own marks by about a second.

| Phone | Model |
| --- | --- |
| `iphone-14-pro` | `iPhone15,2` |

Each phone's budgets are its first measurement with the simulator's headroom over it: the cold first frame with the
simulator's room in milliseconds, since a share of 200 ms is inside one launch's spread; the reconciliations by the
simulator's rule, the ungated launch plus the gated round trip with 15% over; the rest the simulator's budget over
its baseline as a share. Units, groups and tolerances are the simulator's.

| Metric | Phone | Budget | Worst |
| --- | --- | --- | --- |
| `cold first frame` | `iphone-14-pro` | 260 ms | 310 ms |
| `cold store read` | `iphone-14-pro` | 50 ms | |
| `cold fleet render` | `iphone-14-pro` | 60 ms | |
| `reconciliation at 0 ms` | `iphone-14-pro` | 60 ms | |
| `reconciliation at 100 ms` | `iphone-14-pro` | 283 ms | |
| `reconciliation through the relay at 100 ms` | `iphone-14-pro` | 513 ms | |
| `opening hitch time` | `iphone-14-pro` | 120 ms/s | |
| `opening main-thread CPU` | `iphone-14-pro` | 25 % | |
| `streaming hitch time` | `iphone-14-pro` | 5 ms/s | |
| `streaming main-thread CPU` | `iphone-14-pro` | 60 % | |
| `streaming footprint` | `iphone-14-pro` | 100 MB | |
| `idle transcript commits` | `iphone-14-pro` | 0 count | 0 |
| `idle display ticks` | `iphone-14-pro` | 0 count | 0 |
| `idle main-thread CPU` | `iphone-14-pro` | 8 % | |
| `idle home main-thread CPU` | `iphone-14-pro` | 8 % | |

On the iPhone 14 Pro (iOS 27.0) the first frame comes about 200 ms after the process starts, against 790 on the
pinned Mac's simulator: the device's loader does not re-point a shared cache, and most of what remains is UIKit's
scene and SwiftUI's first render, with dressing the navigation bars (about 15 ms), registering the bundled fonts
(about 12) and the shell's and home's bodies (about 22) the largest pieces of ours. The node's start is not on the
first frame's path; its first flight to every host leaves 2 ms after it listens, and the app's composition (and so
the node) is built as the app finishes launching rather than when the root view first asks for it. A working agent's
marks and the chat's activity line were redrawn by SwiftUI on every display frame, at 120 Hz, re-laying out the
composer around the line: a working chat with nothing arriving held the main thread near 31% busy, and home with a
working fleet near 32%. Drawn into a canvas on a clock capped at 60 Hz, with the line's elapsed time on a clock of
its own that ticks once a second, they halved (about 16.5%), the rest being SwiftUI's per-frame update of a view
hierarchy with one animation in it. Run by Core Animation from the same curves (the `workingMotion` leaf in
[the iPhone app](IOS.md)), they leave the main thread at about 2% in a working chat and 4% on home.
