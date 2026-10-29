# Continuous integration

*For developers who need to know what CI runs on a push, how to run the same checks locally, and how to check Windows from a Mac.*

CI is GitHub Actions. Every job installs the pinned toolchain and `just`, then
runs `just` recipes — the same recipes a developer runs locally, so a job's
log names the command to reproduce it. [Build](BUILD.md) describes the
recipes; [Testing](TESTING.md) describes the suites they run.

## Workflows

| Workflow | File | When | What |
| --- | --- | --- | --- |
| CI | [`ci.yml`](../.github/workflows/ci.yml) | Pushes to `main`; pull requests into `main` | Every check a change is held to, on Linux, macOS and Windows |
| iOS gate | [`ios.yml`](../.github/workflows/ios.yml) | Pushes to `main` and pull requests into `main` that touch the phone's paths, and by hand | `just ios gate` on Xcode 26.6 |
| Weekly offline tests | [`offline.yml`](../.github/workflows/offline.yml) | Sundays 04:00 UTC (GitHub often starts it hours late), and by hand | The workspace tests with no external network |
| iOS captures | [`ios-captures.yml`](../.github/workflows/ios-captures.yml) | Nightly 03:00 UTC (GitHub often starts it hours late), and by hand | The phone's component snapshots nightly; journeys and the accessibility audit by hand |
| Models at nightly bounds | [`models-nightly.yml`](../.github/workflows/models-nightly.yml) | Daily 02:00 UTC (GitHub often starts it hours late), and by hand | Every protocol model at its nightly bounds, one Linux job per model, and every simulated testnet story under 2000 seeds |
| Release | [`release.yml`](../.github/workflows/release.yml) | A pushed `v*` tag | The `amux` release binaries; see [Release](RELEASE.md) |

Every workflow sets `CARGO_INCREMENTAL=0`, and every job has a
`timeout-minutes` bound.

### CI jobs

| Job | Runner | Runs |
| --- | --- | --- |
| Check and Clippy | `ubuntu-latest` | `just check`, `just lint` |
| Format | `ubuntu-latest` | `just fmt-check` with `nightly-2026-08-30` |
| Codegen and dependency policy | `ubuntu-latest` | `just codegen-check`, `just dependency-policy`, `just deny`, `just shear`, `just cores-check`, `just typed-provider-check`, `just tests-check`, `just script-tests`, `just docs-check`, `just proto-compat` |
| Test | `ubuntu-latest`, `macos-latest`, `windows-latest` | `scripts/no-update-flags.sh`, `just test -- --no-fail-fast --exclude models` |
| Simulated stories, doctests and contracts | `ubuntu-latest` | `just sim`, `just contracts-check`, `just doctest`: what reads the same on every platform, run once |
| Models | `ubuntu-latest`, one job per protocol-models test target | `just test-crate models -- --test <target>`: one model at its CI bounds |
| Build | `ubuntu-latest`, `macos-latest`, `windows-latest` | `just release-check` |
| Terminal journeys | `ubuntu-latest`, `macos-latest` | Installs `tmux`, then `just journey terminal all`: every terminal story in the manifest |
| Embedded client | `ubuntu-latest` | `just embedded-check`, `just embedded-test` |
| Pointer checks over the C ABI | `ubuntu-latest` | `just miri`: the phone's C ABI tests under the pinned nightly's interpreter, which fails on any undefined behaviour in a pointer handed across the boundary, read or freed; the tests that start an installation or a tokio runtime are ignored there, each saying why |
| Sanitizers | `ubuntu-latest`, one job for each of `address` and `thread` | `just sanitize <kind>`: the tests that reach unsafe code and C, built on the pinned nightly with the standard library under the sanitizer; `address` fails on an out-of-bounds access, a use after free or a double free, `thread` on a data race |
| iOS target check | `macos-latest` | `just mobile-check` |

The test job runs with `--no-fail-fast`, so a platform reports every failing
test binary, not only the first. Its matrix does not fail fast either: a
failure on one operating system does not cancel the others. The Windows
runner compiles and runs every test target; suites that need Unix processes,
PTYs or sockets are compiled out there. Among them are the tests that host
terminal Claude: the agent's terminal cases (in its `attach`, `dump`,
`providers`, `tools` and `lifecycle` suites), provider-fakes' `pty` suite,
the terminal modules of its `conformance` and `scripted` suites, and the
terminal client's served `frames` suite, whose hosts run terminal Claude. Windows does not host
terminal Claude in this build (see
[Windows, as a stated cost](ARCHITECTURE.md#windows-as-a-stated-cost)), so
the Windows job runs the agent's test of that refusal instead, and every
headless Claude and Codex test as everywhere. On Linux and macOS the same run is the
system lane as well: the built `amux` binary's suites (`process`,
`supervise_cli`, `overlap`, `attach` and the rest), node's `supervisor` suite
and the `survive-daemon` system journey are ordinary test targets.
`contracts-check` runs on Linux after `just sim` has built what the
workspace test run builds: the contracts name Unix-only tests, which Windows
does not build, and the one test of a macOS facility is built on every Unix
and ignored off macOS, so the listing names it there too. It lists the
binaries of that build (the same Cargo arguments), so it compiles nothing
more.

The terminal journeys are `reach-host`,
`conversation-decision-claude-pty`, `conversation-decision-claude-sdk`,
`conversation-decision-codex`, `leave-and-recover`, `manage-agent`,
`attachment-or-review` and `keep-authority`.

### Bounds

Two kinds of bound stop a run, and neither is ever raised to hide a failing
or slow test.

- **A recipe bound** is a hang detector for one phase, sized above that
  phase's honest duration. `just test` runs two phases, each under its own
  bound ([`scripts/workspace-test.sh`](../scripts/workspace-test.sh)): compiling
  every test target (`cargo test --no-run`, 1200 s, the bound `just test-build`
  carries), then running them (1000 s). The run's bound is the slowest
  platform's measured clean run — one where no failing test waits out its own
  timeout — times 1.5, rounded up to the hundred.
- **The job guard** (`timeout-minutes`) bounds the honest total of a job:
  setup, cache restore, every step. The test job's is 30 minutes.

What the bounds were sized from, on a warm cache:

| Platform | Compile | Test run | Whole job |
| --- | --- | --- | --- |
| `ubuntu-latest` | 145 s | 601 s | 13 min 6 s |
| `macos-latest` | 95 s | 449 s | 13 min 13 s |
| `windows-latest` | 239 s | 286 s | 11 min 41 s |

Linux is the slowest clean test run: 601 s × 1.5 is 902 s, so the run's
bound is 1000 s. Every job ends near 13 minutes, well inside the 30-minute
guard. Windows
compiles out most of the slow suites (the embedded client, the supervisor,
the system journeys, terminal Claude), so its clean run is under half of
Linux's and does not move the bound. A cold cache, after a `Cargo.lock`
change, compiles the whole dependency graph and can take longer than these
numbers.

Tests that build `amux` or the fake providers mid-run start cargo through
`provider_fakes::cargo::command()`, which drops the variables cargo set for
the package under test. Started with them, every test crate's build reran
`ring`'s build script and recompiled everything above it, about a minute a
crate on the macOS runner; that pushed the macOS run from 449 s to past its
1000 s bound.

### Protocol models

The protocol models ([`crates/models`](../crates/models/README.md)) explore
every state of the real cores within their bounds, a minute or more of
every core each, so they leave the three Test jobs (`--exclude models`;
`just test` itself still runs them) for the Models job: one Linux job per
protocol-models test target, side by side, each building the models crate
and running one model at its CI bounds on the runner's four cores. `just
tests-check` fails when a protocol-models test target is missing from the
job's matrix. Linux minutes cost a public repository nothing, and each job
stays far inside the run's wall time, which the iOS gate sets.

A model's bounds are sized to that runner; its test asserts the state count
those bounds explore, not the time (a run that hangs is the job guard's).
A CI run
measured the models there at their CI bounds: the runner took 2.1 to 4.2 times
the enrolled Mac's four-thread time, and replication, the model heaviest on
the store, was the slowest (274 s against 66 s); a later run measured presence
at 4.7 times (339.9 s against 73 s). So a model's budget is at least twice its
runner time, rounded up to a whole minute, and no budget is above 600 s; a
model whose doubled runner time would pass that has its CI bounds cut; a model
not yet measured there is sized from its four-thread Mac time times 4.7. The models' README records each model's times. The job
guard is 25 minutes.

The Models at nightly bounds workflow runs each model's ignored nightly
test, the same matrix, daily and by hand; `just models-nightly` runs the
same tests locally, one test binary after another. It restores the Models
job's Rust cache and never saves its own. Its job guard is 100
minutes: the slowest model at its nightly bounds, the family model, takes
757 s on the Mac's 12 threads, and roughly seven times that on
the runner.

### Simulated stories

The testnet's stories that the test catalogue declares a simulated mode for
(`modes` in [`tests/catalog.toml`](../tests/catalog.toml)) run twice: on
the real net under `just test` in each Test job, then on the simulated net
under `just sim` in the Linux job beside them, every core in one process, a
few seeds per story. A failing seed is printed with the
`just sim --seed N <story>` that reruns it. The
nightly workflow's seeds job runs `just nightly --seeds 2000 --bounds none`:
2000 seeds per story and a report of every failing seed, without the models,
which its matrix already checks. Locally, `just nightly --seeds N` walks the
seeds and then runs `just models-nightly`.

### The iOS gate

The iOS gate is its own workflow so that it runs only when a push or pull
request touches a path that reaches the phone: `apps/apple`, the three
`app-*` crates, `ios-tools`, the Cargo manifests, the toolchain pin, the
phone's loopback topology, the shared runner scripts the gate's recipes call
(`scripts/bounded`, `py`, `python`, `with`, `no-update-flags.sh`,
`sqlite_linkage.py`), the `setup-just` action or `ios.yml` itself. The phone's
own scripts live in `apps/apple/Tools` and its Rust tools in `ios-tools`, so
they need no entry of their own; anything shared stays in `scripts/` or
`xtask` and leaves the gate alone. The root justfile is not on the list: the
gate reaches it only through `mod ios`, and a workspace test in `ios-tools`
fails on Linux when a root recipe the phone's verification names is renamed
or removed. The rest of the bridge's
Rust graph needs no entry: everything it hands Swift crosses as the
generated mirrors in `apps/apple/Packages/AmuxCore/Sources/AmuxValues`,
which a workspace test holds to the Rust definitions, so a change to their
shape cannot land without touching the app. The iOS target check still
compiles the bridge's graph on every push.

The iOS gate job selects Xcode 26.6, asserts that the iOS 26.5 simulator
runtime and the iPhone 17 Pro device type are available, installs XcodeGen,
the stable toolchain with both ARM iOS targets, and runs `just ios gate`: the
half of the phone's verification that building the app can settle.

`just ios gate` runs, in order and stopping at the first failure,
`just ios` `lint`, `script-tests`, `graph-check`, `rust`, `simulator golden`,
`test-build`, `component-snapshots --skip-build`, `loopback-smoke` and
`unit --skip-build`. The simulator boots while `test-build` compiles the app
and every unit and component suite once, for any simulator, and the gate
waits for both before the suites run what was built. `mobile-check` is left
to the iOS target check, which runs it on every push. The only pictures it
compares are the component snapshots, drawn in-process and flat, so it
answers the same on any machine. The component snapshot batch runs under its own bound, sized by the rule above
from the slowest clean CI run: 466 s with only the examples marked to settle
late waiting (371 to 429 s is usual), so 700 s, and 629 s in the nightly
captures run, where every picture waits a quiet second (`--settle-all`, also
used by every recording), so 1000 s. The job uploads the component snapshot
comparisons and the shipped-scope audit directory to the run, whether it
passed or not.

`just ios verify` is the whole sequence a developer runs before pushing a
phone change: the workspace's `fmt-check`, `lint` and `test` (which runs the spec suites), then the
gate, then the captures (`test-store-ios`, `ios journey`,
`ios accessibility`), then the
shipping stages (`ios package`, `ios scope-audit`). It refuses to start if
any stage names a recipe the justfiles no longer declare. `just ios captures`
and `just ios shipping` run those halves alone.

### iOS captures

The nightly workflow boots the golden simulator on `macos-26` and compares the
native component snapshots with every example waiting to stop changing
(`--settle-all`). Nothing it runs uses the small simulator, which took seven minutes to boot. It
restores the iOS gate's Rust cache and never saves its own: a cache keyed to
the nightly job was saved only after a green run and evicted before the next
one, so every night built the Rust bridge and tools from nothing. Run by
hand, it also runs `just ios captures`: the store suite on the simulator,
journeys and the accessibility sweep. It uploads the journey evidence, the
audit's record and the component snapshot comparisons. [The iPhone app](IOS.md) explains what those
compare.

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
| `fast` | Offline, credential-free tests with no wall-time settling: unit tests, spec suites, adapter replays, store and effect integration, renderer goldens | `just test` on every push, on all three platforms |
| `system` | Real processes and transports with bounded deadlines: the built `amux` binary and its supervisor, the phone bridge on a simulator, the store on iOS | The desktop suites in `just test` on every push (Unix runners); `ios loopback-smoke` in the iOS gate; `test-store-ios` in `just ios captures` |
| `tool` | The repository's own machinery: script contracts, source and dependency policy, the CI tooling, the shipping scope audit | The Rust side in `just test`; `just script-tests` on every push; `ios script-tests` and `ios lint` in the iOS gate; `ios scope-audit` before every app release |
| `journey` | Complete client stories through the shared manifest, and the phone's accessibility sweep | The terminal journeys job and the system journey inside `just test` on every push; phone journeys and the accessibility sweep in iOS captures run by hand |
| `qualification` | Real providers, real cloud accounts and measured performance | Never on an ordinary push: `just live`, `just perf` and the `just ios qa-*` recipes by hand, on machines that have the credentials or hardware; `just ios perf` by hand on the enrolled Mac, last in `just ios verify` |

[Testing](TESTING.md) defines the lanes and boundaries fully.

## `just ci`

`just ci` runs the check sequence the CI jobs are built from, in one process,
stopping at the first failure:

| Recipe | Checks |
| --- | --- |
| `no-update-flags` | No environment variable starting with `UPDATE_` is set, so no fixture is rewritten during an asserted run |
| `check` | The workspace compiles |
| `lint` | Clippy, warnings denied |
| `fmt-check` | Formatting, with the pinned nightly |
| `codegen-check` | The committed protobuf output matches the schemas |
| `proto-compat` | `amux.v1` still reads what the latest release from 0.9.0 wrote; with a ref, what that ref wrote |
| `dependency-policy` | Production and test-infrastructure dependency boundaries ([`scripts/check-dependency-policy.py`](../scripts/check-dependency-policy.py)) |
| `deny` | No dependency has an open security advisory or is a yanked release, every licence is one [`deny.toml`](../deny.toml) allows, and every crate comes from crates.io or the rustls patch; each advisory it sets aside says why it does not reach amux |
| `shear` | Every dependency a manifest lists is used by that crate's code; one only the code cannot show, such as a crate named only to pass on a feature, is listed in that crate's manifest as one to skip, with why |
| `cores-check` | No state-machine core (`daemon-core`, `relay-core`, `agent-core`, `supervisor-core`) nor the `core-ring` they record inputs in has tokio in its dependency graph on any target, and none of their sources holds a lock, spawns a task or awaits ([`scripts/cores-check.py`](../scripts/cores-check.py)) |
| `typed-provider-check` | The interpreters, the agent's provider handshake and Claude's messaging socket read and write provider messages only through the protocol crates: no `json!`, field looked up by name or untyped `Value` outside the listed tool payloads ([`scripts/typed-provider-check.py`](../scripts/typed-provider-check.py)) |
| `docs-check` | Every link, image and heading anchor under `docs/` resolves, every figure is referenced and standalone, and `docs/README.md` lists every page once ([`scripts/docs-check.py`](../scripts/docs-check.py)) |
| `tests-check` | The suite catalogue names only things that exist |
| `test` | Every workspace test target |
| `contracts-check` | Every design contract in [`tests/contracts.toml`](../tests/contracts.toml) names at least one test, and every test and model property it names exists |
| `doctest` | Documentation tests |
| `release-check` | The shipping binary builds and passes the release policy check |
| `embedded-check`, `embedded-test` | The provider-free client graph and the embedded owner and client boundary |
| `mobile-check` | The provider-free graph for iOS devices and simulators |

The terminal journeys are in `ci.yml` but not in `just ci`, and neither is the iOS gate.

## Checking Windows from a Mac

The Windows test job compiles every workspace target for
`x86_64-pc-windows-msvc`, and a Windows-only compile error otherwise shows up
only there. Type-checking for Windows from a Mac needs no Windows toolchain,
but it does need help: `ring`, `libsqlite3-sys` and `zstd-sys` compile C in their build
scripts, and there is no MSVC compiler to run. Type-checking Rust needs no
real object code, so two stub tools that create the files they are asked for
are enough. There is no recipe for this; set it up once:

```sh
mkdir -p /tmp/winstub

cat > /tmp/winstub/cc <<'EOF'
#!/bin/sh
# Stand-in C compiler: create the object file named by -o or -Fo.
out=""
prev=""
for a in "$@"; do
  case "$prev" in -o) out="$a";; esac
  case "$a" in -Fo*) out="${a#-Fo}";; esac
  prev="$a"
done
[ -n "$out" ] && : > "$out"
exit 0
EOF

cat > /tmp/winstub/ar <<'EOF'
#!/bin/sh
# Stand-in archiver: create the library named by -out:.
for a in "$@"; do
  case "$a" in -out:*) : > "${a#-out:}";; esac
done
exit 0
EOF

chmod +x /tmp/winstub/cc /tmp/winstub/ar
```

Then check one crate:

```sh
CC_x86_64_pc_windows_msvc=/tmp/winstub/cc \
CXX_x86_64_pc_windows_msvc=/tmp/winstub/cc \
AR_x86_64_pc_windows_msvc=/tmp/winstub/ar \
cargo check --locked -p <crate> --all-targets --target x86_64-pc-windows-msvc
```

or the whole workspace, as the Windows test job compiles it:

```sh
CC_x86_64_pc_windows_msvc=/tmp/winstub/cc \
CXX_x86_64_pc_windows_msvc=/tmp/winstub/cc \
AR_x86_64_pc_windows_msvc=/tmp/winstub/ar \
cargo check --locked --workspace --all-targets --features bundled \
  --target x86_64-pc-windows-msvc --keep-going
```

`--keep-going` reports every crate's errors instead of stopping at the first.
The target is already installed by `rust-toolchain.toml`. This finds compile
errors only; Windows runtime behaviour still needs the Windows runner.
