# Continuous integration

*For developers who need to know what CI runs on a push, how to run the same checks locally, and how to check Windows from a Mac.*

CI is GitHub Actions. On Linux it runs the gate every landing passes, `just
premerge`, through Buck2 on the build host's cache, so what a landing or a
premerge already tested comes back as cache hits rather than running again.
On macOS two jobs build the Apple apps and run the workspace's tests through
Buck2 the same way, on a worker of the runner's own in front of the build
host's cache. Windows still tests under Cargo beside its Buck2 job. Every job runs `just` recipes — the same recipes a
developer runs locally, so a job's log names the command to reproduce it.
[Build](BUILD.md) describes the recipes; [Testing](TESTING.md) describes
the suites they run.

## Workflows

| Workflow | File | When | What |
| --- | --- | --- | --- |
| CI | [`ci.yml`](../.github/workflows/ci.yml) | Pushes to `main`; pull requests into `main`; by hand | The Linux gate on the build host's cache, the Cargo checks the push reaches, the Apple apps and the macOS tests, and the Windows tests |
| iOS captures | [`ios-captures.yml`](../.github/workflows/ios-captures.yml) | By hand | The phone's store suite on the simulator, journeys and the accessibility audit |
| Release | [`release.yml`](../.github/workflows/release.yml) | A pushed `v*` tag | The `amux` release binaries; see [Release](RELEASE.md) |

The nightlies are not workflows: the build host runs them on timers
([Nightlies](#nightlies)).

Every workflow sets `CARGO_INCREMENTAL=0`, and every job has a
`timeout-minutes` bound.

Every action from outside this repository is pinned to a full commit, with
the version that commit carries in a comment beside it; moving to another
version changes both, from the upstream tag dereferenced to its commit. A
tag can be moved to other code, and a commit cannot. `dtolnay/rust-toolchain`
publishes branches rather than releases: the jobs on the repository's
toolchain pin the commit of its `1.98.0` branch, so a toolchain bump in
`rust-toolchain.toml` moves that pin too, and the nightly jobs pin a commit
of `master` and name their toolchain.

Every workflow's token may only read the repository unless a job asks for
more: the Release workflow's build jobs may sign attestations and its release
job may write the release; the rustls bump pushes a branch and opens a pull
request. The Release workflow restores no Rust cache: a tag's run can read
the caches `main`'s CI jobs saved, so whatever those jobs run could otherwise
reach a shipped binary.

### CI jobs

Every job runs on every push. The Linux gate needs nothing to tell it what
a change can reach: Buck2 reruns only the tests and checks whose inputs
changed, and takes the rest from the cache. The Cargo checks after it run
only when the push changed what they check. The macOS jobs, through Buck2
too, rerun only what changed; the Windows Cargo job runs whole.

A newer push to a branch stops the run of the push before it, so a burst of
pushes does not queue for runners. The Cargo
checks therefore compare a push with the last push whose run finished,
passing or failing, rather than the one before it, whose changes may never
have been judged; a run started by hand, or a push with no such commit to
compare with, runs them all.

An organisation on GitHub's free plan runs 20 jobs at once and the rest
wait, so the run has as few jobs as keep each under ten minutes: work that
takes seconds shares a job.

| Job | Runner | Runs |
| --- | --- | --- |
| Linux gate | `ubuntu-latest`, its actions on the build host | `just premerge` through Buck2 on the build host's cache, `just browser audit`, `just doctest` ([The Linux gate](#the-linux-gate)) |
| Pointer checks over the C ABI | `ubuntu-latest` | `just miri`: the phone's C ABI tests under the pinned nightly's interpreter, which fails on any undefined behaviour in a pointer handed across the boundary, read or freed; the tests that start an installation or a tokio runtime are ignored there, each saying why. After the gate, when the push changed what app-ffi is built from |
| Address sanitizer, Thread sanitizer | `ubuntu-latest` | `just sanitize address` and `just sanitize thread`: the tests that reach unsafe code and C, built on the pinned nightly with the standard library under the sanitizer; `address` fails on an out-of-bounds access, a use after free or a double free, `thread` on a data race. After the gate, when the push changed one of the crates whose tests they run |
| Apple apps | `macos-26`, its actions on a worker of its own | `just ios simulator golden`, then through Buck2 every target under `apps/apple` built, the shipping app for a phone built too, and every Apple test run: the package suites and the hosted app suite on the simulator, the Mac app's tests on the runner ([The macOS jobs](#the-macos-jobs)) |
| macOS tests | `macos-26`, its actions on a worker of its own | `just test //crates/...` through Buck2: every workspace test that isn't Linux-only, the terminal journeys and the system suites among them |
| Test | `windows-latest` | `scripts/no-update-flags.sh`, `just cargo-test -- --profile ci --exclude models` (the whole suite; Windows compiles out the slow Unix suites) |

The test job runs nextest's `ci` profile, which does not stop at the first
failure, so a platform reports every failing test; the job's log holds each
failing test's output. Its matrix does not fail fast either: a
failure on one operating system does not cancel the others. The Windows
runner compiles and runs every test binary; suites that need Unix processes,
PTYs or sockets are compiled out there. Among them are the tests that host
terminal Claude: the agent's terminal cases (in its `attach`, `dump`,
`providers`, `tools` and `lifecycle` suites), provider-fakes' `pty` suite,
the terminal modules of its `conformance` and `scripted` suites, and the
terminal client's `served` suite, whose hosts run terminal Claude. Windows does not host
terminal Claude in this build (see
[Windows, as a stated cost](ARCHITECTURE.md#windows-as-a-stated-cost)), so
the Windows job runs the agent's test of that refusal instead, and every
headless Claude and Codex test as everywhere.

The terminal journeys are `reach-host`,
`conversation-decision-claude-pty`, `conversation-decision-claude-sdk`,
`conversation-decision-codex`, `leave-and-recover`, `manage-agent`,
`attachment-or-review` and `keep-authority`.

### The Linux gate

The Linux gate job runs `just premerge`, the same gate every landing
passes, with Buck2 sending its actions to the build host's NativeLink over
the tailnet, so a commit the landing queue or a premerge on the build host
already tested is cache hits. The runner joins the tailnet as `tag:ci`
with the repository's Tailscale OAuth client (`TS_OAUTH_CLIENT_ID`,
`TS_OAUTH_SECRET`), which the tailnet's rules let reach the build cache's
port and nothing else; the build host's name is `BUILD_HOST` at the top of
`ci.yml`. The pinned tools come from the repository's Nix flake (`nix
develop`), as on the build host, so the runner's actions have the build
host's keys.

A fork's pull request gets no secrets and so no tailnet. Its gate starts a
NativeLink on the runner (`scripts/nativelink.py serve`) and builds and
tests everything there: slower, the same checks. Running the workflow by
hand with `without-build-host` does the same, to check that path.

The job then audits the dependencies the browser extension ships (`just
browser audit`) and runs the documentation tests under Cargo (`just
doctest`). The C ABI's pointer checks (`just miri`) and the sanitizers run
under Cargo too, after the gate, in jobs of their own and only when the
push changed what they check ([`scripts/landing.py`](../scripts/landing.py)
`covers`): they rebuild the standard library under their checks, which
Buck2's Rust rules can't.

### The Windows job

The Windows job tests the workspace on `windows-latest` without compiling
anything there. It joins the tailnet as the Linux gate does and runs `just
check-windows` against the build host, whose Linux worker compiles and
links every Windows binary with the Windows toolchain ([Windows
builds](BUILD.md#windows-builds)); a commit already built that way is cache
hits. Then it starts a NativeLink of its own
([`build/nativelink/windows.json5`](../build/nativelink/windows.json5),
`scripts/nativelink.py windows`) in front of the build host's cache, with a
worker that offers `OSFamily=windows`, and runs `just test-windows`: every
compile and link is a hit in the build host's cache, and each test binary
runs on that worker, which fetches it from the build host. A test runs
through a worker rather than Buck2 itself because only a result from a
worker is recorded, so a test whose inputs haven't changed since a run that
passed it is not run again.

The build host's action cache can't be written from CI, so the tests'
results go into the runner's own, which the job keeps in GitHub's cache
between runs (`windows-test-results-*`); what each test printed goes to the
build host's cache too, so a kept result still finds it. Each test binary
takes the whole worker and runs four of its tests at a time, as nextest did
on the runner's four cores. Left out, as under Cargo: the protocol models,
the simulated stories and the terminal journeys.

A fork's pull request has no tailnet, so its Windows job tests under Cargo
on the runner instead (`just cargo-test -- --profile ci --exclude models`),
as every Windows run did before; so does a run by hand with
`without-build-host`.

### Bounds

Three kinds of bound stop a run, and none is ever raised to hide a failing
or slow test.

- **A test's bound** is nextest's under Cargo (`slow-timeout` in
  [`.config/nextest.toml`](../.config/nextest.toml)) and the same under
  Buck2, whose test runner ([`build/test-runner.py`](../build/test-runner.py))
  holds each test to it: a test is reported slow
  after a minute and ended after three, a protocol model after two and
  sixteen minutes. The slowest ordinary test, a testnet link story, took
  47 s on the Mac with the whole suite running beside it. A hang ends the one
  test that hung, names it, and the run goes on.
- **A recipe bound** is a hang detector for one phase, sized above that
  phase's honest duration. `just cargo-test` runs two phases, each under its own
  bound ([`scripts/workspace-test.sh`](../scripts/workspace-test.sh)): compiling
  every test binary (`cargo nextest run --no-run`, 1200 s, the bound `just
  test-build` carries), then running them (1000 s). The run's bound is the slowest
  platform's measured clean run — one where no failing test waits out its own
  timeout — times 1.5, rounded up to the hundred.
- **The job guard** (`timeout-minutes`) bounds the honest total of a job:
  setup, cache restore, every step. The test job's is 30 minutes.

What the Cargo jobs' bounds were sized from, on a warm cache, under `cargo
test`, before the tests ran under nextest (on the Mac the same selection
went from 441 s to 253 s), when Linux still tested under Cargo too:

| Platform | Compile | Test run | Whole job |
| --- | --- | --- | --- |
| `ubuntu-latest` | 145 s | 601 s | 13 min 6 s |
| `macos-latest` | 95 s | 449 s | 13 min 13 s |
| `windows-latest` | 239 s | 286 s | 11 min 41 s |

Linux is the slowest clean test run: 601 s × 1.5 is 902 s, so the run's
bound is 1000 s. Those are whole runs; each platform now runs the tests as
two jobs of half of them each, so a job is the compile and about half the
run. The compile is most of such a job, and both halves pay it: the test
jobs compile through sccache, which keeps what rustc made of each of amux's
crates by its inputs (cargo's own cache judges them by file times, which a
fresh checkout renews). Run twice on one commit, the second run fetched all
70 crates it could have compiled, and a job took 6 minutes on Linux where
the first took 9, 8 on Windows where it took 12, and 5 to 11 on macOS where
it took 13; a push that changes a crate compiles it and what is built from
it. Linking the 83 test binaries is not kept. Windows
compiles out most of the slow suites (the embedded client, the supervisor,
the system journeys, terminal Claude), so its clean run is under half of
Linux's and does not move the bound. A cold cache, after a `Cargo.lock`
change, compiles the whole dependency graph and can take longer than these
numbers.

No test builds a binary mid-run. The nextest setup script builds `amux`, the fakes and `fake-amux` once, before
the first test, with `--workspace` feature unification so it reuses the rlibs `just test` already compiled (under two seconds). It is skipped for `models` and for simulated runs (`TESTNET_MODE=simulated`), neither of which spawns those binaries. Tests that built the binaries
themselves each ran cargo again, with the features of their own package: every test crate's first such build
recompiled part of the graph while other tests ran, and a test process per test would have queued every one of
them on cargo's lock.

### Protocol models

The protocol models ([`crates/models`](../crates/models/README.md)) explore
every state of the real cores within their bounds. At their CI bounds they
are one Buck2 test, `//crates/models:integration`, in the Linux gate: it
needs the whole of a worker for about eleven minutes, so it carries the
`nova-only` label and runs on the build host from any machine, rerunning
only when what the models are built from changed. The macOS tests job
skips them with the other `nova-only` tests, and the Windows Cargo job
leaves them out (`--exclude models`).

Their nightly bounds explore further than a push can wait for: each
model's ignored nightly test, one after another, in the build host's
`models` nightly ([Nightlies](#nightlies)); `just models-nightly` runs the
same tests anywhere, and model names narrow it. The slowest model at its
nightly bounds, the single daemon's peer, took two hours on the Mac's 12
threads.

### Simulated stories

The testnet's stories that the test catalogue declares a simulated mode for
(`modes` in [`tests/catalog.toml`](../tests/catalog.toml)) run twice: on
the real net in `//crates/testnet:integration`, then on the simulated net
in `//crates/testnet:simulated`, the same test binary under
`TESTNET_MODE=simulated` with its filters naming those suites, every core
in one process, a few seeds per story. `just tests-check` fails when the
filters and the catalogue disagree. A failing seed is printed with the
`just sim --seed N <story>` that reruns it under Cargo. The `models`
nightly walks 2000 seeds per story the same way (`scripts/sim.py --seeds
2000`, under a bound of its own). A story walks its seeds inside one test,
so a walk of many seeds runs under nextest's `seeds` profile, which ends a
story after half an hour rather than three minutes. On the Mac the slowest
story takes four and a half minutes at 2000 seeds, with the others running
beside it.

### Nightlies

What no push or landing waits for runs every night on the build host, from
a checkout of `main` it keeps for each, under systemd timers the build
host's own configuration defines. Each runs
[`scripts/nightly.py`](../scripts/nightly.py) with its kind, every step
inside the checkout's `nix develop`:

| Kind | Starts | Runs |
| --- | --- | --- |
| `models` | 22:00 | `just models-nightly`, every protocol model at its nightly bounds (about three and a half hours on the build host), then every simulated story under 2000 seeds |
| `nightly` | 02:00 | `just release-check`, the shipping binary under the release profile and the release dependency policy, on Linux; then both sanitizers over every crate whose tests they run, where a landing runs them only when one of those crates itself changed |
| `offline` | 03:00 | `just offline-test`, the workspace tests with no network but loopback, after fetching the crates |

Every step runs whichever failed before it; the run's last line names the
kind and what failed, and its log is on the build host. Run one anywhere
with `scripts/nightly.py KIND` (`--nix` inside `nix develop`). A release
builds the shipping binary on all three platforms itself.

### The macOS jobs

Two jobs run what only a Mac can, side by side since they share little: the
Apple apps build the bridge for the phone, the simulator and the Mac, the
tests build the workspace for this Mac. Each selects Xcode 26.6, installs
the pinned Rust toolchain with both ARM iOS targets and tmux, joins the
tailnet and starts a NativeLink on the runner (`scripts/nativelink.py
macos`, `build/nativelink/macos.json5`) whose worker runs every action.
Its CAS writes through to the build host's, and its action cache is the
runner's own, read before the build host's: the build host's can't be
written from here, so the results are kept in GitHub's cache between runs,
as the Windows job keeps its tests'. A push therefore rebuilds and reruns
only what its change reaches. A fork's pull request, with no tailnet, runs
on the runner's NativeLink alone (`nativelink.py run --remote none`).

The Apple apps job makes the golden simulator first (`just ios simulator
golden`), which the phone's suites lease: on a runner, with no `wt`, a test
takes `amux-iphone-1` under a lock ([the iPhone app](IOS.md#builds)). It
then builds every target under `apps/apple`, the Release app for a phone
too, which holds the device graph to compiling, and runs every Apple test.
The macOS tests job runs `just test //crates/...`: on macOS that is the
system lane as well, since the built `amux` binary's suites (`process`,
`supervise_cli`, `overlap`, `attach` and the rest), node's `supervisor`
suite, the `survive-daemon` system journey and the terminal journeys are
ordinary Buck2 tests. The `nova-only` tests are the Linux gate's and are
skipped.

`just ios verify` is the whole sequence a developer runs before pushing a
phone change: the workspace's `fmt-check`, `lint` and `test` (which runs
the spec suites), then `just ios gate` (lint, script tests, graph check,
simulator, one build of the app and every suite, the unit suites), then
the captures (`test-store-ios`, `ios journey`, `ios accessibility`), then
the shipping stage (`ios scope-audit`). A failing `simulator golden` or
`test-build` ends it, since the stages after them run what they made; any
other stage failing lets the rest run, and the run fails at its end naming
every stage that did. It refuses to start if any stage names a recipe the
justfiles no longer declare. `just ios gate`, `just ios captures` and `just
ios shipping` run those parts alone. Nothing compares a picture
([the iPhone app](IOS.md#no-picture-is-compared) says why).

### Android

The Android app is in the Linux gate: `just premerge` runs `//apps/android/...`
(the unit suites, the writing lints, the scope audit and the build of the
debug APK, the bridge built for Android with them), whose actions run on the
build host. The Android gate workflow and the Android target check, which
ran Gradle and `cargo-ndk` on a runner, went with Gradle.

### iOS captures

Run by hand, the workflow boots the golden simulator on `macos-26` and runs
`just ios captures`: the store suite on the simulator, journeys and the
accessibility sweep. Nothing it runs uses the small simulator, which took
seven minutes to boot. It starts the same NativeLink as the macOS jobs and
restores the Apple apps job's results read-only, so the app it already
built is a hit, and never saves its own. It uploads the journey
evidence and the audit's record.
[The iPhone app](IOS.md) explains what those compare.

### Waiting

A timing race in a test is a wait on a proxy for "done": a row visible, an
agent idle, text present, asserted on a neighbour that lands a moment
later. Tests wait instead on what the runtime reports in its own terms,
through one shared wait (the `patience` crate: `until`, a probe that
answers what it found or what it saw instead, and a missed deadline that
reports the last answer) and, across daemons, testnet's fences on a host's
cursors ([testnet](TESTNET.md), "Fences"). Fixtures that know more show it
under the wait's report: a terminal's screen, an agent's journal, a
supervisor's log. The exception is `holds_for`, a window of real time for
asserting that something does not happen when nothing the test controls
gates it; every window carries a comment saying why time is the only
witness, and a new one is a design question, not a default.

## Lanes

Each suite in [`tests/catalog.toml`](../tests/catalog.toml) names a lane:
when and where it runs. `just tests-list` prints the catalogue, and `just
tests-check` (in CI) fails if a suite names a recipe, Cargo target, feature,
journey or baseline that does not exist.

| Lane | What it holds | Where it runs |
| --- | --- | --- |
| `fast` | Offline, credential-free tests with no wall-time settling: unit tests, spec suites, adapter replays, store and effect integration, renderer goldens | `just test` in the Linux gate and the macOS tests job, and `just cargo-test` on Windows, on every push; the Apple suites in the Apple apps job |
| `system` | Real processes and transports with bounded deadlines: the built `amux` binary and its supervisor, the store on iOS | The desktop suites in the Linux gate and the macOS tests job on every push; `test-store-ios` in `just ios captures` |
| `tool` | The repository's own machinery: script contracts, source and dependency policy, the CI tooling, the shipping scope audit | The Rust side in `just test`; `just script-tests`, `ios script-tests`, `ios lint`, `android script-tests` and `android lint` in `just premerge`, so in the Linux gate on every push; `ios scope-audit` on every app release's signed bundle |
| `journey` | Complete client stories through the shared manifest, and the phone's accessibility sweep | The terminal journeys in the Linux gate and the macOS tests job, and the system journey inside `just test`, on every push; phone journeys and the accessibility sweep in iOS captures run by hand |
| `qualification` | Real providers, real cloud accounts and measured performance | Never on an ordinary push: `just live`, `just perf` and the `just ios qa-*` recipes by hand, on machines that have the credentials or hardware; `just ios perf` by hand on the enrolled Mac, last in `just ios verify` |

[Testing](TESTING.md) defines the lanes and boundaries fully.

## `just premerge`

`just premerge` is what a change passes on the machine it was written on
before it reaches main, and what `just land` runs on the merge before
pushing it. It runs every check below, the iOS and Android writing lints
and script tests, every terminal journey, the browser extension's checks and
tests, and every workspace test.

The tests, the journeys, the browser extension's checks and most of the
checks below run through Buck2 and its cache ([Building](BUILD.md#the-build-cache),
[Repository checks](BUILD.md#repository-checks)), so each runs only when
something it reads changed since it last passed, in this tree or any other
on the machine. A change to a leaf crate or a
document reruns a handful of tests, and premerge takes a minute or two; a
change to a crate most of the workspace depends on, such as `wire`, reruns
most of the suite, about a quarter of an hour on a laptop.

It leaves out the Apple apps: any change to a core crate rebuilds the
bridge for three platforms and reruns the simulator suites, which is not a
load every premerge on a laptop should carry. Whoever changes Apple code
runs `just ios verify` and `just mac test`, on the same cache, and CI's
Apple apps job runs them after every push. The models at their CI bounds
are `nova-only`: they run on the build host from any machine, and are
skipped, and named, when it is out of reach. Windows runs only in CI; the sanitizers run at landing on
Linux and in CI ([The Linux gate](#the-linux-gate)), and the release
build in the nightly.

The checks, in the order it runs them:

| Recipe | Checks |
| --- | --- |
| `no-update-flags` | No environment variable starting with `UPDATE_` is set, so no fixture is rewritten during an asserted run |
| `lint` | Clippy over every library, binary and test of the workspace, warnings denied, with the workspace's `clippy.toml` ([`build/checks/clippy.py`](../build/checks/clippy.py)) |
| `fmt-check` | Formatting, with the pinned nightly |
| `codegen-check` | The committed protobuf output matches the schemas (`//:codegen-check`) |
| `proto-compat` | The schemas pass buf's lint, and `amux.v1` still reads what the latest release from 0.11.0 wrote; with a ref, what that ref wrote ([`buf.yaml`](../buf.yaml)) |
| `store-check` | Every store migration the latest release from 0.9.0 shipped is unchanged, and the ones added since only add: no table or column is gone or changed type, and any new column is one an existing store can take; with a ref, against that ref ([`scripts/store-check.py`](../scripts/store-check.py)) |
| `dependency-policy` | Production and test-infrastructure dependency boundaries ([`scripts/check-dependency-policy.py`](../scripts/check-dependency-policy.py)) |
| `deny` | No dependency has an open security advisory or is a yanked release, every licence is one [`deny.toml`](../deny.toml) allows, and every crate comes from crates.io or the rustls patch; each advisory it sets aside says why it does not reach amux |
| `shear` | Every dependency a manifest lists is used by that crate's code; one only the code cannot show, such as a crate named only to pass on a feature, is listed in that crate's manifest as one to skip, with why |
| `cores-check` | No state-machine core (`daemon-core`, `relay-core`, `agent-core`, `supervisor-core`) nor the `core-ring` they record inputs in has tokio in its dependency graph on any target, and none of their sources holds a lock, spawns a task or awaits ([`scripts/cores-check.py`](../scripts/cores-check.py)) |
| `typed-provider-check` | The interpreters, the agent's provider handshake and Claude's messaging socket read and write provider messages only through the protocol crates: no `json!`, field looked up by name or untyped `Value` outside the listed tool payloads ([`scripts/typed-provider-check.py`](../scripts/typed-provider-check.py)) |
| `client-kind-check` | The client crates (ui-state, ui-view, ui-runtime, tui, app-runtime, browser-core) decide nothing by an agent's `Kind` or by a provider id written out (Claude's, Codex's or a built-in ACP agent's, read from `wire::agents::BUILT_INS`), outside the listed places, each with its reason; a listed place that matches nothing fails too ([`scripts/client-kind-check.py`](../scripts/client-kind-check.py)) |
| `docs-check` | Every link, image and heading anchor under `docs/` resolves, every figure is referenced and standalone, and `docs/README.md` lists every page once ([`scripts/docs-check.py`](../scripts/docs-check.py)) |
| `tests-check` | The suite catalogue names only things that exist |
| `script-tests` | The unit tests of the repository's own scripts |
| `shipping-check` | No crate that exists for tests and no test feature is in the desktop binary's or the browser extension's graph as they ship ([`build/checks/shipping.py`](../build/checks/shipping.py)) |
| `parity` | Every crate's `BUCK` file and `Cargo.toml` agree on its targets and dependencies ([`build/checks/parity.py`](../build/checks/parity.py)) |
| `test` | Every workspace test, through Buck2: only the tests whose inputs changed since a passing run, in any tree on the machine, run again |

## Checking Windows from a Mac

`just check-windows` compiles and links every workspace target for
`x86_64-pc-windows-msvc` on the build host's Linux worker, with the Windows
toolchain the Windows job's binaries are built with ([Windows
builds](BUILD.md#windows-builds)): a Windows-only compile or link error
shows up there before CI. It finds build errors only; Windows runtime
behaviour still needs a Windows machine, which the Windows job's runner is.
