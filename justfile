set shell := ["sh", "-cu"]
set windows-shell := ["sh", "-cu"]
set positional-arguments

export AMUX_GIT_SHA := `git rev-parse HEAD`

# On Windows, Git's sh rewrites an argument that looks like a path as it
# starts a native program, and takes a leading `//` as an escaped `/`:
# `//js:` reached Buck2 as `/js:`. An argument starting `//` is left alone,
# while `/c/...` paths still become `C:/...`. The second variable keeps the
# first's `//` from being rewritten too, to `/`, on its way through a native
# program (Python) to a nested sh. Neither means anything elsewhere.
export MSYS2_ARG_CONV_EXCL := if os_family() == "windows" { "//" } else { env("MSYS2_ARG_CONV_EXCL", "") }
export MSYS2_ENV_CONV_EXCL := if os_family() == "windows" { "MSYS2_ARG_CONV_EXCL" } else { env("MSYS2_ENV_CONV_EXCL", "") }
# The repository's scripts read and write text as UTF-8, which Python on
# Windows takes only in its UTF-8 mode (elsewhere the locale's is UTF-8
# already); an empty value is Python's default.
export PYTHONUTF8 := if os_family() == "windows" { "1" } else { env("PYTHONUTF8", "") }

# Wall-clock bound for every recipe; portable where GNU timeout is absent.
bounded := "scripts/bounded"
# The pinned Buck2, which builds and tests through NativeLink (docs/BUILD.md).
buck2 := "scripts/buck2"
# The pinned nightly's cargo, for formatting and Miri. rustup's `+toolchain`
# selects it; the Nix dev shell has no rustup and provides `cargo-nightly`
# instead (nix/toolchain.nix), at the same date.
nightly_cargo := `command -v cargo-nightly >/dev/null && echo cargo-nightly || echo cargo +nightly-2026-08-30`

# List the available repository tasks.
default:
    @just --list

# Export the editable logo and render its visual review sheet.
logo *ARGS:
    {{bounded}} 120 uv run --script scripts/logo.py "$@"

# The scene comes up in ~/.local/state/amux-marketing/<tree>/launch, its
# clones in /Users/Shared; `shots` starts it afresh, waits for the agents to
# come to rest and photographs the terminal client, and the phone when run
# under a simulator lease. Waits for agents, so only its steps are bounded.
# Start or photograph the README's launch scene (marketing/README.md).
scene *ARGS:
    scripts/py -B marketing/scene.py "$@"

# Compose the README's pictures from the scene's shots into images/readme/.
compose *ARGS:
    {{bounded}} 300 uv run --script marketing/compose.py "$@"

# Compose the App Store's screenshots from the scene's phone shots (marketing/README.md).
store *ARGS:
    {{bounded}} 300 uv run --script marketing/store.py "$@"

# Compose the Google Play Store's screenshots, feature graphic and icon from the scene's Android shots.
play-store *ARGS:
    {{bounded}} 300 uv run --script marketing/play_store.py "$@"

# The amux binary's release recipes: `just amux release`, `just amux deploy` (`just --list amux`).
mod amux 'crates/amux/justfile'

# The iPhone app's recipes: `just ios build`, `just ios verify`, ... (`just --list ios`).
mod ios 'apps/apple/justfile'

# The Mac app's recipes: `just mac build`, `just mac run` (`just --list mac`).
mod mac 'apps/apple/Mac/justfile'

# The Android app's recipes: `just android build`, `just android unit`, ... (`just --list android`).
mod android 'apps/android/justfile'

# The browser extension's recipes: `just browser build`, `just browser test`, ... (`just --list browser`).
mod browser 'js/extension/justfile'

# The tree's own amux, at .wt/build/amux, which wt's `amux` wrapper in
# .wt/bin runs (.wt.toml); the path holds whatever Buck2 configuration built it.
# Build the desktop product binary.
build:
    mkdir -p .wt/build && {{bounded}} 900 {{buck2}} build //crates/amux:amux --out .wt/build/amux

# Check every workspace library and binary, and build every test.
check:
    {{bounded}} 3600 {{buck2}} build //crates/...

# Which Windows the two recipes below build for: x64, as releases ship and
# CI tests, except on a Windows on Arm machine, whose own tests run natively
# as Arm64 (docs/BUILD.md, "A Windows development machine").
# AMUX_WINDOWS_PLATFORM names either (`windows`, `windows-arm64`) instead.
windows_platform := env("AMUX_WINDOWS_PLATFORM", if os() == "windows" { `scripts/py -c "import platform; print('windows-arm64' if platform.machine().lower() in ('arm64', 'aarch64') else 'windows')"` } else { "windows" })

# Compile and link every workspace library, binary and test for Windows,
# on a Linux worker with the Windows toolchain (nix/windows.nix), from any
# machine; nothing comes back but the result. CI's Windows job runs its
# tests from these builds.
check-windows:
    {{bounded}} 3600 {{buck2}} build //crates/... --target-platforms root//build/platforms:{{windows_platform}}-cli-testing-dev --materializations none

# Compile every Swift module of the iPhone app and the Mac app, their
# packages' and their test bundles', in the configurations CI's two Apple
# jobs build: nothing is linked or run, so no simulator boots, and nothing
# comes back but the result. A change to the Rust, the bridge's header, its
# generated Swift or the protocol that breaks the Swift fails here, on a
# Mac, before it lands. The Swift reads only the bridge's generated header
# and values, never its compiled library, so a deep Rust change costs a
# small rebuild of the tools that write them, not the bridge. Linking,
# bundling, assets, strings and Info.plist are left to CI's Apple jobs:
# they break from changes under apps/apple, which those jobs already run
# for. Off a Mac the Apple targets don't configure. `just premerge` runs it
# on a Mac. Each build runs once more if the Mac's NativeLink only gave up
# on a blob, as it does under a machine loaded past its cores
# (scripts/rerun-on-cache-timeout.sh).
apple-build:
    #!/bin/sh
    set -u
    query() { {{buck2}} uquery "kind(_swift_library, attrregexfilter(default_target_platform, $1, //apps/apple/...))"; }
    iphone=$(query ios-sim) && mac=$(query macos) || exit 1
    build() { scripts/rerun-on-cache-timeout.sh "{{bounded}} 3600 {{buck2}} build --materializations none --target-platforms root//build/platforms:$1 $2"; }
    build ios-sim-app-shipping-dev "$(echo $iphone)" & first=$!
    build macos-app-shipping-dev "$(echo $mac)" & second=$!
    status=0
    wait $first || status=1
    wait $second || status=1
    exit $status

# The workspace's tests for Windows, as CI's Windows job runs them after
# `just check-windows` (docs/CI.md, "The Windows job"): only a Windows
# worker runs them, which CI's Windows job has, and so does a Windows
# development machine. Left out, as before Buck2: the protocol models (the
# nightly's), the simulated stories, the linux-only tests and the live lanes.
# Run the workspace's tests for Windows on a Windows worker.
test-windows *ARGS:
    if [ "${1-}" = -- ]; then shift; fi; [ $# -gt 0 ] || set -- //crates/...; {{bounded}} 3600 {{buck2}} test "$@" --target-platforms root//build/platforms:{{windows_platform}}-cli-testing-dev --exclude live --exclude nightly --exclude linux-only --exclude journey --skip-incompatible-targets -- --timeout 3600

# Every test runs in a process of its own (build/test-runner.py), and a
# passing result is cached in NativeLink, so only tests whose inputs changed
# run. Target patterns narrow the run (`just test //crates/node:`, `just test
# //: //js:` for the repository's and the JavaScript's checks, which read
# what build/checks/inputs.py writes first); the live provider lanes are left
# out (`just live`), and so are the `nightly` tests, the protocol models,
# which the nightly workflow runs (`just models-nightly`), unless a target
# names them. With no Linux worker in reach, the `linux-only` tests are
# skipped, and the run ends by naming them. The Android app's tests are among
# them (every Android target is linux-only). A target this machine can't run
# is skipped even when named (the terminal journeys on Windows).
# Run the workspace's and the Android app's tests through Buck2.
test *ARGS:
    if [ "${1-}" = -- ]; then shift; fi; [ $# -gt 0 ] || set -- //crates/... //apps/android/...; scripts/py build/checks/inputs.py "$@"; status=0; {{bounded}} 3600 {{buck2}} test "$@" --exclude live --exclude nightly --skip-incompatible-targets -- --timeout 3600 || status=$?; scripts/py scripts/buck-config.py skipped "$@"; exit $status

# The repository's checks (the root BUCK file, and the apps' script tests and
# lints beside their scripts) and the JavaScript's (js/BUCK) as Buck2 tests,
# each over exactly the files it reads, so only those whose files changed
# run; `just premerge` runs them with every other test. The recipes of the
# same names (`just docs-check`, `just deny`) run each one's test.
# Run the repository's and the JavaScript's checks through Buck2.
checks:
    "{{just_executable()}}" test //: //js: //apps/apple/Tools: //apps/android/tools:

# Run tests for one named workspace crate.
test-crate CRATE:
    {{bounded}} 3600 {{buck2}} test //crates/{{CRATE}}: --exclude live -- --timeout 3600

# `just goldens` re-records the goldens of the interpreters, the terminal
# client and the views; target names narrow it (`just goldens
# //crates/ui-view:integration`). The tests run here, in the checkout, so what
# they write lands in it; review the diff before committing it.
# Re-record the goldens.
goldens *ARGS:
    {{bounded}} 1800 scripts/py build/goldens.py "$@"

# CI's Windows job runs Cargo and nextest through this twin of `test`, with
# nextest's flags, when it has no build host to build for it (a fork's pull
# request; .github/workflows/ci.yml).
# Run workspace tests under cargo-nextest, as CI's Windows job does without the build host.
cargo-test *ARGS: nextest
    if [ "${1-}" = -- ]; then shift; fi; scripts/workspace-test.sh "$@"

# `just flaky [--load] TARGET TEST [COUNT]` runs the tests of TARGET
# (`//crates/node:integration`) whose names contain TEST COUNT times
# (default 50), one round after another, and stops at the first failure;
# --load keeps every core busy meanwhile.
# Make a test that fails now and then fail on demand.
flaky *ARGS:
    {{bounded}} 7200 scripts/flaky.sh "$@"

# Refuse to start a test recipe without the pinned cargo-nextest.
[private]
nextest:
    @cargo nextest --version >/dev/null 2>&1 || { echo "cargo-nextest is not installed: brew install cargo-nextest, or cargo install --locked cargo-nextest@0.9.150 (.config/nextest.toml requires it)" >&2; exit 1; }

# Cross-compile every store test target and run it on the leased, booted iOS
# simulator, after proving the store's integration binary carries SQLite itself.
test-store-ios:
    scripts/with iphone -- {{bounded}} 2300 scripts/py -B apps/apple/Tools/ios-store-test.py

# Compile every ordinary workspace test target without running it.
test-build:
    {{bounded}} 3600 {{buck2}} build //crates/...

# Exercise TUI behavior.
test-tui:
    {{bounded}} 1200 {{buck2}} test //crates/tui: -- --timeout 1200

# Lint every workspace target with warnings denied: clippy over each of the
# workspace's own libraries, binaries and tests (build/checks/clippy.py),
# the live tests and perf binaries among them.
lint:
    {{bounded}} 3600 scripts/py build/checks/clippy.py

# Fail when a crate's BUCK file and its Cargo.toml disagree on its targets
# or dependencies (build/checks/parity.py).
parity:
    {{bounded}} 300 scripts/py build/checks/parity.py

# Fail when a test-only crate or a test feature reaches the desktop binary
# or the extension's WebAssembly as they ship (build/checks/shipping.py).
shipping-check:
    {{bounded}} 600 scripts/py build/checks/shipping.py

# Regenerate third-party/rust/BUCK from Cargo.toml and Cargo.lock with
# reindeer, after a dependency changes, then check every crate's BUCK still
# agrees with its manifest.
buckify:
    {{bounded}} 600 sh -c '"$(scripts/py scripts/buck-tools.py path reindeer)" buckify'
    "{{just_executable()}}" parity

# Format all Rust sources with the pinned nightly toolchain.
fmt:
    {{bounded}} 600 {{nightly_cargo}} fmt --all

# A Buck2 check (`//:fmt-check`), on a Linux worker with the Linux
# toolchain's dated nightly, over every member's Rust files.
# Check Rust formatting with the pinned nightly toolchain.
fmt-check:
    "{{just_executable()}}" test //:fmt-check

# Regenerate the committed protobuf bindings.
protobuf:
    out=$({{bounded}} 600 scripts/built //crates/wire:protobuf) && rm -rf crates/wire/src/generated && cp -R "$out" crates/wire/src/generated && chmod -R u+w crates/wire/src/generated

# Check that committed protobuf bindings match their sources.
codegen-check:
    "{{just_executable()}}" test //:codegen-check

# Lint amux.v1, and check it still reads what the latest release wrote; with
# a ref, check against that ref instead. Needs buf (docs/BUILD.md).
proto-compat *ARGS:
    {{bounded}} 120 scripts/proto-compat.sh "$@"

# Check the store only grew since the latest release; with a ref, since that
# ref instead.
store-check *ARGS:
    {{bounded}} 60 scripts/py scripts/store-check.py "$@"

# Write the profile the upgrade test opens, as this commit's build leaves it,
# under crates/node/tests/fixtures/upgrade/. Run once at the tag of a release
# that changed what a profile holds on disk, and commit the result.
upgrade-fixture: nextest
    {{bounded}} 600 cargo nextest run --locked -p node --run-ignored only -E 'test(write_this_releases_profile)'

# Build the shipping binary as a release builds it, and enforce the release
# dependency policy. Buck2 builds it from scratch with this machine's own
# toolchains (scripts/release-build.py) into target/release/, or
# target/<triple>/release/ with `--target`, which must name this machine.
release-check *ARGS:
    if [ "${1-}" = -- ]; then shift; fi; {{bounded}} 2400 scripts/py scripts/release-build.py "$@"
    if [ "${1-}" = -- ]; then shift; fi; scripts/release-policy-check.sh "$@"

# Install the pre-push credential scan (scripts/hooks/pre-push) for every worktree.
hooks:
    scripts/hooks/install

# Check the provider-free graph for iOS.
mobile-check:
    {{bounded}} 1200 cargo check --locked -p node -p client -p ui-state -p ui-runtime -p app-runtime -p app-embedded -p app-bridge --target aarch64-apple-ios
    {{bounded}} 1200 cargo check --locked -p node -p client -p ui-state -p ui-runtime -p app-runtime -p app-embedded -p app-bridge --target aarch64-apple-ios-sim

# Run workspace tests with isolated user configuration and no external network,
# on Linux, through sudo. The workspace test script bounds its own compile and
# run phases. The protocol models are pure computation that would take most of
# the run's bound, and run in nightly jobs of their own.
offline-test: nextest
    scripts/offline-check.sh scripts/workspace-test.sh --lib --tests --exclude models

# Build the product with the full-debug profile.
full-debug:
    {{bounded}} 900 cargo build --locked -p amux --bins --profile full-debug

# Render or inspect deterministic TUI evidence.
shot *ARGS:
    if [ "${1-}" = -- ]; then shift; fi; {{bounded}} 600 {{buck2}} run --quiet //crates/shot:amux-shot -- "$@"

# Replay a report (its id, its prefix in the reports bucket, its file or its
# folder), an installation folder, or a relay's or the supervisor's report
# through every stage it carries, keeping it under target/repro/ (docs/DEBUGGING.md).
repro WHAT *ARGS:
    repro=$(scripts/built //crates/replay-support:repro) && cd "{{invocation_directory()}}" && "{{justfile_directory()}}/{{bounded}}" 900 "$repro" "$@"

# Interactive, so only the build and a capture are bounded; `just storybook
# capture --out DIR` draws every story to text and PNG (docs/TERMINAL.md).
# Step through the terminal client's named screens, rebuilding on save.
storybook *ARGS:
    #!/usr/bin/env sh
    set -eu
    storybook=$({{bounded}} 900 scripts/built //crates/shot:amux-storybook)
    case "${1-}" in
    capture) exec {{bounded}} 600 "$storybook" "$@" ;;
    *) exec "$storybook" --watch "$@" ;;
    esac

# Run one declared journey through its real client. System journeys are the
# built binaries on the fakes, printing their transcripts. `just journey
# terminal all` runs every terminal story, keeps going after a failure,
# prints one line per story to target/journeys/summary.txt and fails if any
# story did.
journey CLIENT NAME *ARGS:
    #!/usr/bin/env sh
    set -eu
    if [ "${3-}" = -- ]; then shift 3; else shift 2; fi
    case "{{CLIENT}}" in
    terminal)
        TESTNET_BINARIES=$({{bounded}} 900 scripts/built //crates/provider-fakes:binaries)
        testnet=$({{bounded}} 900 scripts/built //crates/testnet:testnet-bin)
        export TESTNET_BINARIES
        if [ "{{NAME}}" = all ]; then
            {{bounded}} 3000 "$testnet" journey terminal all
        else
            {{bounded}} 600 "$testnet" journey terminal "{{NAME}}"
        fi
        ;;
    system)
        # --keep leaves the install running for other clients to open.
        if [ "${1-}" = --keep ]; then export AMUX_JOURNEY_KEEP=1; fi
        AMUX_TEST_LOCAL=1 {{bounded}} 1500 {{buck2}} run //crates/amux:integration -- --exact "system_journeys::$(echo "{{NAME}}" | tr - _)"
        ;;
    *)
        echo "no {{CLIENT}} journeys: terminal or system" >&2
        exit 2
        ;;
    esac

# The world comes up in ~/.local/state/amux-env/<tree>/NAME with the
# clients its file declares and is held, with the simulator when it has a
# phone, until Ctrl-C; drive it from that folder with `testnet ctl`.
# Interactive, so only its steps are bounded.
# Start a named world of real agents from environments/NAME.json (docs/TESTNET.md).
env NAME:
    scripts/py -B scripts/env.py {{NAME}}

# Fuzz the code that reads bytes amux did not write (the Claude and Codex
# decoders, the journal's frames and reader, redaction) for 30 seconds a
# target on the pinned nightly, seeded from the recorded fixtures; fails on a
# crash and prints the command that replays it. `--seconds N` and target
# names (`just fuzz-smoke journal_reader`) change the run. Needs cargo-fuzz.
fuzz-smoke *ARGS:
    {{bounded}} 1500 scripts/py scripts/fuzz-smoke.py "$@"

# Run the phone's C ABI tests under the pinned nightly's Miri, which checks
# every pointer handed across the boundary, read and freed again. Tests that
# start an installation or a tokio runtime are ignored under Miri, each with
# its reason. Needs the nightly's miri and rust-src components. The nightly
# workflow runs it.
miri *ARGS:
    {{bounded}} 1500 {{nightly_cargo}} miri test --locked -p app-ffi "$@"

# Run the tests that reach unsafe code and C under the pinned nightly's
# AddressSanitizer (`just sanitize address`) or ThreadSanitizer (`just
# sanitize thread`, Linux only), with the standard library rebuilt under it;
# crate names after the kind narrow the run. scripts/sanitize.py says which
# suites run and what goes unchecked. Needs the nightly's rust-src component.
# The nightly workflow runs both.
sanitize KIND *CRATES:
    {{bounded}} 2400 scripts/py scripts/sanitize.py --cargo "{{nightly_cargo}}" "$@"

# Fail when any state-machine core's dependency graph contains tokio, or any
# core source contains a lock, a spawned task, an async fn or an await.
cores-check:
    "{{just_executable()}}" test //:cores-check //:cores-graph-check

# A desktop-platform conditional (`cfg(unix)`, `cfg(windows)`, a desktop
# `target_os`, `std::os::unix`, `libc::`, ...) belongs in the homes named for
# platform code; elsewhere a file may hold no more than
# build/checks/platform-allowlist.txt allows it, and a test gated off Windows
# must name a feature Windows leaves out (docs/TESTING.md, "Holding a test
# back on Windows"). With no options it is the Buck2 check
# `//:platform-check`; with any it runs the script here: `--strict` also
# wants the allowlist empty and no test ignored on Windows, `--list` prints
# every conditional counted, `--write-allowlist` writes the allowlist.
# Fail on desktop-platform code outside its homes, beyond the allowlist.
platform-check *ARGS:
    if [ $# -eq 0 ]; then "{{just_executable()}}" test //:platform-check; else {{bounded}} 120 scripts/py -B build/checks/platform_check.py "$@"; fi

# Enforce production and test-infrastructure dependency boundaries.
dependency-policy:
    "{{just_executable()}}" test //:dependency-policy

# Check every dependency against deny.toml with cargo-deny: no open security
# advisory or yanked release, only the licences it lists, and only crates.io
# and the rustls patch and wt as sources. A Buck2 check (`//:deny`) on a
# Linux worker, with the Linux toolchain's cargo-deny, over Cargo's graph
# and crates.io's index as the day's fetch of the registry has them
# (//:cargo-registry) and the snapshot of the advisory database that
# build/checks/inputs.py takes at most once a day.
deny:
    "{{just_executable()}}" test //:deny

# Fail on any dependency no crate's code uses, so unused ones do not pile up
# again. A crate lists what the code cannot show it needs under
# `[package.metadata.cargo-shear]`. A Buck2 check (`//:shear`), with the
# Linux toolchain's cargo-shear over the fetched registry.
shear:
    "{{just_executable()}}" test //:shear

# `just suite desktop-cli-system` runs exactly the Cargo tests that suite of
# tests/catalog.toml lists, on `just test`'s build; anything after the
# name goes to nextest (a test-name filter, `--no-capture`). The bound is the
# protocol models' at their CI bounds, one after another on a four-core machine.
# Run one catalogued suite's Cargo tests.
suite NAME *ARGS:
    {{bounded}} 2400 scripts/py -B scripts/tests-catalog.py run "$@"

# Print the suite catalogue with its boundaries, lanes and focused recipes.
tests-list:
    scripts/py -B scripts/tests-catalog.py list

# Validate every catalogued recipe, target, feature, manifest and baseline.
# A script, not a Buck2 check: it asks just about every recipe and looks
# for every path the catalogue names; Cargo's view of every crate's targets
# it reads from //:cargo-metadata (scripts/cargo_metadata.py).
tests-check:
    {{bounded}} 120 scripts/py -B scripts/tests-catalog.py check

# The unit tests of the repository's own scripts, as Buck2 checks, one per
# test file (the root BUCK). The phone's scripts have theirs in `just ios
# script-tests` and `just android script-tests`.
script-tests:
    "{{just_executable()}}" test //: --include script-tests

# Check the protocol models: each model's tests at its CI bounds and at its
# nightly bounds (the ignored ones), one after another, every core each.
# Model names, the modules of the models' integration tests (`just
# models-nightly family`), narrow it to those models. The bound leaves the
# nightly job, one model each on a four-core runner, a quarter of an hour of
# its six for the build.
models-nightly *MODELS: nextest
    if [ "${1-}" = -- ]; then shift; fi; models='[a-z_]+'; [ $# -eq 0 ] || models=$(echo "$*" | tr ' ' '|'); {{bounded}} 20700 cargo nextest run --locked --cargo-profile models -p models --profile nightly --run-ignored all --no-capture -E "binary_id(models::integration) & test(/^($models)::/)"

# Walk the testnet's simulated stories by seed: every suite tests/catalog.toml
# declares a simulated mode for, under TESTNET_MODE=simulated, a few seeds per
# story. `--seed N` reruns one seed (a failure prints its command), `--seeds N`
# walks N, and names after them narrow the run to matching stories.
sim *ARGS: nextest
    if [ "${1-}" = -- ]; then shift; fi; {{bounded}} 1200 scripts/py scripts/sim.py "$@"

# Run every workspace test under cargo-llvm-cov and write line coverage per
# crate, with the commit it measured, to target/coverage/coverage.md. Needs
# `cargo install --locked cargo-llvm-cov@0.9.0` and the llvm-tools-preview
# rustup component.
coverage: nextest
    {{bounded}} 3600 scripts/py scripts/coverage.py

# Fail when a docs link or image does not resolve, a figure is unreferenced
# or not standalone, or docs/README.md does not list every page once.
docs-check:
    "{{just_executable()}}" test //:docs-check

# Fail if an interpreter or the agent's provider handshake builds or reads
# provider JSON by hand instead of through the protocol crates.
typed-provider-check:
    "{{just_executable()}}" test //:typed-provider-check

# Fail if a client crate decides something by an agent's kind or a
# provider's name outside the places its list allows.
client-kind-check:
    "{{just_executable()}}" test //:client-kind-check

# Refuse fixture-regeneration flags before any asserted CI check runs.
[private]
no-update-flags:
    scripts/no-update-flags.sh

# The Buck2 tests `just premerge` runs: every crate's tests and the terminal
# journeys, the repository's checks (the scripts' tests and deny among them),
# the browser extension's, the Apple apps' lints and script tests, and the
# Android app's tests, lints and script tests.
premerge_tests := "//crates/... //: //js: //apps/apple/Tools:"
premerge_android := "//apps/android/..."

# The checks a change passes before it reaches main: through Buck2 every
# test and check in `premerge_tests` and `premerge_android` (only those whose
# inputs changed run; the rest come from NativeLink's cache, in this tree or
# any other), and beside them the checks that stay scripts, because they read
# the whole workspace or query the build graph itself (build/checks/premerge.py
# names them and says why), among them the build of every Windows binary on
# a Linux worker (`check-windows`) and, on a Mac, the compile of every
# Swift module of the Apple apps and their tests (`apple-build`). A failing
# one fails the run and its output is printed at the end. Linking the Apple
# apps, their tests and the tests on macOS and Windows stay with `just ios
# verify` and CI (docs/CI.md, "`just premerge`").
# Run the checks a change passes before it reaches main.
premerge: no-update-flags
    JUST="{{just_executable()}}" scripts/py -B build/checks/premerge.py {{premerge_tests}} {{premerge_android}}

# `just premerge` in two halves, which together run all of it: `workspace`
# is everything but the Android app, `android` the Android app's tests,
# lints and script tests. A fork's pull request gets no build host, so CI
# builds and tests everything on four-core runners, and all of it on one
# takes longer than a test run's bound; it runs the halves side by side.
# Neither builds Windows: a fork's Windows job compiles it under Cargo.
# Run one half of `just premerge`: workspace or android.
premerge-half HALF:
    #!/bin/sh
    set -eu
    case "{{HALF}}" in
        workspace)
            "{{just_executable()}}" no-update-flags
            JUST="{{just_executable()}}" scripts/py -B build/checks/premerge.py --without-windows {{premerge_tests}}
            ;;
        android)
            "{{just_executable()}}" no-update-flags
            "{{just_executable()}}" test {{premerge_android}}
            ;;
        *) echo "premerge-half: workspace or android, not {{HALF}}" >&2; exit 2 ;;
    esac

# That build/ci-jobs.toml, which decides which of CI's macOS and Windows
# jobs a change runs, names every file only the Apple apps' graphs read
# (scripts/ci_jobs.py graph-check): a query of the configured graph, so
# premerge runs it beside the tests.
# Check CI's job table against the Apple apps' build graphs.
ci-jobs-check:
    {{bounded}} 300 scripts/py scripts/ci_jobs.py graph-check {{premerge_tests}} -- {{premerge_android}}

# `just land "What the branch does"` merges this tree's HEAD onto a fresh
# origin/main, runs `just premerge` on the merge and pushes it. With `git
# config amux.landInbox` set it goes through the landing host's queue, one
# landing at a time from every machine; `--local`, or no inbox, lands from
# a worktree of this machine's own. Nothing in this tree is touched
# (scripts/land.py).
# Land this branch on main.
land *ARGS:
    scripts/py -B scripts/land.py "$@"

# Print the whole log of a landing the queue ran, by the id `just land` named.
land-log ID:
    scripts/py -B scripts/land.py --log "{{ID}}"

# Merge a fresh origin/main into this branch; when only the generated
# protobuf bindings conflict, regenerate them (scripts/merge-main.sh).
merge-main:
    scripts/merge-main.sh

# Run the live provider compatibility lane for one kind (claude_pty,
# claude_sdk or codex) and scenario (initialize, respond, decide, interrupt,
# resume, plan, questions, usage, catalogue, host_catalogue, permission, mode,
# attach, a headless-corpus scenario for claude_sdk, or all), under the
# operator's existing login. The acp kind takes an agent and a scenario
# instead (`just live acp grok cancel`, `just live acp all all`; lists may
# be comma-separated), drives the agent with a scripted ACP client through
# acp-probe, and joins each run to crates/interpret/fixtures/acp/live.
# With no arguments it reports not_run for every kind and starts nothing.
# Its tests are programs of their own, not libtest's, so each runs here as
# it is, with the operator's HOME, after Buck2 builds it and the binaries
# it starts.
live *ARGS:
    set -e; if [ "${1-}" = -- ]; then shift; fi; if [ $# -eq 0 ]; then for kind in claude_pty claude_sdk codex acp; do echo "live $kind: not_run (no scenario selected)"; done; exit 0; fi; kind=$1; shift; case "$kind" in claude_pty|claude_sdk|codex|acp) ;; *) echo "live: unknown kind $kind; known: claude_pty, claude_sdk, codex, acp" >&2; exit 2 ;; esac; {{bounded}} 3600 {{buck2}} run "//crates/qualification:${kind}_live" -- "$@"

# Waves of Sonnet testers, each on a test network of its own with real
# Claude and Codex agents, keeping evidence of what they find (docs/QA.md).
# By hand only, never in CI: `just qa --testers 2 --phone 1 --waves 1`.
# Run adversarial QA by hand: waves of testers on real agents (docs/QA.md).
qa *ARGS:
    if [ "${1-}" = -- ]; then shift; fi; {{bounded}} 86400 scripts/py -B scripts/qa.py "$@"

# Qualify desktop performance on an enrolled machine. Pass --baseline to
# record the current release medians after every absolute budget passes.
perf *ARGS:
    set -e; if [ "${1-}" = -- ]; then shift; fi; {{bounded}} 2400 cargo build --locked --release -p qualification --bin perf --features perf; {{bounded}} 2400 cargo build --locked --release -p amux -p provider-fakes --bins; {{bounded}} 1800 target/release/perf "$@"

# Records the release flood under an Instruments template on this Mac, for
# reading what the daemon's core thread spends its time on and waits for
# (docs/PERFORMANCE.md): `just perf-profile 'System Trace' 15s` stops the run
# 15 s in (the agents it leaves ignore SIGTERM and are killed), `just perf-profile 'Time
# Profiler'` records it whole. Trace and report land in target/perf-profile/.
perf-profile TEMPLATE LIMIT="300s":
    set -e; {{bounded}} 2400 cargo build --locked --release -p qualification --bin perf --features perf; {{bounded}} 2400 cargo build --locked --release -p amux -p provider-fakes --bins; mkdir -p target/perf-profile; out="target/perf-profile/$(date +%Y%m%d-%H%M%S)"; window=; case "{{TEMPLATE}}" in "System Trace") window="--window 300s" ;; esac; {{bounded}} 1800 xcrun xctrace record --template "{{TEMPLATE}}" $window --no-prompt --time-limit "{{LIMIT}}" --env TESTNET_BINARIES="$PWD/target/release" --target-stdout "$out.log" --output "$out.trace" --launch -- "$PWD/target/release/perf" --only flood || echo "xctrace exited $? (a time limit ends the run this way)"; pkill -9 -f "$PWD/target/release/amux agent" || true; pkill -9 -f "$PWD/target/release/fake-" || true; pkill -9 -f "$PWD/target/release/perf --cluster-warmer" || true; test -d "$out.trace"; echo "$out.trace"

# Records the loopback scaling curve of the production relay at 10, 100 and
# 1,000 connections (aggregate throughput, stream-open latency p50 and p99);
# --baseline records, --compare judges within noise (docs/PERFORMANCE.md).
relay-curve *ARGS:
    set -e; if [ "${1-}" = -- ]; then shift; fi; {{bounded}} 2400 cargo build --locked --release -p qualification --bin relay-curve --features perf; {{bounded}} 1200 target/release/relay-curve "$@"
