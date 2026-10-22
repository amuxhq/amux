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
```

The hosts link over the relay's TLS-over-TCP listener, the one a deployment runs beside QUIC: the relay admits ten
new QUIC handshakes a minute from one address, and every host here dials from the same loopback address. The links
between partners are QUIC whose packets the relay forwards over those TCP legs. The hosts are bare: a link to the
relay, a link to the partner and raw streams on it, serving no calls, so the work measured is the relay's forwarding,
the partners' QUIC and the TLS on the relay's legs, in the same process with the relay on a runtime of its own. The run records the machine model, OS, cores,
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

## The phone

`just ios perf` measures the iPhone app itself, on this Mac's pinned simulator, against a served network built for
it: three machines on the phone's local network, signed in to one account at a served relay, forty agents dealt
across them, and one conversation a thousand rows long that streams on cue (`crates/qualification/src/perf/phone.rs`
generates the topology; `scripts/ios-perf.py` runs the suite). The app is the `Measured` build: compiled the way
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
| `cold first frame` | cold | Kernel process start to the first presented frame carrying the remembered fleet's rows, the app terminated between the five launches, the machines up | 650 ms | 700 ms | 15% |
| `cold store read` | cold | Inside each of those launches, the store's own share: the embedded node's start (opening every profile's store, the listener, the identity) plus the profile on screen opening its fleet from the store and catching up with it, before any host is reached; the main thread's wait between the two is not counted | 50 ms | | 15% |
| `cold fleet render` | cold | From the fleet on screen having caught up with its store to the first presented frame carrying its rows: building the home from what the store held | 100 ms | | 15% |
| `reconciliation at 0 ms` | reconciliation | From that point to the first presented frame after every trusted machine's agents were current with the machine, over loopback | 250 ms | | 15% |
| `reconciliation at 100 ms` | reconciliation | The same, with every gate holding each packet 100 ms | 1,000 ms | | 15% |
| `reconciliation through the relay at 100 ms` | reconciliation | The same, the phone away from home: signed in to the machines' account at the served relay, every direct dial lost, and every way to the relay, the phone's and each machine's, holding each packet 100 ms | 2,000 ms | | 15% |
| `streaming hitch time` | streaming | Missed frame time per second while fifty rows a second arrive for twenty seconds into the conversation on screen, resting at its tail | 5 ms/s | | ceiling only |
| `streaming main-thread CPU` | streaming | The main thread's share of one core over the same stream | 60 % | | 15% |
| `streaming footprint` | streaming | The process's footprint at the end of the stream | 120 MB | | 10% |
| `idle transcript commits` | idle | Rows the chat on screen took over five seconds with nothing arriving, after a two-second settle | 0 count | 0 | 0% |
| `idle display ticks` | idle | Display refreshes the app asked for over the same five seconds | 0 count | 0 | 0% |

The cold-start budgets are a simulator's: an empty SwiftUI app linking the frameworks this one links draws its first
frame at about 414 ms on the pinned simulator, so the gate is the floor plus room for the app's own work, and the
400 ms a phone is asked for stays on the physical-phone checklist in [the iPhone app](IOS.md). The simulator reports
60 Hz and composites through the Mac's display, so hitch time is display-link missed-frame accounting, a proxy for a
device's hitch metric. The store read was first measured at about 270 ms on the pinned Mac, of which the node's own
start was 25 ms and the fleet's open one; the rest was the main thread, busy building the shell, taking its time to
hear that the node was up, which the two intervals now leave out. The fleet-render budget is what building the home
from the store's rows should take.

Where a cold launch goes, from the marks (`scripts/ios-perf.py` prints them per launch): about 300 ms before
`main` is reached, loading the images; about 100 ms of UIKit building the scene before the app's composition is
asked for, which an empty app pays too; the composition itself in 2 ms; then the main thread building the shell's
first frame, which is where the app's own time is. The shell builds the tab somebody is looking at, and a tab is
built when first reached for and kept from then; the home's rows are built as they come near the screen. Those two
took the shell from about 430 ms to about 230 ms on the pinned Mac, of which the home with forty remembered rows is
about 80. What is left is a navigation stack, a tab bar and a home being built by SwiftUI for the first time, with
no one item a profile points at. An Instruments profile of the launch on the pinned Mac puts the main thread's
heavy leaves in the loader (270 ms: symbol comparisons, load-command walks, and 85 ms of dyld_sim re-pointing the
shared cache's exports at the host, which a device's loader does not do) and in the Swift runtime's conformance and
metadata work, with no function of ours among them; so the budget on the simulator is 650 ms median and 700 worst,
set from that profile. A device's budget is written when a device is enrolled and measured.

Where reconciliation goes, over a gate that holds each packet 100 ms (a 200 ms round trip, printed by the links'
own logs as `rtt`): two round trips for the QUIC handshake, since a listener answers an address it has not seen with
a Retry; one for the link's Hello and HelloAck; then one to each host for its inventory, with the remembered
sessions' subscriptions in the same flight. A stream to a paired host over a direct link of our own is plain, with
no handshake inside and no wait to be accepted, so the inventory's first bytes leave with the stream's preface; one
Session channel per host carries every agent's subscription. That is four round trips, about 760 ms on the pinned
Mac from 1,986 when each stream waited to be accepted and then handshook inside, and each agent opened a stream of
its own. The test gate itself passes datagrams in the order they came: held each on its own timer, two sent in the
same millisecond could swap places, and a packet overtaking the handshake it followed was dropped and counted lost,
costing a round trip a household network never does.

The relay launch was first gated on the phone's hop alone, which measured about 1,007 ms on the pinned Mac while
leaving the relay's leg to each machine free; a relayed round trip crosses both legs, so the gate now holds both.
Under both gates the pinned Mac measured medians of 1,608 and 1,649 ms, and the 2,000 ms budget leaves about the
room the same-network figure has. A person on mobile data accepts about a second and a half to see a current fleet;
that is a figure for fewer round trips through the relay to reach, not one the budget can ask for yet.

`--only cold|reconciliation|streaming|idle` takes one group, for working on it; `--baseline` needs a whole run.
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

A Mac not in the table is refused, not skipped: `scripts/tests/ios_perf_test.py` holds this page's tables to the
script's, so enrolling a machine or changing a budget is a change here and there in one commit. The report lands in
`target/ios/perf/report.md` with `verdict.json` and `samples.json` beside it; the served net's own record is under
`target/ios/perf/journey/`. `just ios verify` runs the suite last, as `just ios measured`; the hosted runners are not
enrolled, so it is not in `just ios captures`.

The flood's viewer (above) still reads the fleet and one chat through the same subscriptions the app reads, from a
runtime holding nothing, so its budgets say what the phone waits for on the host's side, without a simulator in the
number; the phone suite says what the app does with it.
