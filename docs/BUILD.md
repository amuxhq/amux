# Building amux

*For developers building amux from source: the toolchain, the build system and its cache, the build profiles, the task recipes and the generated code that is committed.*

## Toolchains

[`rust-toolchain.toml`](../rust-toolchain.toml) pins Rust 1.98.0 with the
`minimal` profile, the components repository tasks use (`clippy`,
`llvm-tools`, `rust-src`, `rustfmt`) and the cross-compilation targets:

| Target | Used for |
| --- | --- |
| `aarch64-apple-ios`, `aarch64-apple-ios-sim`, `x86_64-apple-ios` | The iPhone app's Rust bridge and `just mobile-check` |
| `aarch64-linux-android`, `x86_64-linux-android` | The Android app's Rust bridge and `just android check` |
| `x86_64-pc-windows-msvc` | The Windows release build and the Windows builds on a Linux worker ([Windows builds](#windows-builds)) |
| `x86_64-unknown-linux-gnu` | The Linux release build |
| `wasm32-unknown-unknown` | The browser extension's WebAssembly, `browser-core` ([Browser extension](BROWSER.md)) |

Formatting uses `nightly-2026-08-30` explicitly (`just fmt`, `just
fmt-check`), so a stable-toolchain update cannot rewrite the tree
unexpectedly. Install that nightly with `rustfmt` once, or use the Nix dev
shell below, which has it.

Other tools a full checkout uses:

- [`just`](https://github.com/casey/just) runs every task.
- Python 3.11 or newer runs the repository scripts. `scripts/py` finds one
  even when `python3` on the PATH is the macOS system 3.9.
- Buck2 builds the Apple apps with Xcode's compilers and SDKs (see [the iPhone app](IOS.md#builds)).
- Buck2 builds the Android app on a Linux worker with its own SDK, NDK and JDK (see [the Android app](ANDROID.md), `just --list android`); a Mac needs the Android SDK's emulator and platform tools only to run what the build host built.
- `tmux` drives the terminal journeys.
- Node 24 or newer, with npm, builds the browser extension under `js/`
  (`just browser build`). Biome formats and lints it (`just browser fmt`),
  and `js/.npmrc` stops packages' install scripts from running. Its checks
  and tests (`just browser test`) bring their own Node, and their Chromium
  comes with the Linux toolchain.
- [`gitleaks`](https://github.com/gitleaks/gitleaks) scans what a push
  would publish for credentials. `just hooks` installs that scan as the
  pre-push hook, for every worktree at once; it reads `.gitleaks.toml`, and
  `.gitleaksignore` lists the test keys committed on purpose. The repository
  is public, so the scan runs before a push rather than in CI, which would
  only see a leak after it was published.

### The Nix dev shell

On a machine with [Nix](https://nixos.org/download/) and flakes enabled,
`nix develop` at the root opens a shell with everything above except Xcode and
the Android SDK:
the toolchain `rust-toolchain.toml` pins, the formatting nightly, `just`,
`tmux`, Python, `git`, `gitleaks`, JDK 21 and `cargo-ndk`, the dependency checkers `just deny` and
`just shear` run, the pinned `buck2`, `reindeer` and `nativelink`
(`build/tools.json`) and, on Linux, a C compiler. `nix develop -c just test`
runs one recipe in it; on Linux, where there is no rustup, that is how
Buck2 finds the pinned Rust. [`nix/toolchain.nix`](../nix/toolchain.nix) is the
list, and `flake.lock` pins every tool in it, so two machines on the same
revision get the same tools. `nix profile install .#toolchain` installs the
same list onto the PATH of every shell, for a machine that should always have
it.

The shell has no rustup, so it provides the nightly's cargo as
`cargo-nightly`, which `just fmt`, `just fmt-check`, `just miri` and `just sanitize` use
whenever it is on the PATH. Its date in `nix/toolchain.nix` has to move with the justfile's.

The workspace has one lockfile. Build and verification recipes pass
`--locked`, and protobuf output is committed, so the same revision selects
the same Rust dependencies and wire code on every machine. Protobuf
generation uses a vendored `protoc`, so none needs installing.

`just proto-compat` needs [buf](https://buf.build/docs/cli/installation/),
which `nix/toolchain.nix` provides; outside Nix, `brew install
bufbuild/buf/buf`. Its Buck2 test, `//:proto-compat`, brings its own.

## Buck2 and Cargo

[Buck2](https://buck2.build) builds and tests the workspace for every
`just` recipe a person or agent types. Cargo still builds the same crates
from the same `Cargo.toml` files for what Buck2 doesn't do: CI's Windows
job when it has no build host, a fork's pull request (through `just
cargo-test`), rust-analyzer, `just miri`, `just sanitize`, `just
fuzz-smoke` and `just doctest`, and the nightlies. The Apple apps build
with Buck2 ([the iPhone app](IOS.md#builds)), and so do the command-line
release binaries ([Release](RELEASE.md)). CI's Linux gate and Windows job
run through Buck2 on the build host ([CI](CI.md#the-linux-gate)).

Each crate describes itself twice, so the two stay in step by hand:

- A crate's `BUCK` file, beside its `Cargo.toml`, declares its library,
  binaries and tests with the macros in
  [`build/rust.bzl`](../build/rust.bzl). `just parity` fails when a crate's
  `BUCK` and `Cargo.toml` disagree on its targets or dependencies; `just
  premerge` runs it.
- [`third-party/rust/BUCK`](../third-party/rust/BUCK) is generated from
  `Cargo.toml` and `Cargo.lock` by reindeer: run `just buckify` after a
  dependency changes. reindeer reads no `[dev-dependencies]`, so a crate
  only tests use is also listed in `third-party/rust/dev-only`, a workspace
  member with no code. A crate whose build script reindeer can't run gets a
  fixup under `third-party/rust/fixups/`. Each library there is built
  through [`build/third_party.bzl`](../build/third_party.bzl), which keeps
  a crate's sources out of every dependent's compile: the prelude would
  otherwise stage the sources of all of a crate's dependencies on the
  worker for each compile, 11,000 files for a crate near the top.
- A test finds its crate's files and binaries through the
  [`source-tree`](../crates/source-tree) crate (`crate_dir!()`, `bin!()`)
  rather than `env!("CARGO_MANIFEST_DIR")` and `env!("CARGO_BIN_EXE_…")`,
  which Buck2 sets to paths inside the build's sandbox. Under Cargo both
  macros give what those variables give.

### Configurations

A target is built for a target platform named
`<target>-<product>-<purpose>-<mode>` ([`build/platforms`](../build/platforms)):
`macos-cli-shipping-dev` is a development build of the desktop product for
this Mac. The purpose is what keeps test code out of what ships:

- **shipping** builds a crate with only the features its product has.
- **testing** turns on the test features (`release/test-key`,
  `store/explore`, `tui/fixtures` and the rest). Every test builds this
  way, and so do the crates that only tests and tools use (`testnet`,
  `provider-fakes`, `models`, `qualification`, `shot`, `replay-support`):
  nothing shipping can depend on them.

Product and purpose decide only the workspace's own crates' features, so a
third-party crate is always built as `<target>-cli-shipping-<mode>`,
whichever configuration reaches it, build tools included
([`build/third_party.bzl`](../build/third_party.bzl)): each is compiled
once per target and mode. A fixup can't make a third-party crate differ by
product or purpose.

The mode is the profile: `dev`, `release`, `full-debug`, `phone-dev`,
`wasm` and `models`, with the flags the profiles below give Cargo, set in
[`build/toolchains`](../build/toolchains). `--target-platforms` picks
another configuration: the browser extension's WebAssembly is
`//crates/browser-core:browser-core[shared]` built for
`wasm-app-shipping-wasm`.

### The build cache

Buck2 sends every compile, link and test to
[NativeLink](https://github.com/TraceMachina/nativelink), which keeps each
result keyed by its inputs, so a result is computed once and every
worktree on the machine reuses it. On a Mac it runs as a login agent on
`127.0.0.1:50051` with a worker that leaves three cores to the rest of the
machine:

```sh
scripts/nativelink.py install      # once, and after its version changes
scripts/nativelink.py status
```

Two macOS settings make it usable, both once per machine and again after an
upgrade to NativeLink changes its binary:

- **Developer Tools.** Gatekeeper scans every newly linked binary before it
  first runs, which turns each test binary's first start into seconds and,
  under a full build, makes NativeLink's own uploads time out. Add the
  `nativelink` binary (`~/.cache/amux/tools/nativelink/<version>/`) under
  System Settings → Privacy & Security → Developer Tools, then run
  `scripts/nativelink.py install` again so the running process gets the
  exemption. What it starts inherits it.
- **Local Network.** The first test that opens a local socket other than
  loopback makes macOS ask whether `nativelink` may find devices on the
  local network, since the tests run as its children. Allow it: the
  discovery and pairing tests need it.

A test result is cached too: a passing test is not run again until
something it reads changes. Each test runs in a process of its own under
[`build/test-runner.py`](../build/test-runner.py), with a `HOME` and
`TMPDIR` of its own, made empty for it, and the bounds
`.config/nextest.toml` gives nextest: reported slow after a minute and
ended after three. A test that leaves a process running behind it fails.
Tests labelled `live` (the live provider lanes) are never cached and never
run by `just test`.

A compile that fails is never kept. The prelude builds a library's
`[check]` output and its `[diag.*]` and `[clippy.*]` subtargets in a mode
where rustc's failure still exits 0, its errors recorded for a later step
that fails the build; a worker stores anything that exits 0, so a compile
that failed because the machine did (a full disk) was replayed from the
cache until its crate changed. Every prelude tool written in Python runs
through [`build/toolchains/bootstrap-guard.py`](../build/toolchains/bootstrap-guard.py),
which fails such a compile where it ran instead, with rustc's errors in its
output, so it runs again next time.

A golden test writes its file into the sandbox, where nothing reaches the
checkout, so `just goldens` re-records them instead: it runs the golden
tests here, with every update switch on. The next `just test` reruns only
the tests that read what changed.

Build output is under `buck-out/`, which holds only what was asked for:
everything else stays in the cache until something needs it. `scripts/buck2
clean` empties it without losing the cache. Buck2 never removes an output
from `buck-out/` by itself, so it grows with every build; `scripts/buck2
clean --stale 1d` removes what no build used in the last day, through the
running daemon, which stays warm. The landing queue trims its tree that way
every six hours.

### Linux builds and the build host

Linux targets build and run on a Linux worker: a build host's NativeLink,
or the one on a Linux machine itself. Their toolchain is
`packages.actions` in [`flake.nix`](../flake.nix), the same list as the dev
shell less the build tools, and an action on the Linux worker realizes it
(`//:nix-linux`, [`build/toolchains/nix.bzl`](../build/toolchains/nix.bzl)):
every Linux compile, link and test calls its tools by that store path, so a
change to the flake changes every Linux key, and a branch that changes the
toolchain gets it built on the worker from whichever machine sent it. A Mac
needs no Nix for this.

A Mac sends its Linux actions to the build host through its own
NativeLink, once set up with the host's address and the Mac's address on
the tailnet, where the host's worker registers:

```sh
scripts/nativelink.py install --remote grpc://<host>:50051 --workers <this Mac's tailnet address>:50062
```

The build host's own setup names both. The Mac's cache then sits in front
of the host's: what Buck2 uploads reaches the host too, and a result the
host already has is a hit on the Mac, so a branch tested on the host is
cache hits for the same configuration on the Mac and the other way round.
macOS results stay on the Mac.

An output built on the host stays there until the Mac needs it, so a Linux
test sent from the Mac brings back only its result. `buck2 build` is
different: it fetches every output it was asked for into `buck-out`. A
Linux test binary is 150–240 MB, and the tailnet carries about 10 MB/s, so
building the Linux test binaries above `daemon-core` from the Mac fetched
2.5–2.7 GiB and took 365–414 s, where the build itself took 124–186 s;
while the download ran, every other call to the host queued behind it.
With `--materializations none`, which `just check-windows` passes for the
same reason, the same build took 128 s and fetched 2 MiB. The host builds it
in 85 s. Pass it whenever the Mac won't run what it builds.

Asked whether it holds a blob, the Mac's cache asks the host, then its own
layer for what the host lacks. Buck2 asks before it uploads an action's
inputs, and again as an output it refers to nears the end of the three
hours it assumes one lives; answered by the host alone, the outputs the
Mac's own worker built were called gone, and a daemon up for hours failed
the next action that needed one ("an artifact that has expired in the RE
CAS"). Counting the Mac's layer means Buck2 never uploads a blob only the
Mac holds: an output built on the Mac, one the host has evicted while the
Mac kept it, one uploaded while the host was out of reach. So the host's
worker reads such a blob from the Mac when an action of the Mac's needs
it, over the port it registers at, and writes it into the host's cache as
it passes (the build host's worker configuration does this, one store per
Mac it serves). A Linux target can take an output built on the Mac.

NativeLink has no store that copies a blob to the host when it is asked
whether it holds one, so the copy happens when the host's worker reads it
instead. Uploading everything the Mac builds to the host as well would
also close the gap for the Mac's own outputs, but not for what the host
evicts, and at the price of sending every Mac build over the tailnet.

When the host doesn't answer (or the tailnet is down), `scripts/buck2`
says so, restarts NativeLink without it and turns Linux off for the tree:
Linux actions fail at once rather than wait, and tests marked `linux_only`
(the model checks' integration target, which takes every core for ten
minutes) are skipped, which `just test` names at the end. When the host
answers again it switches back. Each switch restarts NativeLink, which
interrupts any build another tree has running at that moment. What the
Mac's layer gained in between never reached the host; the host's worker
reads it from the Mac when a Linux action needs it.

A Linux machine sends its actions to the NativeLink on its own loopback,
as the build host's trees do, or to the one `AMUX_NATIVELINK` names
(`grpc://<host>:50051`): that is how CI's Linux gate reaches the build
host over the tailnet. `scripts/nativelink.py serve DIR` runs a NativeLink
with its own worker and no remote in the foreground, for a Linux machine
that has neither (a fork's pull request in CI).

### Windows builds

Everything built for Windows compiles and links on a Linux worker, from
any machine, for x64 (`x86_64-pc-windows-msvc`, what releases ship and CI
tests; the `windows` target platforms) or for Arm64
(`aarch64-pc-windows-msvc`, Windows on Arm; `windows-arm64`): Rust's own Windows target, LLVM's
MSVC-compatible C compiler and linker (`clang-cl`, `lld-link`,
`llvm-lib`), and Microsoft's C runtime and Windows SDK, which nixpkgs
fetches with `xwin` from the Visual Studio release it pins, under
Microsoft's licence. They are `packages.x86_64-linux.windows` in
[`flake.nix`](../flake.nix) ([`nix/windows.nix`](../nix/windows.nix)),
realized on the worker like the Linux toolchain (`//:nix-windows`), and
apart from it, so an SDK update changes only Windows keys. Each
architecture has its own compiler and linker wrappers over its own CRT and
SDK (Arm64's under `arm64/bin`), beside one Rust that carries both. A Windows
machine only runs what it is given: the binaries a Windows test starts are
built for Windows (`amux_binaries` in [`build/rust.bzl`](../build/rust.bzl)),
and the test runs on a Windows worker (`exec-windows`), which CI's
Windows job has, and so does a Windows development machine (below).

```sh
just check-windows    # every library, binary and test, for Windows
AMUX_WINDOWS_PLATFORM=windows-arm64 just check-windows    # for Windows on Arm
```

builds the whole workspace for `windows-cli-testing-dev` (on a Windows on
Arm machine, `windows-arm64-cli-testing-dev`) and downloads nothing. It needs a Linux worker in reach. A crate's C, which reindeer's
fixups compile for Windows, is compiled against the C runtime as a DLL
(`/MD`), as Rust's own Windows code and the `cc` crate's builds link it;
`windows-targets` links the Windows API with `raw-dylib`
(`third-party/rust/fixups/windows-targets`), since the import library its
companion crate carries is found through a search path a build script
gives Cargo, which reaches no dependent's link under Buck2.

### A Windows development machine

A Windows machine you develop on is set up like a Mac in front of a build
host: its own checkout, and its own NativeLink, whose cache fronts the build
host's and whose worker runs the Windows tests. The build host's worker
registers with it too and runs the Linux actions, which on Windows means
every compile and link, so nothing builds on the Windows machine itself.
It needs:

- Git for Windows, with its `bin` directory on `PATH`: the recipes run under
  its `sh` (the justfile's `windows-shell`).
- `just`, and Python 3.12 or newer, also named `python3`. Python's installer
  makes only `python.exe`, and the `python3.exe` that answers first is a
  stub that opens the Microsoft Store; a copy of `python.exe` named
  `python3.exe` beside it does.
- Developer Mode (symbolic links without elevation), long paths, and
  `git config core.autocrlf false`, so a checkout's goldens and fixtures
  keep main's bytes.
- The build host's NativeLink in reach, and this machine reachable from the
  build host's worker on the port it registers at.

```sh
python scripts/nativelink.py install --remote grpc://<host>:50051 --workers <this machine's address>:50062
just test-windows
```

`install` runs NativeLink as a scheduled task of yours that starts at boot,
signed in or not, and starts it again a minute after it stops; at boot the
address the build host's worker registers at may not be up yet, and
NativeLink exits until it is. Its cache and log are under
`C:\amux\nativelink`, near the drive's root because an action's directory
is under it, and from the profile's `AppData` a test's git objects passed
the 260 characters git on Windows allows a path. The worker offers every core, and never
fewer than the six a Windows test that takes the whole worker asks for
(`build/toolchains/BUCK`). Unlike a Mac's, it doesn't fall back to building
alone when the build host is out of reach: a Windows machine can't compile
anything itself, so Linux actions wait for it instead.

On Windows on Arm (a Windows VM on an Apple Silicon Mac), `just
check-windows` and `just test-windows` build and run Arm64 binaries, natively;
`AMUX_WINDOWS_PLATFORM=windows` runs the x64 ones instead, under Windows'
emulation, which matches what CI tests but is slower. The worker says it is
Arm (its `cpu` property), and an Arm64 test asks for that
(`exec-windows-arm64`), so it never reaches CI's x64 runner; an x64 test asks
for no `cpu` and may run on either. Buck2 and NativeLink themselves are their
x64 builds, under emulation: the pins name no Arm build for Windows.

### Repository checks

The repository's own checks are Buck2 tests too, so `just premerge` gets a
check from the cache until a file it reads changes: a docs-only edit reruns
the docs check and the tests that read that page, and nothing else. Each is a
script run over exactly the files its target declares
([`build/checks.bzl`](../build/checks.bzl)):

- in the root [`BUCK`](../BUCK) file: `docs-check`, `store-check`,
  `proto-compat`, `cores-check` (the sources half), `typed-provider-check`,
  `client-kind-check`, `codegen-check`, `deny`, and the unit tests of the
  repository's scripts, one test per file (`//:script-tests-land_queue`);
- beside the apps' scripts: the Apple apps' writing lints and their recipe
  scripts' tests ([`apps/apple/Tools/BUCK`](../apps/apple/Tools/BUCK)), and
  the Android app's ([`apps/android/BUCK`](../apps/android/BUCK),
  [`apps/android/tools/BUCK`](../apps/android/tools/BUCK)); the Swift and
  Kotlin mirrors of the bridge's values are held to their Rust by
  `//crates/ios-tools:unit`;
- in [`js/BUCK`](../js/BUCK): the browser extension's Biome, type checks,
  unit tests, end-to-end test and npm audit ([Browser extension](BROWSER.md#building-and-testing));
- the terminal journeys, `//crates/testnet:terminal-journeys`.

```sh
just checks                    # the repository's and the JavaScript's checks
just test //:docs-check        # one of them
just docs-check                # the same: the recipes of the checks' names run their tests
just script-tests              # a set of them, by label (`just test //: --include script-tests`)
```

A check is sent to the Linux worker when one is in reach, so a Mac and the
build host run the same action and share its result; with none in reach
it runs on the Mac's own worker, except `deny`, whose dependency checker
is the Linux toolchain's: it is skipped then, and `just test` names it. On a Mac,
Node for the JavaScript's checks is the release `//build:node` pins, the
version the Linux toolchain's is. A check finds nothing it didn't declare:
a test that reads a file outside its inputs fails, rather than passing on
whatever the checkout holds.

A check that needs more than the checkout's files gets it from
[`build/checks/inputs.py`](../build/checks/inputs.py), which `just test`
runs first and which writes `build/checks/inputs/` (not committed):

- the list of files the checkout tracks, which the docs check resolves
  links against, so it reruns when a file is added or removed rather than
  whenever one changes. A docs page that shows a source file word for word
  (`<!-- source: ... -->`) needs that file among the docs check's inputs.
- the newest release's store schema and protobuf schemas, which the store
  and protocol checks compare with in place of git's history. A clone
  without the release tags therefore fails those two checks.
- what `deny` reads: Cargo's resolved graph with every feature (which needs
  Cargo's registry, so it can't be made inside an action) and the
  workspace's manifests, made again when a manifest or the lockfile
  changes; a snapshot of the RustSec advisory database; and crates.io's
  index entry for each crate in the graph, which says whether a release was
  yanked. The last two are refreshed at most once a day, the index by
  `cargo update --dry-run`, which writes no lockfile. `deny` itself runs
  offline on them, so its result is a function of its inputs: it reruns
  when the lockfile, a manifest, `deny.toml` or a day's snapshot changes.
- today's date (UTC), for the npm audit. npm's advisories are a question
  to its registry that no file can stand in for, so `//js:audit` asks it
  inside the action and declares the date: its answer is kept for the rest
  of the day and asked again on the next, or when the lockfile changes.

`just premerge` is `just test` over the whole graph and, at the same time,
the checks that stay scripts ([`build/checks/premerge.py`](../build/checks/premerge.py)),
each run in the checkout with its output kept and printed when it fails:

| Check | Why it isn't a Buck2 test |
| --- | --- |
| `fmt-check` | rustfmt on the dated nightly over every Rust file: a test target would need every file of the workspace and the nightly realized on the worker, to replace a three-second run |
| `dependency-policy`, `shear`, the tokio half of `cores-check` | They read Cargo's resolved graph (`cargo metadata`, `cargo tree`), which needs Cargo's registry, and shear every crate's code besides |
| `tests-check` | It checks the suite catalogue against every crate's targets as Cargo sees them, every `just` recipe, and every path the catalogue names: its inputs would be the whole checkout |
| `parity`, `shipping-check` | Queries of Buck2's own graph (`uquery`, `cquery`), which no action of that graph can make |
| `lint` | Clippy is Buck2 actions, cached like the tests, but a build of each target's clippy output, which `buck2 test` doesn't build, so it is a `buck2 build` beside the test run |

`no-update-flags`, which refuses fixture-regeneration variables in the
environment, runs first, before anything starts, and then
`build/checks/inputs.py`: Buck2's daemon runs the test run and clippy's
build at once only while both see the same files, so what the check
targets read is written before either starts.

At the end, [`build/checks/timing.py`](../build/checks/timing.py) says
where the test run's time went: how many compiles, links and tests ran
rather than came from the cache, the worker time each kind took and the
longest of each, how many actions the worker ran at once, and the critical
path. A landing's log carries the same lines from the build host.
`scripts/py build/checks/timing.py <event log>` says it for any Buck2
command's log in `buck-out/v2/log`.

## Profiles

Five profiles are defined in the workspace [`Cargo.toml`](../Cargo.toml),
and Buck2's modes give the same flags:

| Profile | Purpose |
| --- | --- |
| `dev` | Routine builds and tests: incremental, no debug information, and unwinding panics |
| `release` | What ships, for the desktop binary and the mobile apps alike, and what `just perf` measures: fat LTO, one codegen unit, line tables, aborting panics, and no incremental compilation |
| `full-debug` | A `dev` build with complete debug information for debugger sessions |
| `phone-dev` | The bridge every development build of the iPhone and Android apps links: `release` without LTO or debug information |
| `wasm` | The browser extension's WebAssembly: `release` optimised for size, without debug information |

`release` builds are slow, since whole-program optimisation runs over the
whole binary at once; nothing on the routine edit-and-test path uses it.
`phone-dev` exists for the same reason: LTO would make every Rust edit's
rebuild of the app's development bridge three to four times slower, and keep
a copy of each crate's LLVM bitcode beside its code, about five times the
library's size on disk. There is no separate `test` or CI
profile, so one development configuration of each workspace crate serves
product builds, focused tests, and the full test build. CI disables
incremental compilation through its environment (`CARGO_INCREMENTAL=0`).

`dev` builds carry no debug information: line tables gave every test and
tool binary a dSYM bundle, about a third of a tree's `target/`. Panic and
assert messages still name their file and line. Use `just full-debug` when a
debugger needs file and line information. On Apple targets,
[`.cargo/config.toml`](../.cargo/config.toml) sets `split-debuginfo=packed`,
so a profile that does carry debug information keeps it in one dSYM per
binary instead of leaving every incremental object beside its Cargo unit.

The store always compiles SQLite into the binary (rusqlite's `bundled`
feature). The same config file's `LIBSQLITE3_FLAGS` compiles that SQLite
without the extensions the store never uses (full-text search, R-trees, JSON, statistics
tables, loadable extensions).

## Recipes

The root [`justfile`](../justfile) is the task contract, and `just --list`
shows every recipe. The iPhone and Android apps' recipes are modules:
`just --list ios` (`just ios build`, `just ios verify`) and
`just --list android` (`just android lint`, `just android unit`,
`just android build`, `just android verify`). Every recipe runs its
command under `scripts/bounded`, a wall-clock bound that uses GNU `timeout`
where present and a small fallback elsewhere; a firing bound is a hang to
diagnose, not a slow machine. `AMUX_GIT_SHA` is exported from the current
commit for product builds.

The recipes a build or change usually needs:

| Recipe | What it runs |
| --- | --- |
| `just build` | The desktop binary, at `.wt/build/amux` |
| `just check` | Builds every workspace library, binary and test |
| `just full-debug` | The desktop binary under the `full-debug` profile |
| `just release-check` | The shipping binary under `release`, then the release policy check (see [Release](RELEASE.md)) |
| `just test` | Every workspace test whose inputs changed; target patterns narrow it (`just test //crates/node:`) |
| `just test-crate <crate>` | One crate's tests |
| `just test-build` | Compiles every test target without running it |
| `just goldens` | Re-records the goldens in the checkout |
| `just doctest` | Documentation tests, under Cargo |
| `just lint` | Clippy on every target, warnings denied |
| `just parity` | Fails when a crate's `BUCK` and `Cargo.toml` disagree |
| `just shipping-check` | Fails when a test-only crate or test feature reaches the desktop binary or the extension's WebAssembly as they ship |
| `just buckify` | Regenerates `third-party/rust/BUCK` after a dependency changes |
| `just cargo-test` | The workspace tests under Cargo and nextest, as CI's macOS and Windows jobs run them |
| `just fmt`, `just fmt-check` | Format, or check formatting, with the pinned nightly |
| `just protobuf` | Regenerates the committed protobuf output |
| `just codegen-check` | Fails if the committed protobuf output is stale |
| `just checks` | The repository's and the browser extension's checks, as Buck2 tests ([Repository checks](#repository-checks)) |
| `just mobile-check` | Check the provider-free client graph for iOS |
| `just android check` | Build the JNI bridge for Android, as it ships and as a driving build carries it |
| `just android lint` | Check Android writing rules and the string inventory |
| `just android graph-check` | Prove the `app-jni` graph carries no provider, host, schema, or test crate |
| `just android unit` | Build the host JNI library and run all Android JVM and Robolectric unit suites |
| `just android build` | Build the Android JNI library and debug APK |
| `just android storybook` | Render the headless Robolectric/Roborazzi PNG gallery into `target/android/storybook/index.html` |
| `just android journey` | Drive the Android debug APK on `amux-android-1` against real daemons and relay (`testnet journey android`) |
| `just android accessibility` | Run the Android Robolectric Compose semantics, bounds, 48 dp touch-target, heading and WCAG contrast-token audit (`AccessibilityAuditTest`) |
| `just android scope-audit` | Build the Release APK and verify, from R8's mapping and the library's symbols, that no debug surfaces or `debug-tools` symbols ship and SQLite is bundled |
| `just android release-smoke` | Run the launch smoke test on the minified Release APK on an emulator or phone |
| `just android verify` | Run the full ordered Android verification sequence (lint, graph check, script tests, unit with the accessibility audit, storybook, scope audit, release launch, journeys) |
| `just premerge` | Every check and test before a change reaches main; see [CI](CI.md) |
| `just land "<message>"` | Merges this tree's HEAD onto main, runs `just premerge` on the merge and pushes it |

[Testing](TESTING.md) covers the test recipes (`suite`, `test-tui`,
`spec`, `journey`, `live`, `perf`, `tests-list`) in detail.

## Generated code

Generated code is committed, and a check fails when it is stale.

**Protobuf.** The schemas live under
[`proto/amux/v1`](../proto/amux/v1). The generated
Rust (`amux.v1.rs`) and descriptor set (`amux.v1.bin`) are committed under
[`crates/wire/src/generated`](../crates/wire/src/generated). After changing a
schema:

```sh
just protobuf
just codegen-check
```

Both build `//crates/wire:protobuf`, which runs `xtask codegen` into
Buck2's output with protoc 31.1 from protobuf's releases, so it runs again
only when a schema changes. `just protobuf` copies the result into
`crates/wire/src/generated`; `just codegen-check` fails if the committed
files differ from it. Under Cargo, `cargo run -p xtask -- codegen` writes the
same files with the protoc the `protoc-bin-vendored` crate carries.
`just proto-compat`
lints the schemas with buf, and fails when they no longer read what the latest
release wrote: a field removed without reserving its number, a number reused
or retyped, a name changed; [the wire](WIRE.md#compatibility) has the rules.

**Swift and Kotlin value types.** The Swift mirrors of the view values the
Apple apps read (`Values.swift`, which the Apple build generates with
`ios-tools swift-types --values`, and the committed
`apps/apple/Packages/AmuxPush/Sources/AmuxPush/Said.swift`) and the Kotlin
mirrors the Android app reads (`Values.kt`, which the Android build
generates, and the committed
`apps/android/push/src/main/kotlin/sh/amux/push/Said.kt`) are generated from
the same Rust definitions by `cargo run -p ios-tools -- swift-types` and
`cargo run -p ios-tools -- kotlin-types`. Unit tests in the `ios-tools` crate,
run by `just test` and `just premerge`, fail when the committed files differ
from what the generators write.

## Worktrees

Development happens in git worktrees managed by `wt`, which owns each tree's
environment, its daemon and the simulator pools; `just` owns every build and
test task. [`.wt.toml`](../.wt.toml) holds the tree's side:

- A new tree runs `just build` in the background, logging to
  `.wt/create.log`; `wt ls` shows whether it is still running or failed.
  It copies no build output: what another tree built is already in the
  cache. A tree that uses Cargo (the phone apps, `just miri`) builds its
  own `target/`.
- `amux` inside a tree is a wrapper, `.wt/bin/amux`, that runs that tree's
  `.wt/build/amux`.
  Until the tree's first build has succeeded it refuses and says why.
- Each tree gets its own amux installation under `.wt/amux/`:
  `AMUX_CONFIG` names its installation file and `AMUX_LOG` its daemon log.
  The installation sets its own discovery scope, so a tree's daemon never
  meets real machines or other trees. It has no supervisor, so it never
  installs a release, and it turns `keep_awake` off. Its daemon starts the
  first time an `amux` command runs in the tree, and `wt stop` (or removing
  the tree) stops it, and the tree's Buck2 daemon with it.
- The pinned iPhone simulators are machine-wide resources that the iOS
  recipes lease through `scripts/with`.
- `just land` never touches the tree it is run from
  ([AGENTS.md](../AGENTS.md)). With `git config --global amux.landInbox
  <inbox>` set to your landing host's inbox (an SSH URL such as
  `host:/path/to/inbox.git`, or a path on the landing host itself), it
  pushes the branch to that host's landing queue and waits for the verdict:
  the queue lands one request at a time, from every machine, oldest first,
  and writes each landing's whole log beside its result. `just land
  --local`, or no inbox set, lands from a worktree of this machine's own
  under `~/.cache/amux/land`, one landing at a time per machine; that is
  the way to land while the landing host is down. Either
  way the landing runs `just premerge` and nothing else: the protocol
  models, the C ABI's pointer checks and the sanitizers run in the nightly
  ([CI](CI.md#nightlies)).
- A landing host runs the queue from a checkout of main, as an account that
  may push to the repository: `python3 scripts/land_queue.py serve --inbox
  <inbox> --tree <tree> --nix --self-update`, where the inbox is a bare
  repository everyone who lands can write and the tree is a clone of its
  own that landings reset and clean. `--nix` runs each check in the merge's
  own `nix develop`; `--self-update` moves the queue to main's copy of its
  code when a landing changes it. Where those live, and the service that
  runs it, are the host's configuration, not this repository's.
