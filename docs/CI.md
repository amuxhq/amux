# Continuous integration

*For developers who need to know what CI runs on a push, how to run the same checks locally, and how to check Windows from a Mac.*

CI is GitHub Actions. On Linux it runs the gate every landing passes, `just
premerge`, through Buck2 on the build host's cache, so what a landing or a
premerge already tested comes back as cache hits rather than running again.
On macOS four jobs build the iPhone app, build the Mac app, run the
workspace's tests and run the terminal journeys through Buck2, on the runner itself, with a cache of
their own kept between runs, and on Windows a job runs the workspace's tests.
Those five run only when a change reaches what they alone test ([Which jobs
a change runs](#which-jobs-a-change-runs)). Every job runs `just` recipes — the same recipes a
developer runs locally, so a job's log names the command to reproduce it.
[Build](BUILD.md) describes the recipes; [Testing](TESTING.md) describes
the suites they run.

## Workflows

| Workflow | File | When | What |
| --- | --- | --- | --- |
| CI | [`ci.yml`](../.github/workflows/ci.yml) | Pushes to `main`; pull requests into `main`; by hand | The Linux gate on the build host's cache, the Apple apps and the macOS tests, and the Windows tests |
| Nightly | [`nightly.yml`](../.github/workflows/nightly.yml) | Every night at 01:00 UTC; by hand | The protocol models, the C ABI's pointer checks, the sanitizers, the release build, the offline tests and the simulated stories under 2000 seeds ([Nightlies](#nightlies)) |
| iOS captures | [`ios-captures.yml`](../.github/workflows/ios-captures.yml) | By hand | The phone's store suite on the simulator, journeys and the accessibility audit |
| Release | [`release.yml`](../.github/workflows/release.yml) | A pushed `v*` tag | The `amux` release binaries; see [Release](RELEASE.md) |

Every workflow sets `CARGO_INCREMENTAL=0`, and every job has a
`timeout-minutes` bound.

Every action from outside this repository is pinned to a full commit, with
the version that commit carries in a comment beside it; moving to another
version changes both, from the upstream tag dereferenced to its commit. A
tag can be moved to other code, and a commit cannot. `dtolnay/rust-toolchain`
publishes branches rather than releases: the jobs on the repository's
toolchain pin the commit of its `1.98.0` branch, so a toolchain bump in
`rust-toolchain.toml` moves that pin too, and a job on the pinned nightly
pins a commit of `master` and names its toolchain.

Every workflow's token may only read the repository unless a job asks for
more: the Release workflow's build jobs may sign attestations and its release
job may write the release; the rustls bump pushes a branch and opens a pull
request. The Release workflow restores no Rust cache: a tag's run can read
the caches `main`'s CI jobs saved, so whatever those jobs run could otherwise
reach a shipped binary.

### CI jobs

The Linux gate runs on every push, and needs nothing to tell it what a
change can reach: Buck2 reruns only the tests and checks whose inputs
changed, and takes the rest from the cache. The macOS and Windows jobs,
through Buck2 too, rerun only what changed, but each still costs minutes of a
runner the organisation has few of, so each runs only when the change
reaches what it alone tests ([Which jobs a change runs](#which-jobs-a-change-runs)).

CI on `main` judges the newest commit, not every commit. Each job of a
push's run waits for the same job of the run before it, and a newer push
takes the place of one still waiting; a job that has started is never
stopped, so the macOS jobs always keep their store. The newer commit holds
every change of the ones it skipped, so a burst of landings is judged once,
at its end, rather than queuing an hour of runs: the organisation has five
macOS runners and a run takes four. A newer push to a pull request's
branch stops the run of the push before it, which it supersedes.

When GitHub is short of macOS runners, a job can wait about 15 minutes and
end cancelled with "not acquired by Runner" without running a step. If those
are the only jobs of a branch's newest run that didn't pass, the workflow in
`rerun-unacquired.yml` asks for them once more; a job that ran and failed,
or one a newer push replaced, is left alone.

An organisation on GitHub's free plan runs 20 jobs at once and the rest
wait, so the run has as few jobs as keep each under ten minutes: work that
takes seconds shares a job.

| Job | Runner | Runs |
| --- | --- | --- |
| Which jobs this change runs | `ubuntu-latest` | `scripts/ci_jobs.py decide`: whether each of the five jobs below the documentation tests runs, and why, in the run's summary ([Which jobs a change runs](#which-jobs-a-change-runs)) |
| Linux gate | `ubuntu-latest`, its actions on the build host | `just premerge` through Buck2 on the build host's cache ([The Linux gate](#the-linux-gate)) |
| Documentation tests | `ubuntu-latest` | `just doctest`, under Cargo, beside the gate |
| Linux gate, Android app | `ubuntu-latest` | Only without the build host (a fork's pull request): `just premerge-half android`, the Android app's half of the gate, while the Linux gate runs `just premerge-half workspace`; with the build host it does nothing |
| Windows | `windows-latest`, its builds on the build host | `just check-windows` on the build host, then `just test-windows` on a Windows worker of the runner's own ([The Windows job](#the-windows-job)); without the build host, `just cargo-test` |
| iPhone app | `macos-26`, building in Buck2 and testing on a worker of its own | `just ios simulator golden`, then through Buck2 every target under `apps/apple` but the Mac app's built, and the package suites and the hosted app suite run on the simulator ([The macOS jobs](#the-macos-jobs)) |
| Mac app | `macos-26`, the same | The Mac app, its storybook and screens built, and the Mac app's tests run on the runner |
| macOS tests | `macos-26`, the same | `just test //crates/... --exclude journey` through Buck2: every workspace test that isn't Linux-only, the system suites among them |
| macOS journeys | `macos-26`, the same | `just test //crates/testnet:terminal-journeys`: the terminal journeys, which take the whole worker |

On Windows, suites that need Unix processes, PTYs or sockets are compiled
out. Among them are the tests that host
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
port and nothing else. The build host's name on the tailnet is the
repository's `BUILD_HOST` Actions variable (`gh variable set BUILD_HOST
--body <host>`), which `ci.yml` reads into its environment. The pinned
tools come from the repository's Nix flake (`nix develop`), as on the build
host, so the runner's actions have the build host's keys.

A fork's pull request gets no secrets and so no tailnet, and a repository
with no `BUILD_HOST` set has no build host to reach. Its gate starts a
NativeLink on the runner (`scripts/nativelink.py serve`) and builds and
tests everything there: slower, the same checks. All of it on one
four-core runner takes longer than a test run's bound, so the gate runs in
two halves on two runners (`just premerge-half workspace` here, `just
premerge-half android` in the Linux gate, Android app job), which together
are `just premerge`. The runner's worker offers its real cores, and a test
that takes the whole worker asks for that many (`AMUX_LINUX_WORKER_CORES`,
read by `scripts/buck-config.py`): a worker claiming the build host's
eight would run such a test beside others, and the tests that measure time
fail. Running the workflow by hand with `without-build-host` does the
same, to check that path.

The documentation tests run under Cargo (`just doctest`)
in a job of their own beside the gate, since Cargo compiles the workspace for
them on the runner. The C ABI's pointer checks (`just miri`) and the sanitizers run
under Cargo too, since they rebuild the standard library under their checks,
which Buck2's Rust rules can't, but in the nightly rather than on a push
([Nightlies](#nightlies)).

### Which jobs a change runs

Five jobs are gated: the iPhone app, the Mac app, the macOS tests, the macOS
journeys and Windows. A first job on Linux, `changes`
([`scripts/ci_jobs.py`](../scripts/ci_jobs.py)), decides for each whether
this change runs it, and the job's `if` reads the answer; a job the change
doesn't reach shows as skipped. The rest of CI always runs.

A job runs when the change touches its territory: what that job alone
exercises, which the Linux gate cannot vouch for. The territories are in
[`build/ci-jobs.toml`](../build/ci-jobs.toml), and each changed file is
judged in this order:

1. `everyone`: the build graph's shape (`Cargo.lock`'s third-party
   entries, `third-party/`, the Rust and C++ rules and toolchains, the
   platforms) and CI's own machinery (the actions, the Buck2 and NativeLink
   scripts, the table itself). Every gated job runs.
2. A job's `paths`, or for a file under its `contains_in`, a `contains`
   substring in the file before or after the change. The phone's is
   everything under `apps/apple` but the Mac app's, the Apple build rules,
   the C ABI crate it calls (`app-ffi`) and the tools that write its Swift
   mirrors (`ios-tools`), and code only an iOS build compiles (code for iOS
   and Android alike is the Android build's in the Linux gate). The macOS
   tests and journeys own code only a Mac compiles, the root `justfile` and
   the macOS worker's configuration; Windows owns code only Windows compiles
   (`cfg(windows)`, `not(unix)` and the like), its toolchain, worker and
   recipes. That job runs.
3. `covered`: what the Linux gate exercises (every crate, the protocol, the
   documentation, the scripts) or what no gated job uses. No gated job runs.
4. Anything else is unclassified, and every gated job runs.

Two files are read rather than matched. A `Cargo.lock` change that touches
only workspace crates' entries (a version, a dependency between crates) is
`covered`. In `.github/workflows/ci.yml` a changed line runs the job whose
block holds it, every job that shares a block through a YAML anchor, every
gated job when it is outside the jobs, and none when it is in another job's
block.

Each job judges everything that changed since its newest successful run on
main, which the decision reads from GitHub, not just the push: CI on main
runs for the newest commit only, so a commit whose job a newer push took the
place of never ran it, and a job that failed keeps running until it passes.
That run's commit must be behind this one, or the job runs. A pull request
is judged from main's last pass when the pull request contains it, else from
where it left main. A run started by hand runs every job, and so does a
decision that fails: a gated job runs unless `changes` said `false`, and
`ci_jobs.py` answers `true` for every job when anything goes wrong deciding.
`scripts/ci_jobs.py explain BASE HEAD` says what a change runs, locally.

To classify a new file, add its path, or a glob, to the table it belongs
in: a job's `paths` when only that job tests it, `covered` when the Linux gate
does or nothing gated uses it, `everyone` when it shapes every build. Two
checks hold the table to the tree. The scripts' unit tests
(`//:script-tests-ci_jobs`) fail on a tracked file no table names, so a new
directory gets a decision when it lands rather than running everything. And
`just ci-jobs-check`, in premerge, asks Buck2 what each Apple app's graph
reads that no Linux target compiles, and fails on any such file outside that
app's `paths`: the Mac app's storybook reads `tests/stories.json`, which only
the graph says. Those graphs configure only on a Mac, so off one, as on
the landing host, the check says it skipped.

The macOS and Windows jobs are skipped on a change far from them, such as
one in `node` or `daemon-core`, though the phone embeds both: a change there
is tested by the Linux gate, the terminal client and testnet, and should a
behaviour only the phone's suites see break, the phone's job finds it at the
next change near the phone, and a release finds it first (below). Two things
keep a skipped job from hiding what only it catches. Every Windows binary is
built in the landing gate (`just premerge` runs `just check-windows`), since
a change that stops Windows compiling is usually ordinary code calling a Unix
API defined elsewhere, which no file territory can name. And each release
asks that CI judged the commit it ships: `just ios release` refuses unless
the iPhone app's last pass on main covers HEAD (`scripts/ci_jobs.py
--covered iphone`), `just mac release` the Mac app's, and `just amux release`
the macOS tests', the macOS journeys' and Windows'. When one has not, it says
which file it has not judged; `gh workflow run ci.yml --ref main` runs every
gated job on main, and once they pass the release goes ahead. A rehearsal
ships nothing and doesn't ask.

Replayed over main's last 400 commits, the table skips the phone's job on
about half of them, the Mac app's on half, the macOS tests and journeys on
two thirds and Windows on three fifths; a job that does nothing still takes
six to nine minutes of a macOS runner to set up.

Rejected: choosing jobs by what a change can reach, as
`scripts/ci-affected.py` did before the gate moved to Buck2 (a job ran when
the change touched a crate its tests are built from). Buck2's cache already
reruns only what a change reaches, so that saved nothing the cache doesn't,
and nearly every Rust change reaches the phone's bridge (32 of the 53
crates): replayed over the same commits, it would skip the phone's job on a
fifth of them.

### The Windows job

The Windows job tests the workspace on `windows-latest` without compiling
anything there, when the change reaches what only Windows tests. It joins
the tailnet as the Linux gate does and runs `just check-windows` against the
build host, whose Linux worker compiles and links every Windows binary with
the Windows toolchain ([Windows builds](BUILD.md#windows-builds)); the
landing gate built the same commit that way, so the step is cache hits. Then it starts a NativeLink of its own
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
takes one of the worker's six slots, on the runner's four cores, and runs
two of its tests at a time, so six binaries run at once and their
downloads from the build host overlap the others' running. The job keeps Buck2's and NativeLink's logs
as its `buck2-logs-windows` artifact. Left out, as under Cargo: the protocol models,
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
  setup, cache restore, every step. The Windows job's is 60 minutes.

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
every state of the real cores within their bounds. They look for faults
rarer than a change should wait on, and take minutes at their small CI
bounds and hours at their nightly ones, so no push, landing or premerge
runs them: their Buck2 test, `//crates/models:integration`, carries the
`nightly` label, which `just test` and `just test-windows` leave out
unless a target names it, and the Windows Cargo job leaves them out too
(`--exclude models`). The nightly workflow checks each model at both its
bounds ([Nightlies](#nightlies)); `just models-nightly` runs the same tests
anywhere, and model names narrow it.

### Simulated stories

The testnet's stories that the test catalogue declares a simulated mode for
(`modes` in [`tests/catalog.toml`](../tests/catalog.toml)) run twice: on
the real net in `//crates/testnet:integration`, then on the simulated net
in `//crates/testnet:simulated`, the same test binary under
`TESTNET_MODE=simulated` with its filters naming those suites, every core
in one process, a few seeds per story. `just tests-check` fails when the
filters and the catalogue disagree. A failing seed is printed with the
`just sim --seed N <story>` that reruns it under Cargo. The nightly walks
2000 seeds per story the same way (`scripts/sim.py --seeds
2000`, under a bound of its own). A story walks its seeds inside one test,
so a walk of many seeds runs under nextest's `seeds` profile, which ends a
story after half an hour rather than three minutes. On the Mac the slowest
story takes four and a half minutes at 2000 seeds, with the others running
beside it.

### Nightlies

What no push or landing waits for runs every night on `main`, at 01:00 UTC,
in the Nightly workflow on GitHub's hosted runners: the protocol models,
the C ABI's pointer checks, the sanitizers, the release build, the offline
tests and the simulated stories under thousands of seeds. They look for
faults rarer than a change should wait on, and the models alone run for
hours. Each job runs
[`scripts/nightly.py`](../scripts/nightly.py) with its kind, inside the
repository's `nix develop` but for the release build, which uses rustup's
pinned Rust and the system's C compiler as a release does:

| Job | Kind | Runs |
| --- | --- | --- |
| Model (one per model) | `models <model>` | `just models-nightly <model>`: the model's tests at its CI bounds and at its nightly bounds, every core of a four-core runner |
| Pointer checks over the C ABI | `pointers` | `just miri`: the phone's C ABI tests under the pinned nightly's interpreter, which fails on any undefined behaviour in a pointer handed across the boundary, read or freed; the tests that start an installation or a tokio runtime are ignored there, each saying why |
| Address sanitizer, Thread sanitizer | `address`, `thread` | `just sanitize address` and `just sanitize thread`: the tests that reach unsafe code and C, built on the pinned nightly with the standard library under the sanitizer; `address` fails on an out-of-bounds access, a use after free or a double free, `thread` on a data race |
| Offline tests | `offline` | `just offline-test`, the workspace tests with no network but loopback, after fetching the crates |
| Simulated stories, 2000 seeds | `seeds` | `scripts/sim.py --seeds 2000`, every simulated story under 2000 seeds |
| Release build | `release` | `just release-check`, the shipping binary under the release profile and the release dependency policy, on Linux |

The models run one job each, the list read from the modules of their
integration tests, because all of them one after another take longer than a
hosted job's six hours on four cores; the checker explores depth first,
which holds the path it is on rather than a whole frontier, so each fits the
runner's 16 GB. Each job under Cargo keeps its dependencies' builds in
GitHub's cache between nights (the models' jobs share one, as do the
offline tests and the seeds), so a night compiles only amux's own crates.

Every job runs to its end whatever the others do, and every step of a kind
runs whichever failed before it; the job's last line names the kind and
what failed. A scheduled run that fails is mailed by GitHub. Run one
anywhere with `scripts/nightly.py KIND`, inside `nix develop` as the job
does, or start the workflow by hand (`gh workflow run nightly.yml`). A
release builds the shipping binary on all three platforms itself.

### The macOS jobs

Four jobs run what only a Mac can, side by side, each when the change
reaches it ([Which jobs a change runs](#which-jobs-a-change-runs)); they
share their steps through a YAML anchor and set what differs as `JOB_*`
variables: the iPhone app builds the
bridge for the simulator, the Mac app the bridge for the Mac, the tests the
workspace for this Mac, and the journeys the part of it the terminal
journeys start. The journeys take the whole worker and miss their waits
beside compiles, so in the tests job they ran last, after everything else
had built. Each selects Xcode 26.6, installs the pinned Rust toolchain with both
ARM iOS targets and tmux, restores its store and starts a NativeLink on
the runner (`scripts/nativelink.py macos`, `build/nativelink/macos.json5`).
Buck2 runs every build action itself (`AMUX_BUILD_HERE=1`,
`scripts/buck-config.py`), looking it up in the cache first and uploading
what it made, C and assembly included (`third_party_cxx_library` in
`build/third_party.bzl`); NativeLink's worker runs the tests, so their
results are cached too. What Buck2 builds itself is keyed apart from the
same action on a worker (`built_in` in `build/platforms/defs.bzl`): outside
a sandbox it sees all of `buck-out`, so its result must never answer for a
sandboxed one. The store, every output and every result, is on the runner's
disk, and the job keeps it in GitHub's cache between runs
(`macos-<job>-store-*`), restoring the newest any run of that job kept.
A push therefore rebuilds and reruns only what its change reaches. Each
job's CAS holds at most 6 GiB, the tests job's 12 (its `JOB_CAS_GIB`;
compressed, a store is about a fifth of that), so the four stores and the
other jobs' caches fit in the 10 GB GitHub keeps for the repository; past
the cap the least recently used outputs go, and an action whose outputs
went runs again. After saving, a job deletes its older stores
(`scripts/ci_cache.py`), and the documentation tests job its older Rust
caches: GitHub evicts the least recently used entry past 10 GB, and an
older store, just read by the run that restored it, could otherwise
outlive another job's newer cache.

The macOS jobs don't use the build host. It can't run a Mac's actions,
and it is about 100 ms from GitHub's runners: when the runners' CAS was in
front of it, staging an action's inputs took about 4 s against a fraction
of a second of work, and uploading a 190 MB test binary to it took five
minutes, which the test waited for.

Both app jobs build in `dev`, the bridge unoptimised (`--target-platforms
…-app-shipping-dev`, which `bridge()` in `apps/apple/defs.bzl` follows),
where a developer's builds use `phone-dev`: the bridge's Rust then shares
the development workspace's third-party graph, and a cold build of it
takes about half as long. The debug tools are in both. Release builds are
unchanged.

The iPhone app job first builds every target under `apps/apple` but the
Mac app's, and only then makes and boots the golden simulator (`just ios
simulator golden`) and runs the phone's suites on it. A booted simulator
is some 240 processes holding about 3 GB of the runner's 7 GB, with a load
of 150 to 350 on its three cores: compiling beside it took the job to 21
minutes after a protocol change, and in its first minutes stalled every
process on the runner for 20 s at a time, long enough for NativeLink to
give up on reading a blob from its own store (a 30 s limit fixed in
NativeLink) and fail the build. With the device made, NativeLink starts
again so its worker hands every test that device's udid
(`AMUX_SIMULATOR_UDID`, `scripts/nativelink.py`), outside the action's key;
the tests take it in turns, under a lock ([the iPhone app](IOS.md#builds)).
A test that looked the device up itself asked CoreSimulator while the
runner compiled on all three cores, and `simctl list` took over three
minutes; xcodebuild, handed the udid, can still give up on finding the
device then, and is asked again when it does. The Mac app job builds the
Mac app, its storybook and screens and runs the Mac app's tests in one
`buck2 test --build-default-info` over its targets, so a suite starts as
soon as what it needs is built: the Mac app's bridge and the development
workspace its tests start are two chains of about a quarter of an hour
each on the
runner, and one after the other they were half an hour. The
Release app for a phone is not built here: its bridge is a fat-LTO link of
the whole Rust graph for one more target, and it reruns on every Rust
change; `just ios release` builds and audits it on the Mac. The run step is bounded
at 75 minutes inside a 100-minute job, so a cold run that runs out of time
still saves what it finished and the next run picks up from there, as a
cancelled one does: NativeLink is stopped before the store is archived,
since it may still be writing to it. A Buck2 command that failed only
because NativeLink gave up on reading or writing a blob in that time (the
burst of uploads as the tests job's eighty tests start has held a write
past it too) runs once more
(`scripts/rerun-on-cache-timeout.sh`), taking what the first finished from
the cache; any other failure stands. Spotlight is turned off on the
runners, which otherwise indexed every output Buck2 wrote. Buck2's event
logs, NativeLink's log and what the runner did every ten seconds (load,
memory, swap and the busiest processes, `scripts/ci-sample-system.sh`) are
kept as the run's `buck2-logs-*` artifacts for a week (`scripts/buck2 log
what-ran`, `log critical-path`). A run started by hand with `cold` set
starts the macOS jobs with no kept store, to time them from nothing.
The macOS tests job builds every test binary first, then runs `just test
//crates/...` but the journeys: beside the compiles of a protocol change,
on three cores, the system suites crawled and failed on timing. On macOS that is the
system lane as well, since the built `amux` binary's suites (`process`,
`supervise_cli`, `overlap`, `attach` and the rest), node's `supervisor`
suite and the `survive-daemon` system journey are ordinary Buck2 tests. The `linux-only` tests are the Linux gate's and are
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
restores the iPhone app job's store read-only, so the app it already
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
| `fast` | Offline, credential-free tests with no wall-time settling: unit tests, spec suites, adapter replays, store and effect integration, renderer goldens | `just test` in the Linux gate and the macOS tests job, and `just cargo-test` on Windows, on every push; the Apple suites in the iPhone app and Mac app jobs |
| `system` | Real processes and transports with bounded deadlines: the built `amux` binary and its supervisor, the store on iOS | The desktop suites in the Linux gate and the macOS tests job on every push; `test-store-ios` in `just ios captures` |
| `tool` | The repository's own machinery: script contracts, source and dependency policy, the CI tooling, the shipping scope audit | The Rust side, the scripts' tests and the phone apps' lints, all Buck2 tests, in `just premerge` (`just script-tests`, `ios script-tests`, `ios lint`, `android script-tests` and `android lint` run each set alone), so in the Linux gate on every push; `ios scope-audit` on every app release's signed bundle |
| `journey` | Complete client stories through the shared manifest, and the phone's accessibility sweep | The terminal journeys in the Linux gate and the macOS tests job, and the system journey inside `just test`, on every push; phone journeys and the accessibility sweep in iOS captures run by hand |
| `qualification` | Real providers, real cloud accounts and measured performance | Never on an ordinary push: `just live`, `just perf` and the `just ios qa-*` recipes by hand, on machines that have the credentials or hardware; `just ios perf` by hand on the enrolled Mac, last in `just ios verify` |
| `nightly` | Searches for faults rarer than a change should wait on: the protocol models | The Nightly workflow, every night on `main` ([Nightlies](#nightlies)); never on a push, a landing or `just premerge` |

[Testing](TESTING.md) defines the lanes and boundaries fully.

## `just premerge`

`just premerge` is what a change passes on the machine it was written on
before it reaches main, and what `just land` runs on the merge before
pushing it. It runs every check below, the iOS and Android writing lints
and script tests, every terminal journey, the browser extension's checks and
tests, and every workspace test.

Nearly all of it is one `just test` through Buck2 and its cache
([Building](BUILD.md#the-build-cache), [Repository
checks](BUILD.md#repository-checks)): the tests, the journeys, the browser
extension's checks and its npm audit, the repository's checks, deny among
them, the scripts' unit tests and the phone apps' lints. Each runs only when
something it reads changed since it last passed, in this tree or any other on
the machine. The few checks that stay scripts, because they read the whole
workspace or query the build graph itself (`fmt-check`, `dependency-policy`,
`shear`, the tokio half of `cores-check`, `tests-check`, `parity`,
`shipping-check` and `ci-jobs-check`), Clippy, and the build of every Windows
binary (`check-windows`), each a Buck2 build of its own, run at the same time
as the tests ([`build/checks/premerge.py`](../build/checks/premerge.py)); each
one's output is kept and printed at the end if it fails. With nothing
changed, premerge takes a few seconds; a change to a leaf crate or a document
reruns a handful of tests; a change to a crate most of the workspace depends
on, such as `wire`, reruns most of the suite, about a quarter of an hour on a
laptop.

It leaves out the Apple apps: any change to a core crate rebuilds the
bridge for three platforms and reruns the simulator suites, which is not a
load every premerge on a laptop should carry. Whoever changes Apple code
runs `just ios verify` and `just mac test`, on the same cache, and CI's
iPhone app and Mac app jobs run them after every push. It leaves out the
protocol models, the C ABI's pointer checks, the sanitizers and the
release build too, which run in the nightly ([Nightlies](#nightlies)), and
Windows' tests, which run only in CI. It does build every Windows binary, on
the build host's Linux worker with nothing fetched back: CI runs Windows'
tests only for changes that reach what only Windows tests, so the landing
gate is where a change that stops Windows compiling is caught. With no Linux
worker in reach it skips that build and says so.

The checks, by the recipe that runs each alone:

| Recipe | Checks |
| --- | --- |
| `no-update-flags` | No environment variable starting with `UPDATE_` is set, so no fixture is rewritten during an asserted run; first, before anything else starts |
| `lint` | Clippy over every library, binary and test of the workspace, warnings denied, with the workspace's `clippy.toml` ([`build/checks/clippy.py`](../build/checks/clippy.py)) |
| `fmt-check` | Formatting, with the pinned nightly |
| `codegen-check` | The committed protobuf output matches the schemas (`//:codegen-check`) |
| `proto-compat` | The schemas pass buf's lint, and `amux.v1` still reads what the latest release from 0.11.0 wrote; with a ref, what that ref wrote ([`buf.yaml`](../buf.yaml)) |
| `store-check` | Every store migration the latest release from 0.11.0 shipped is unchanged, and the ones added since only add: no table or column is gone or changed type, and any new column is one an existing store can take; with a ref, against that ref ([`scripts/store-check.py`](../scripts/store-check.py)) |
| `dependency-policy` | Production and test-infrastructure dependency boundaries ([`scripts/check-dependency-policy.py`](../scripts/check-dependency-policy.py)) |
| `deny` | No dependency has an open security advisory or is a yanked release, every licence is one [`deny.toml`](../deny.toml) allows, and every crate comes from crates.io or the rustls patch; each advisory it sets aside says why it does not reach amux. A Buck2 test (`//:deny`) on the day's snapshots of the advisory database and crates.io's index, run on a Linux worker ([Repository checks](BUILD.md#repository-checks)) |
| `shear` | Every dependency a manifest lists is used by that crate's code; one only the code cannot show, such as a crate named only to pass on a feature, is listed in that crate's manifest as one to skip, with why |
| `cores-check` | No state-machine core (`daemon-core`, `relay-core`, `agent-core`, `supervisor-core`) nor the `core-ring` they record inputs in has tokio in its dependency graph on any target, and none of their sources holds a lock, spawns a task or awaits ([`scripts/cores-check.py`](../scripts/cores-check.py)) |
| `typed-provider-check` | The interpreters, the agent's provider handshake and Claude's messaging socket read and write provider messages only through the protocol crates: no `json!`, field looked up by name or untyped `Value` outside the listed tool payloads ([`scripts/typed-provider-check.py`](../scripts/typed-provider-check.py)) |
| `client-kind-check` | The client crates (ui-state, ui-view, ui-runtime, tui, app-runtime, browser-core) decide nothing by an agent's `Kind` or by a provider id written out (Claude's, Codex's or a built-in ACP agent's, read from `wire::agents::BUILT_INS`), outside the listed places, each with its reason; a listed place that matches nothing fails too ([`scripts/client-kind-check.py`](../scripts/client-kind-check.py)) |
| `docs-check` | Every link, image and heading anchor under `docs/` resolves, every figure is referenced and standalone, and `docs/README.md` lists every page once ([`scripts/docs-check.py`](../scripts/docs-check.py)) |
| `tests-check` | The suite catalogue names only things that exist |
| `script-tests` | The unit tests of the repository's own scripts, one Buck2 test per test file |
| `browser audit` | No known vulnerability in the npm packages the browser extension ships, as the npm registry says that day (`//js:audit`) |
| `shipping-check` | No crate that exists for tests and no test feature is in the desktop binary's or the browser extension's graph as they ship ([`build/checks/shipping.py`](../build/checks/shipping.py)) |
| `parity` | Every crate's `BUCK` file and `Cargo.toml` agree on its targets and dependencies ([`build/checks/parity.py`](../build/checks/parity.py)) |
| `check-windows` | Every workspace library, binary and test compiles and links for Windows, on a Linux worker with the Windows toolchain (`--materializations none`: nothing comes back); skipped, and named, with no Linux worker in reach |
| `ci-jobs-check` | What the Apple apps' build graphs read that no Linux target compiles is inside their territory in [`build/ci-jobs.toml`](../build/ci-jobs.toml) ([Which jobs a change runs](#which-jobs-a-change-runs)); on a Mac only |
| `test` | Every workspace test, through Buck2: only the tests whose inputs changed since a passing run, in any tree on the machine, run again |

## Checking Windows from a Mac

`just check-windows` compiles and links every workspace target for
`x86_64-pc-windows-msvc` on the build host's Linux worker, with the Windows
toolchain the Windows job's binaries are built with ([Windows
builds](BUILD.md#windows-builds)): a Windows-only compile or link error
shows up there before CI. It finds build errors only; Windows runtime
behaviour still needs a Windows machine, which the Windows job's runner is.
