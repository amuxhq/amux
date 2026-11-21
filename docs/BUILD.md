# Building amux

*For developers building amux from source: the toolchain, the build profiles, the task recipes and the generated code that is committed.*

## Toolchains

[`rust-toolchain.toml`](../rust-toolchain.toml) pins Rust 1.98.0 with the
`minimal` profile, the components repository tasks use (`clippy`,
`llvm-tools`, `rust-src`, `rustfmt`) and the cross-compilation targets:

| Target | Used for |
| --- | --- |
| `aarch64-apple-ios`, `aarch64-apple-ios-sim`, `x86_64-apple-ios` | The iPhone app's Rust bridge and `just mobile-check` |
| `aarch64-linux-android`, `x86_64-linux-android` | The Android app's Rust bridge and `just android check` |
| `x86_64-pc-windows-msvc` | The Windows release build and the [Windows cross-check](CI.md) |
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
- Xcode and XcodeGen build the iPhone app (see [the iPhone app](IOS.md)).
- JDK 21, the Android SDK/NDK, and `cargo-ndk` build the Android app (`just --list android`).
- `tmux` drives the terminal journeys.
- Node 24 or newer, with npm, builds and tests the browser extension under
  `js/` (`just browser build`, `just browser test`). Biome formats and lints it
  (`just browser fmt`), and `js/.npmrc` stops packages' install scripts from
  running.
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
`just shear` run and, on Linux, a C compiler. `nix develop -c just test`
runs one recipe in it. [`nix/toolchain.nix`](../nix/toolchain.nix) is the
list, and `flake.lock` pins every tool in it, so two machines on the same
revision get the same tools. `nix profile install .#toolchain` installs the
same list onto the PATH of every shell, for a machine that should simply have
it.

The shell has no rustup, so it provides the nightly's cargo as
`cargo-nightly`, which `just fmt`, `just fmt-check`, `just miri` and `just sanitize` use
whenever it is on the PATH. Its date in `nix/toolchain.nix` has to move with the justfile's.

The workspace has one lockfile. Build and verification recipes pass
`--locked`, and protobuf output is committed, so the same revision selects
the same Rust dependencies and wire code on every machine. Protobuf
generation uses a vendored `protoc`, so none needs installing.

`just proto-compat` needs [buf](https://buf.build/docs/cli/installation/),
which `nix/toolchain.nix` provides and CI pins to 1.75.0 in
`.github/actions/setup-buf`; outside Nix, `brew install bufbuild/buf/buf`.

## Profiles

Five profiles are defined in the workspace [`Cargo.toml`](../Cargo.toml):

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
`just android build`). Every recipe runs its
command under `scripts/bounded`, a wall-clock bound that uses GNU `timeout`
where present and a small fallback elsewhere; a firing bound is a hang to
diagnose, not a slow machine. `AMUX_GIT_SHA` is exported from the current
commit for product builds.

The recipes a build or change usually needs:

| Recipe | What it runs |
| --- | --- |
| `just build` | `cargo build -p amux --bins`: the desktop binary |
| `just check` | `cargo check` of every workspace library and binary |
| `just full-debug` | The desktop binary under the `full-debug` profile |
| `just release-check` | The shipping binary under `release`, then the release policy check (see [Release](RELEASE.md)) |
| `just test` | Every workspace test target; arguments after `--` go to Cargo |
| `just test-crate <crate>` | One crate's tests |
| `just test-build` | Compiles every ordinary test target without running it |
| `just doctest` | Documentation tests |
| `just lint` | Clippy on every target, warnings denied |
| `just fmt`, `just fmt-check` | Format, or check formatting, with the pinned nightly |
| `just protobuf` | Regenerates the committed protobuf output |
| `just codegen-check` | Fails if the committed protobuf output is stale |
| `just mobile-check` | Check the provider-free client graph for iOS |
| `just android check` | Check the provider-free client and JNI bridge graph for Android (`cargo-ndk`) |
| `just android lint` | Check Android writing rules, string inventory, and generated Kotlin mirrors |
| `just android graph-check` | Prove the `app-jni` graph carries no provider, host, schema, or test crate |
| `just android unit` | Build the host JNI library and run all Android JVM and Robolectric unit suites |
| `just android build` | Build the Android JNI library and debug APK |
| `just premerge` | The quick checks before a change reaches main; see [CI](CI.md) |
| `just ci` | The whole check sequence; see [CI](CI.md) |

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

`just protobuf` runs `cargo run -p xtask -- codegen`. `just codegen-check`
regenerates and fails if the committed result differs. `just proto-compat`
lints the schemas with buf, and fails when they no longer read what the latest
release wrote: a field removed without reserving its number, a number reused
or retyped, a name changed; [the wire](WIRE.md#compatibility) has the rules.

**Swift and Kotlin value types.** The Swift mirrors of the view values the
iPhone app reads (`apps/apple/Packages/AmuxCore/Sources/AmuxValues/Values.swift`)
and the Kotlin mirrors the Android app reads
(`apps/android/core/values/src/main/kotlin/sh/amux/values/Values.kt`,
`apps/android/push/src/main/kotlin/sh/amux/push/Said.kt`) are generated from
the same Rust definitions by `cargo run -p ios-tools -- swift-types` and
`cargo run -p ios-tools -- kotlin-types`. Unit tests in the `ios-tools` crate,
run by `just test`, fail when the committed files differ from what the
generators write.

## Worktrees

Development happens in git worktrees managed by `wt`, which owns each tree's
environment, its daemon and the simulator pools; `just` owns every build and
test task. [`.wt.toml`](../.wt.toml) holds the tree's side:

- A new tree starts with a copy-on-write copy of the main checkout's `target`
  directory, so third-party crates that haven't changed are already built.
  Then `just build` runs in the background, logging to `.wt/create.log`;
  `wt ls` shows whether it is still running or failed. Each tree owns its
  build output, and removing the tree removes it.
- `amux` inside a tree is a wrapper that runs that tree's
  `target/debug/amux`. Until the tree's first build has succeeded it refuses
  and says why, rather than run the binary copied from the main checkout.
- Each tree gets its own amux installation under `.wt/amux/`:
  `AMUX_CONFIG` names its installation file and `AMUX_LOG` its daemon log.
  The installation sets its own discovery scope, so a tree's daemon never
  meets real machines or other trees. It has no supervisor, so it never
  installs a release, and it turns `keep_awake` off. Its daemon starts the
  first time an `amux` command runs in the tree, and `wt stop` (or removing
  the tree) stops it.
- The pinned iPhone simulators are machine-wide resources that the iOS
  recipes lease through `scripts/with`.
- [AGENTS.md](../AGENTS.md) says how a branch lands on main.
